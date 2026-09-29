//! Command plugins (`runtime = "command"`): each tool is a command line, so any CLI
//! script can be offered to the LLM without writing a plugin protocol (design §9.9).
//!
//! Arguments are declared in the manifest and checked before anything runs; values are
//! placed into the command line as separate arguments, never through a shell, and may
//! not start with `-`, so the LLM cannot slip in options such as `--out /etc/passwd`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use clankjob_core::llm::ToolSpec;
use clankjob_core::tool::{PluginTool, ToolOutput};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::process::PASSED_ENV;

/// Default time a tool may run.
const DEFAULT_TIMEOUT: Duration = Duration::from_mins(2);
/// Most output read from a tool; the rest is cut off.
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Output up to this many characters is returned inline by `output = "auto"`.
const INLINE_CHARS: usize = 12_000;
/// Longest text returned inline by `output = "text"`.
const MAX_INLINE_CHARS: usize = 20_000;
/// Characters of a stored file shown as a preview.
const PREVIEW_CHARS: usize = 600;
/// Longest accepted argument value.
const MAX_ARG_CHARS: usize = 2_000;

/// How a tool's output reaches the LLM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    /// Inline when short, as a case file when long.
    #[default]
    Auto,
    /// Always inline (cut off when very long).
    Text,
    /// Always as a case file, read with `read_file`.
    File,
}

/// Type of a declared argument.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgType {
    /// Text.
    #[default]
    String,
    /// A whole number.
    Integer,
    /// Any number.
    Number,
    /// `true` or `false`; with `options`, adds the option's arguments when true.
    Boolean,
}

/// A declared argument: `[tools.args.<name>]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgManifest {
    /// Its type.
    #[serde(default, rename = "type")]
    pub kind: ArgType,
    /// What it means, shown to the LLM.
    pub description: String,
    /// Whether the LLM must give it.
    #[serde(default)]
    pub required: bool,
    /// Allowed values, if limited.
    #[serde(default, rename = "enum")]
    pub choices: Vec<String>,
}

/// A tool: `[[tools]]` in `plugin.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolManifest {
    /// Name the LLM calls, e.g. `youtube_transcript`; `^[a-zA-Z0-9_-]{1,64}$`.
    pub name: String,
    /// What it does and when to use it, shown to the LLM.
    pub description: String,
    /// Program and arguments. `{name}` in an element is replaced by that argument, which
    /// must then be required. A program path with a `/` is relative to the plugin.
    pub command: Vec<String>,
    /// Arguments added only when an optional argument is given, e.g.
    /// `lang = ["--lang", "{lang}"]`.
    #[serde(default)]
    pub options: BTreeMap<String, Vec<String>>,
    /// Declared arguments.
    #[serde(default)]
    pub args: BTreeMap<String, ArgManifest>,
    /// How the output reaches the LLM.
    #[serde(default)]
    pub output: OutputMode,
    /// Name of the case file for long output; `{name}` placeholders allowed.
    #[serde(default)]
    pub file_name: Option<String>,
    /// How long it may run, e.g. `"2m"`.
    #[serde(default)]
    pub timeout: Option<String>,
}

/// A value made safe for a file name: no URL scheme, only letters, digits, `.`, `-` and
/// `_`, runs of `_` collapsed, at most 60 characters (the end, which is usually the id).
fn file_name_part(value: &str) -> String {
    let value = value.split_once("://").map_or(value, |(_, rest)| rest);
    let mut cleaned = String::with_capacity(value.len());
    for c in value.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
            c
        } else {
            '_'
        };
        if !(c == '_' && cleaned.ends_with('_')) {
            cleaned.push(c);
        }
    }
    let cleaned = cleaned.trim_matches(|c| c == '_' || c == '.');
    let skip = cleaned.chars().count().saturating_sub(60);
    cleaned.chars().skip(skip).collect()
}

/// A command tool, ready to run.
pub struct CommandTool {
    plugin: String,
    dir: PathBuf,
    spec: ToolSpec,
    manifest: ToolManifest,
    program: PathBuf,
    env: BTreeMap<String, String>,
    timeout: Duration,
}

