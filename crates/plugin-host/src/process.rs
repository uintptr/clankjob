//! External plugins: child processes spoken to in JSON-RPC 2.0 over stdin/stdout, one
//! message per line (design §9.8).
//!
//! One process serves every instance of its plugin, because each call carries the
//! instance's configuration. Calls are serialized. A process that times out, exits or
//! writes garbage is killed and started again on the next call.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use clankjob_core::channel::ChannelError;
use serde_json::{Value, json};

/// Plugin protocol version this host speaks.
pub const PROTOCOL: u64 = 1;

/// How long the `initialize` handshake may take.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);

/// Environment variables passed through to plugin processes. Everything else, server
/// secrets included, is left out.
const PASSED_ENV: &[&str] = &["PATH", "HOME", "TZ", "LANG", "LC_ALL", "SYSTEMROOT"];

/// A running plugin process. Dropping it kills the process.
struct Running {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Drop for Running {
    fn drop(&mut self) {
        // Already exited is fine; there is nothing more to do either way.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Why an exchange failed.
enum Failure {
    /// The plugin answered with an error; the process is fine.
    Rpc(ChannelError),
    /// The process is unusable (exited, timed out, bad output) and must be restarted.
    Broken(String),
}

/// An external plugin process, started on first use.
pub struct ProcessPlugin {
    id: String,
    dir: PathBuf,
    command: Vec<String>,
    running: Mutex<Option<Running>>,
    next_id: AtomicU64,
    /// Set by [`ProcessPlugin::shutdown`]: the plugin was reloaded or the server is
    /// stopping, so the process must not be started again.
    retired: AtomicBool,
}

impl ProcessPlugin {
    /// Describe a plugin process. Nothing starts until the first call.
    ///
    /// # Arguments
    ///
    /// * `id` - Plugin id, for logs
    /// * `dir` - The plugin's directory, used as working directory
    /// * `command` - Program and arguments, e.g. `["python3", "discord_plugin.py"]`
    #[must_use]
    pub fn new(id: &str, dir: &Path, command: Vec<String>) -> Self {
        Self {
            id: id.to_owned(),
            dir: dir.to_path_buf(),
            command,
            running: Mutex::new(None),
            next_id: AtomicU64::new(1),
            retired: AtomicBool::new(false),
        }
    }

    /// The plugin id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Call a method and wait for its result.
    ///
    /// # Errors
    ///
    /// Returns the plugin's error, or a retryable error if the process could not be
    /// started, exited, or did not answer within `timeout`.
    pub fn call(&self, method: &str, params: &Value, timeout: Duration) -> Result<Value, ChannelError> {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if self.retired.load(Ordering::SeqCst) {
            return Err(ChannelError::retryable(format!("plugin `{}` was reloaded", self.id)));
        }
        if running.is_none() {
            *running = Some(self.start()?);
        }
        let Some(process) = running.as_mut() else {
            return Err(ChannelError::retryable(format!("plugin `{}` is not running", self.id)));
        };
        match self.exchange(process, method, params, timeout) {
            Ok(result) => Ok(result),
            Err(Failure::Rpc(error)) => Err(error),
            Err(Failure::Broken(message)) => {
                tracing::warn!(plugin = %self.id, method, %message, "plugin process restarted");
                // Dropping the process kills it; the next call starts a fresh one.
                *running = None;
                Err(ChannelError::retryable(format!("plugin `{}`: {message}", self.id)))
            }
        }
    }

    /// Ask the process to exit, and kill it if it does not. Later calls fail instead of
    /// starting it again.
    pub fn shutdown(&self) {
        self.retired.store(true, Ordering::SeqCst);
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(process) = running.as_mut() {
            // Errors only mean the process is already gone.
            let _ = self.exchange(process, "shutdown", &json!({}), Duration::from_secs(2));
        }
        *running = None;
    }

    fn start(&self) -> Result<Running, ChannelError> {
        let Some((program, args)) = self.command.split_first() else {
            return Err(ChannelError::fatal(format!("plugin `{}` has no command", self.id)));
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&self.dir)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in PASSED_ENV {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = command.spawn().map_err(|error| {
            ChannelError::retryable(format!("cannot start plugin `{}` ({program}): {error}", self.id))
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err(ChannelError::retryable(format!("plugin `{}`: no stdio pipes", self.id)));
        };
        let (sender, lines) = mpsc::channel();
        let name = format!("plugin-{}", self.id);
        thread::Builder::new()
            .name(name.clone())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| ChannelError::retryable(format!("plugin `{}`: {error}", self.id)))?;
        let id = self.id.clone();
        thread::Builder::new()
            .name(format!("{name}-stderr"))
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
                    tracing::info!(plugin = %id, "{line}");
                }
            })
            .map_err(|error| ChannelError::retryable(format!("plugin `{}`: {error}", self.id)))?;
        let mut process = Running { child, stdin, lines };
        let params = json!({ "protocol": PROTOCOL, "plugin_id": self.id });
        let reply = match self.exchange(&mut process, "initialize", &params, INITIALIZE_TIMEOUT) {
            Ok(reply) => reply,
            Err(Failure::Rpc(error)) => return Err(ChannelError::fatal(format!("plugin `{}`: {error}", self.id))),
            Err(Failure::Broken(message)) => {
                return Err(ChannelError::retryable(format!("plugin `{}`: {message}", self.id)));
            }
        };
        if reply.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL) {
            return Err(ChannelError::fatal(format!(
                "plugin `{}` speaks protocol {}, the server speaks {PROTOCOL}",
                self.id,
                reply.get("protocol").unwrap_or(&Value::Null)
            )));
        }
        tracing::info!(plugin = %self.id, "plugin process started");
        Ok(process)
    }