/// Placeholders (`{name}`) in a template string.
fn placeholders(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = rest.get(start.saturating_add(1)..).unwrap_or_default();
        let Some(end) = after.find('}') else { break };
        found.push(after.get(..end).unwrap_or_default());
        rest = after.get(end.saturating_add(1)..).unwrap_or_default();
    }
    found
}

fn fill(template: &str, values: &BTreeMap<String, String>) -> String {
    values.iter().fold(template.to_owned(), |text, (name, value)| {
        text.replace(&format!("{{{name}}}"), value)
    })
}

/// Find a program: relative to the plugin when the path has a `/`, else on `PATH`.
fn locate(program: &str, dir: &Path) -> Result<PathBuf, String> {
    if program.contains('/') {
        if program.starts_with('/') || program.contains("..") {
            return Err(format!(
                "program `{program}` must be a path inside the plugin directory"
            ));
        }
        // Absolute, because the command runs with the plugin directory as its working
        // directory, where a relative `plugins_dir` would no longer resolve.
        let path = dir.join(program);
        return if path.is_file() {
            std::fs::canonicalize(&path).map_err(|error| format!("program `{program}`: {error}"))
        } else {
            Err(format!("program `{program}` not found in the plugin directory"))
        };
    }
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join(program))
        .find(|path| path.is_file())
        .ok_or_else(|| format!("program `{program}` not found on PATH"))
}

/// Check that a program is on `PATH`, e.g. one named in `requires`.
///
/// # Errors
///
/// Returns why it was not found.
pub fn require(program: &str) -> Result<(), String> {
    locate(program, Path::new(".")).map(drop)
}

impl CommandTool {
    /// Check a tool's manifest and build it.
    ///
    /// # Arguments
    ///
    /// * `plugin` - Plugin id
    /// * `dir` - Plugin directory, the working directory of the command
    /// * `manifest` - The `[[tools]]` entry
    /// * `env` - Extra environment variables, already resolved (`[env]` in `config.toml`)
    ///
    /// # Errors
    ///
    /// Returns what is wrong with the manifest, or that the program cannot be found.
    pub fn new(
        plugin: &str,
        dir: &Path,
        manifest: ToolManifest,
        env: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let name = &manifest.name;
        let valid_name = !name.is_empty()
            && name.len() <= 64
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid_name {
            return Err(format!("tool name `{name}` must be 1-64 letters, digits, `_` or `-`"));
        }
        let Some(program) = manifest.command.first() else {
            return Err(format!("tool `{name}`: `command` is empty"));
        };
        if !placeholders(program).is_empty() {
            return Err(format!("tool `{name}`: the program cannot be an argument"));
        }
        for used in manifest.command.iter().flat_map(|part| placeholders(part)) {
            match manifest.args.get(used) {
                Some(arg) if arg.required => {}
                Some(_) => {
                    return Err(format!(
                        "tool `{name}`: `{{{used}}}` in `command` must be a required argument; put optional ones in `options`"
                    ));
                }
                None => return Err(format!("tool `{name}`: `{{{used}}}` is not a declared argument")),
            }
        }
        for (option, parts) in &manifest.options {
            if !manifest.args.contains_key(option) {
                return Err(format!("tool `{name}`: option `{option}` is not a declared argument"));
            }
            if let Some(other) = parts.iter().flat_map(|part| placeholders(part)).find(|used| used != option) {
                return Err(format!(
                    "tool `{name}`: option `{option}` can only use `{{{option}}}`, not `{{{other}}}`"
                ));
            }
        }
        let timeout = match &manifest.timeout {
            None => DEFAULT_TIMEOUT,
            Some(text) => {
                humantime::parse_duration(text).map_err(|error| format!("tool `{name}`: timeout: {error}"))?
            }
        };
        let program = locate(program, dir).map_err(|error| format!("tool `{name}`: {error}"))?;
        let mut properties = Map::new();
        let mut required = Vec::new();
        for (arg, declared) in &manifest.args {
            let kind = match declared.kind {
                ArgType::String => "string",
                ArgType::Integer => "integer",
                ArgType::Number => "number",
                ArgType::Boolean => "boolean",
            };
            let mut schema = json!({ "type": kind, "description": declared.description });
            if !declared.choices.is_empty()
                && let Some(object) = schema.as_object_mut()
            {
                object.insert("enum".to_owned(), json!(declared.choices));
            }
            properties.insert(arg.clone(), schema);
            if declared.required {
                required.push(arg.clone());
            }
        }
        let spec = ToolSpec {
            name: name.clone(),
            description: manifest.description.clone(),
            parameters: json!({ "type": "object", "properties": properties, "required": required }),
        };
        Ok(Self {
            plugin: plugin.to_owned(),
            dir: dir.to_path_buf(),
            spec,
            manifest,
            program,
            env,
            timeout,
        })
    }

    /// Check the LLM's arguments and turn them into text values.
    fn values(&self, arguments: &Value) -> Result<BTreeMap<String, String>, String> {
        let empty = Map::new();
        let given = match arguments {
            Value::Object(object) => object,
            Value::Null => &empty,
            _ => return Err("arguments must be an object".to_owned()),
        };
        if let Some(unknown) = given.keys().find(|key| !self.manifest.args.contains_key(*key)) {
            let known: Vec<&str> = self.manifest.args.keys().map(String::as_str).collect();
            return Err(format!(
                "unknown argument `{unknown}`; this tool takes: {}",
                known.join(", ")
            ));
        }
        let mut values = BTreeMap::new();
        for (name, declared) in &self.manifest.args {
            let value = match given.get(name) {
                None | Some(Value::Null) if declared.required => return Err(format!("`{name}` is required")),
                None | Some(Value::Null) => continue,
                Some(value) => value,
            };
            let text = match (declared.kind, value) {
                (ArgType::String, Value::String(text)) => text.trim().to_owned(),
                (ArgType::Integer, Value::Number(number)) if number.is_i64() || number.is_u64() => number.to_string(),
                (ArgType::Number, Value::Number(number)) => number.to_string(),
                (ArgType::Boolean, Value::Bool(flag)) => flag.to_string(),
                (kind, _) => return Err(format!("`{name}` must be a {kind:?}").to_lowercase()),
            };
            if text.is_empty() {
                return Err(format!("`{name}` is empty"));
            }
            if text.starts_with('-') && declared.kind == ArgType::String {
                return Err(format!("`{name}` cannot start with `-`"));
            }
            if text.contains('\0') || text.chars().count() > MAX_ARG_CHARS {
                return Err(format!("`{name}` is too long or contains a NUL byte"));
            }
            if !declared.choices.is_empty() && !declared.choices.contains(&text) {
                return Err(format!("`{name}` must be one of: {}", declared.choices.join(", ")));
            }
            if declared.kind == ArgType::Boolean && text == "false" {
                continue;
            }
            values.insert(name.clone(), text);
        }
        Ok(values)
    }

    /// The argument list after the program.
    fn argv(&self, values: &BTreeMap<String, String>) -> Vec<String> {
        let mut argv: Vec<String> = self.manifest.command.iter().skip(1).map(|part| fill(part, values)).collect();
        for (option, parts) in &self.manifest.options {
            if values.contains_key(option) {
                argv.extend(parts.iter().map(|part| fill(part, values)));
            }
        }
        argv
    }