    /// Send one request and wait for the response with the same id.
    fn exchange(
        &self,
        process: &mut Running,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, Failure> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut line = request.to_string();
        line.push('\n');
        process
            .stdin
            .write_all(line.as_bytes())
            .and_then(|()| process.stdin.flush())
            .map_err(|error| Failure::Broken(format!("cannot write to the process: {error}")))?;
        let deadline = Instant::now().checked_add(timeout).unwrap_or_else(Instant::now);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = match process.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(Failure::Broken(format!("`{method}` timed out after {timeout:?}")));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(Failure::Broken("the process exited".to_owned())),
            };
            if line.trim().is_empty() {
                continue;
            }
            let response: Value = serde_json::from_str(&line)
                .map_err(|error| Failure::Broken(format!("invalid JSON on stdout: {error}")))?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                // A late answer to a call that already timed out.
                continue;
            }
            if let Some(error) = response.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_owned();
                let retryable = error.pointer("/data/retryable").and_then(Value::as_bool).unwrap_or(false);
                return Err(Failure::Rpc(ChannelError { message, retryable }));
            }
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plugin that echoes params, errors on `fail`, hangs on `hang` and exits on `die`.
    const SCRIPT: &str = r#"
import json, sys, time
for line in sys.stdin:
    request = json.loads(line)
    method, rid = request["method"], request["id"]
    if method == "initialize":
        result = {"protocol": 1}
    elif method == "fail":
        print(json.dumps({"jsonrpc": "2.0", "id": rid, "error": {"code": -32000, "message": "nope", "data": {"retryable": True}}}), flush=True)
        continue
    elif method == "hang":
        time.sleep(5)
        continue
    elif method == "die":
        sys.exit(1)
    else:
        result = {"echo": request["params"], "env": sorted(k for k in __import__("os").environ if k.startswith("CLANK"))}
    print(json.dumps({"jsonrpc": "2.0", "id": rid, "result": result}), flush=True)
"#;

    fn plugin() -> (tempfile::TempDir, ProcessPlugin) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plugin.py"), SCRIPT).unwrap();
        let plugin = ProcessPlugin::new("fake", dir.path(), vec!["python3".to_owned(), "plugin.py".to_owned()]);
        (dir, plugin)
    }

    #[test]
    fn calls_return_results_and_plugin_errors() {
        // Arrange
        let (_dir, plugin) = plugin();

        // Act
        let echo = plugin.call("echo", &json!({ "a": 1 }), Duration::from_secs(5)).unwrap();
        let error = plugin.call("fail", &json!({}), Duration::from_secs(5)).unwrap_err();

        // Assert
        assert_eq!(echo["echo"], json!({ "a": 1 }));
        assert_eq!(echo["env"], json!([]), "server environment must not leak");
        assert_eq!(error, ChannelError::retryable("nope"));
        plugin.shutdown();
    }

    #[test]
    fn a_hung_or_dead_process_is_restarted_on_the_next_call() {
        // Arrange
        let (_dir, plugin) = plugin();

        // Act
        let hung = plugin.call("hang", &json!({}), Duration::from_millis(200)).unwrap_err();
        let after_hang = plugin.call("echo", &json!(1), Duration::from_secs(5));
        let died = plugin.call("die", &json!({}), Duration::from_secs(5)).unwrap_err();
        let after_death = plugin.call("echo", &json!(2), Duration::from_secs(5));

        // Assert
        assert!(hung.retryable && hung.message.contains("timed out"));
        assert_eq!(after_hang.unwrap()["echo"], 1);
        assert!(died.retryable && died.message.contains("exited"));
        assert_eq!(after_death.unwrap()["echo"], 2);
    }
}