    /// Run the command and wait for it, within the timeout.
    fn execute(&self, argv: &[String]) -> Result<String, String> {
        let name = &self.spec.name;
        let mut command = Command::new(&self.program);
        command
            .args(argv)
            .current_dir(&self.dir)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for variable in PASSED_ENV {
            if let Some(value) = std::env::var_os(variable) {
                command.env(variable, value);
            }
        }
        command.envs(&self.env);
        let mut child = command.spawn().map_err(|error| format!("`{name}` could not start: {error}"))?;
        let read_all = |mut source: Box<dyn Read + Send>| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                // Read to the end even past the cap, so the child never blocks on a full pipe.
                let mut chunk = [0_u8; 8192];
                while let Ok(count) = source.read(&mut chunk) {
                    if count == 0 {
                        break;
                    }
                    let room = MAX_OUTPUT_BYTES.saturating_sub(bytes.len());
                    bytes.extend_from_slice(chunk.get(..count.min(room)).unwrap_or_default());
                }
                bytes
            })
        };
        let stdout = child.stdout.take().map(|pipe| read_all(Box::new(pipe)));
        let stderr = child.stderr.take().map(|pipe| read_all(Box::new(pipe)));
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() >= self.timeout => {
                    // Already exited is fine; it is reaped by `wait` either way.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "`{name}` timed out after {}",
                        humantime::format_duration(self.timeout)
                    ));
                }
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(error) => return Err(format!("`{name}`: {error}")),
            }
        };
        let collect = |handle: Option<thread::JoinHandle<Vec<u8>>>| {
            handle
                .and_then(|handle| handle.join().ok())
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default()
        };
        let (stdout, stderr) = (collect(stdout), collect(stderr));
        if status.success() {
            return Ok(stdout);
        }
        let detail = stderr.trim();
        let tail: String = detail.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
        Err(format!(
            "`{name}` failed ({status}){}",
            if tail.is_empty() {
                String::new()
            } else {
                format!(": {tail}")
            }
        ))
    }

    /// The case file name for long output.
    fn file_name(&self, values: &BTreeMap<String, String>) -> String {
        let template = self
            .manifest
            .file_name
            .clone()
            .unwrap_or_else(|| format!("{}.txt", self.spec.name));
        let safe: BTreeMap<String, String> = values
            .iter()
            .map(|(name, value)| (name.clone(), file_name_part(value)))
            .collect();
        fill(&template, &safe)
    }
}

impl PluginTool for CommandTool {
    fn plugin(&self) -> &str {
        &self.plugin
    }

    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn run(&self, arguments: &Value) -> Result<ToolOutput, String> {
        let values = self.values(arguments)?;
        let output = self.execute(&self.argv(&values))?;
        let chars = output.chars().count();
        let inline = match self.manifest.output {
            OutputMode::Text => true,
            OutputMode::File => false,
            OutputMode::Auto => chars <= INLINE_CHARS,
        };
        if inline {
            let text: String = output.chars().take(MAX_INLINE_CHARS).collect();
            let cut = chars > MAX_INLINE_CHARS;
            return Ok(ToolOutput::Json(json!({ "output": text, "truncated": cut })));
        }
        if output.trim().is_empty() {
            return Err(format!("`{}` produced no output", self.spec.name));
        }
        let preview: String = output.chars().take(PREVIEW_CHARS).collect();
        Ok(ToolOutput::File {
            name: self.file_name(&values),
            content: output.into_bytes(),
            summary: json!({ "preview": preview }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "#!/bin/sh\n\
        if [ \"$1\" = fail ]; then echo 'Error: NoTranscriptFound' >&2; exit 1; fi\n\
        if [ \"$1\" = slow ]; then sleep 5; fi\n\
        if [ \"$1\" = long ]; then i=0; while [ $i -lt 3000 ]; do echo \"line $i of a long transcript\"; i=$((i+1)); done; exit 0; fi\n\
        echo \"args: $*\"; echo \"secret: ${TOOL_SECRET:-none} leak: ${CLANKJOB_TOKEN:-none}\"\n";

    fn manifest(toml_text: &str) -> ToolManifest {
        toml::from_str(toml_text).unwrap()
    }

    fn tool(extra: &str) -> (tempfile::TempDir, CommandTool) {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("tool.sh");
        std::fs::write(&script, SCRIPT).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let text = format!(
            "name = \"transcript\"\ndescription = \"Get a transcript.\"\n\
             command = [\"./tool.sh\", \"{{video}}\", \"--format\", \"stamped\"]\n\
             file_name = \"yt-{{video}}.md\"\n{extra}\n\
             [options]\nlang = [\"--lang\", \"{{lang}}\"]\n\
             [args.video]\ndescription = \"URL or id\"\nrequired = true\n\
             [args.lang]\ndescription = \"Language\"\n"
        );
        let env = BTreeMap::from([("TOOL_SECRET".to_owned(), "s3".to_owned())]);
        let built = CommandTool::new("youtube", dir.path(), manifest(&text), env).unwrap();
        (dir, built)
    }

    #[test]
    fn arguments_become_separate_command_line_arguments() {
        // Arrange
        let (_dir, tool) = tool("");

        // Act
        let short = tool.run(&json!({ "video": "abc def", "lang": "de" })).unwrap();

        // Assert
        let ToolOutput::Json(value) = short else {
            unreachable!("expected inline output")
        };
        let output = value["output"].as_str().unwrap();
        assert!(output.contains("args: abc def --format stamped --lang de"));
        assert!(
            output.contains("secret: s3 leak: none"),
            "only declared env reaches the tool"
        );
        assert_eq!(tool.spec().parameters["required"], json!(["video"]));
    }

    #[test]
    fn bad_arguments_are_refused_before_anything_runs() {
        let (_dir, tool) = tool("");

        let errors: Vec<String> = [
            json!({}),
            json!({ "video": "--out=/etc/passwd" }),
            json!({ "video": 3 }),
            json!({ "video": "x", "shell": "rm" }),
        ]
        .iter()
        .map(|arguments| tool.run(arguments).unwrap_err())
        .collect();

        assert_eq!(errors[0], "`video` is required");
        assert_eq!(errors[1], "`video` cannot start with `-`");
        assert_eq!(errors[2], "`video` must be a string");
        assert!(errors[3].starts_with("unknown argument `shell`"));
    }

    #[test]
    fn long_output_becomes_a_file_and_failures_carry_stderr() {
        // Arrange
        let (_dir, tool) = tool("timeout = \"1s\"");

        // Act
        let long = tool.run(&json!({ "video": "long" })).unwrap();
        let failed = tool.run(&json!({ "video": "fail" })).unwrap_err();
        let slow = tool.run(&json!({ "video": "slow" })).unwrap_err();

        // Assert
        let ToolOutput::File { name, content, summary } = long else {
            unreachable!("expected a file")
        };
        assert_eq!(name, "yt-long.md");
        assert!(content.len() > INLINE_CHARS);
        assert!(summary["preview"].as_str().unwrap().starts_with("line 0"));
        assert!(failed.contains("NoTranscriptFound"));
        assert!(slow.contains("timed out after 1s"));
    }

    #[test]
    fn tools_run_when_the_plugins_directory_is_relative() {
        // Arrange: a directory given relative to the current one, like `plugins_dir = "./plugin"`.
        let dir = tempfile::tempdir_in(".").unwrap();
        let relative = PathBuf::from(".").join(dir.path().file_name().unwrap());
        std::fs::write(relative.join("hello.sh"), "#!/bin/sh\necho hello\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(relative.join("hello.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let text = "name = \"hello\"\ndescription = \"d\"\ncommand = [\"./hello.sh\"]";

        // Act
        let tool = CommandTool::new("p", &relative, manifest(text), BTreeMap::new()).unwrap();

        // Assert
        assert_eq!(
            tool.run(&json!({})).unwrap(),
            ToolOutput::Json(json!({ "output": "hello\n", "truncated": false }))
        );
    }

    #[test]
    fn values_become_readable_file_names() {
        assert_eq!(
            file_name_part("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            "www.youtube.com_watch_v_dQw4w9WgXcQ"
        );
        assert_eq!(file_name_part("../../etc/passwd"), "etc_passwd");
        assert_eq!(file_name_part(&"x".repeat(100)).len(), 60);
    }

    #[test]
    fn manifests_with_unsafe_or_inconsistent_commands_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let build = |text: &str| CommandTool::new("p", dir.path(), manifest(text), BTreeMap::new()).err();
        let base = "name = \"t\"\ndescription = \"d\"\n";

        assert!(
            build(&format!(
                "{base}command = [\"{{x}}\"]\n[args.x]\ndescription = \"x\"\nrequired = true"
            ))
            .unwrap()
            .contains("program cannot be an argument")
        );
        assert!(
            build(&format!(
                "{base}command = [\"sh\", \"{{x}}\"]\n[args.x]\ndescription = \"x\""
            ))
            .unwrap()
            .contains("must be a required argument")
        );
        assert!(
            build(&format!("{base}command = [\"../evil.sh\"]"))
                .unwrap()
                .contains("inside the plugin directory")
        );
        assert!(
            build(&format!("{base}command = [\"no-such-program-xyz\"]"))
                .unwrap()
                .contains("not found on PATH")
        );
        assert!(
            build("name = \"bad name\"\ndescription = \"d\"\ncommand = [\"sh\"]")
                .unwrap()
                .contains("must be 1-64")
        );
    }
}
