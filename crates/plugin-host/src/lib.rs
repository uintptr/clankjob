//! The plugin host (design §9): loads plugins from the plugins directory and runs
//! external ones as child processes.
//!
//! Each plugin lives in its own directory with a `plugin.toml` manifest and a
//! `config.toml` defining its instances. Only human-channel plugins (design §10.3) are
//! wired so far. A broken plugin or instance is reported and skipped; it never stops
//! the server. [`PluginRegistry`] remembers what was found, so the API can show it and
//! check an instance again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use clankjob_core::channel::{ChannelError, HumanChannel};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

mod channel;
pub mod process;

pub use channel::{DEFAULT_NOTIFY_ON, ProcessChannel};
use process::{PROTOCOL, ProcessPlugin};

/// Poll interval when an instance sets none.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(20);
/// Shortest poll interval accepted, to stay well inside the service's rate limits.
const MIN_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How a plugin runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// Compiled into the server.
    Builtin,
    /// A Python script run with `python3`.
    Python,
    /// Any executable.
    Exec,
}

/// `plugin.toml`. Fields the host does not use yet (tools, wait conditions) are ignored.
#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    runtime: Runtime,
    #[serde(default)]
    protocol: Option<u64>,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default)]
    human_channel: Option<toml::Table>,
}

/// A plugin's `config.toml`.
#[derive(Deserialize)]
struct PluginConfig {
    #[serde(default)]
    instances: BTreeMap<String, toml::Table>,
}

/// Whether an instance is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceState {
    /// Loaded and in use; see its problems and warnings.
    On,
    /// Turned off in its config (`enabled = false`).
    Off,
    /// Its configuration is invalid, so it did not load.
    Error,
}

/// What is known about one plugin instance.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstanceStatus {
    /// Instance name, e.g. `discord_joe`.
    pub name: String,
    /// Whether it runs.
    pub state: InstanceState,
    /// Why it did not load, or why its last check could not run.
    pub error: Option<String>,
    /// What the last check reported, e.g. the bot and channel names.
    pub report: Map<String, Value>,
    /// Things that stop it from working, each with its fix.
    pub problems: Vec<String>,
    /// Things that make it work less well.
    pub warnings: Vec<String>,
    /// Every check the plugin ran: `{ ok, required, what, fix }`.
    pub findings: Vec<Value>,
    /// When it was last checked.
    pub checked_at: Option<DateTime<Utc>>,
}

impl InstanceStatus {
    fn new(name: &str, state: InstanceState, error: Option<String>) -> Self {
        Self {
            name: name.to_owned(),
            state,
            error,
            report: Map::new(),
            problems: Vec::new(),
            warnings: Vec::new(),
            findings: Vec::new(),
            checked_at: None,
        }
    }

    /// Record the result of a check.
    fn checked(&mut self, result: Result<Value, ChannelError>) {
        self.checked_at = Some(Utc::now());
        self.report.clear();
        self.problems.clear();
        self.warnings.clear();
        self.findings.clear();
        match result {
            Ok(Value::Object(report)) => {
                self.error = None;
                for (key, value) in report {
                    match (key.as_str(), value) {
                        ("problems", Value::Array(items)) => self.problems = strings(items),
                        ("warnings", Value::Array(items)) => self.warnings = strings(items),
                        ("findings", Value::Array(items)) => self.findings = items,
                        (_, value) => {
                            self.report.insert(key, value);
                        }
                    }
                }
            }
            Ok(_) => self.error = None,
            Err(error) => self.error = Some(format!("check failed: {error}")),
        }
    }

    /// Log the result of a check.
    fn log(&self, plugin: &str) {
        let name = &self.name;
        for problem in &self.problems {
            tracing::error!(instance = %name, "{problem}");
        }
        for warning in &self.warnings {
            tracing::warn!(instance = %name, "{warning}");
        }
        let report = Value::Object(self.report.clone());
        if let Some(error) = &self.error {
            tracing::error!(instance = %name, plugin, %error, "channel check failed; it stays loaded and is retried");
        } else if self.problems.is_empty() {
            tracing::info!(instance = %name, plugin, %report, "channel ready");
        } else {
            tracing::warn!(instance = %name, plugin, %report, problems = self.problems.len(), "channel loaded with problems; it may not work until they are fixed");
        }
    }
}

fn strings(items: Vec<Value>) -> Vec<String> {
    items
        .into_iter()
        .filter_map(|item| match item {
            Value::String(text) => Some(text),
            _ => None,
        })
        .collect()
}

/// What is known about one plugin.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PluginStatus {
    /// Directory name, which is also its id.
    pub id: String,
    /// Display name from the manifest.
    pub name: Option<String>,
    /// Version from the manifest.
    pub version: Option<String>,
    /// How it runs, if the manifest could be read.
    pub runtime: Option<Runtime>,
    /// What it provides, e.g. `human_channel`.
    pub provides: Vec<String>,
    /// Why the plugin as a whole did not load.
    pub error: Option<String>,
    /// Anything else worth knowing, e.g. that it has no instances.
    pub note: Option<String>,
    /// Its instances.
    pub instances: Vec<InstanceStatus>,
}

impl PluginStatus {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: None,
            version: None,
            runtime: None,
            provides: Vec::new(),
            error: None,
            note: None,
            instances: Vec::new(),
        }
    }
}

/// Plugins found in the plugins directory, their instances and how they are doing.
#[derive(Default)]
pub struct PluginRegistry {
    statuses: Mutex<Vec<PluginStatus>>,
    channels: BTreeMap<String, Arc<ProcessChannel>>,
    processes: Vec<Arc<ProcessPlugin>>,
    errors: Vec<String>,
}

impl PluginRegistry {
    /// The loaded human channel instances by name, for the engine.
    #[must_use]
    pub fn channels(&self) -> BTreeMap<String, Arc<dyn HumanChannel>> {
        self.channels
            .iter()
            .map(|(name, channel)| (name.clone(), Arc::clone(channel) as Arc<dyn HumanChannel>))
            .collect()
    }

    /// Every plugin found and the state of its instances.
    #[must_use]
    pub fn statuses(&self) -> Vec<PluginStatus> {
        self.statuses.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Problems found while loading (plugins or instances that did not load, failed
    /// checks, problems reported by checks); each was also logged.
    #[must_use]
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// The status of one instance.
    #[must_use]
    pub fn instance(&self, name: &str) -> Option<InstanceStatus> {
        self.statuses()
            .into_iter()
            .flat_map(|plugin| plugin.instances)
            .find(|instance| instance.name == name)
    }

    /// Check a loaded instance again (blocks until the plugin answers) and remember the
    /// result.
    ///
    /// # Returns
    ///
    /// The new status, or `None` if no instance of that name is loaded
    #[must_use]
    pub fn test(&self, name: &str) -> Option<InstanceStatus> {
        let channel = self.channels.get(name)?;
        let result = channel.validate();
        let mut statuses = self.statuses.lock().unwrap_or_else(PoisonError::into_inner);
        let status = statuses
            .iter_mut()
            .flat_map(|plugin| plugin.instances.iter_mut())
            .find(|instance| instance.name == name)?;
        status.checked(result);
        status.log(channel.plugin());
        Some(status.clone())
    }

    /// Ask every plugin process to exit.
    pub fn shutdown(&self) {
        for process in &self.processes {
            process.shutdown();
        }
    }

    fn error(&mut self, message: String) {
        tracing::error!("{message}");
        self.errors.push(message);
    }
}

/// Load every plugin in `plugins_dir`, start the processes of channel plugins and check
/// each instance's configuration.
///
/// # Arguments
///
/// * `plugins_dir` - Directory holding one sub-directory per plugin
/// * `secrets_dir` - Where `{ secret = "name" }` references are read from
#[must_use]
pub fn load(plugins_dir: &Path, secrets_dir: &Path) -> PluginRegistry {
    let mut registry = PluginRegistry::default();
    let mut statuses = Vec::new();
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(plugins_dir) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.join("plugin.toml").is_file())
            .collect(),
        Err(error) => {
            registry.error(format!(
                "cannot read plugins directory {}: {error}",
                plugins_dir.display()
            ));
            return registry;
        }
    };
    dirs.sort();
    for dir in dirs {
        let id = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut status = PluginStatus::new(&id);
        if let Err(message) = load_plugin(&dir, secrets_dir, &mut registry, &mut status) {
            registry.error(format!("plugin {id}: {message}"));
            status.error = Some(message);
        }
        statuses.push(status);
    }
    registry.statuses = Mutex::new(statuses);
    registry
}

fn read_toml<T>(path: &Path) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let text = std::fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    toml::from_str(&text).map_err(|error| format!("invalid {}: {error}", path.display()))
}

fn load_plugin(
    dir: &Path,
    secrets_dir: &Path,
    registry: &mut PluginRegistry,
    status: &mut PluginStatus,
) -> Result<(), String> {
    let manifest: Manifest = read_toml(&dir.join("plugin.toml"))?;
    status.name.clone_from(&manifest.name);
    status.version.clone_from(&manifest.version);
    status.runtime = Some(manifest.runtime);
    if manifest.human_channel.is_some() {
        status.provides.push("human_channel".to_owned());
    }
    if status.id != manifest.id {
        return Err(format!("id `{}` must match the directory name", manifest.id));
    }
    if manifest.protocol.is_some_and(|protocol| protocol != PROTOCOL) {
        return Err(format!(
            "protocol {:?} is not supported (the server speaks {PROTOCOL})",
            manifest.protocol
        ));
    }
    if manifest.human_channel.is_none() {
        tracing::info!(plugin = %manifest.id, "plugin skipped: only human channels are supported so far");
        status.note = Some("not loaded: only human channel plugins are supported so far".to_owned());
        return Ok(());
    }
    let entrypoint = manifest.entrypoint.as_deref().ok_or("`entrypoint` is required")?;
    if entrypoint.contains("..") || Path::new(entrypoint).is_absolute() {
        return Err("`entrypoint` must be a file inside the plugin directory".to_owned());
    }
    let command = match manifest.runtime {
        Runtime::Python => vec!["python3".to_owned(), entrypoint.to_owned()],
        Runtime::Exec => vec![dir.join(entrypoint).to_string_lossy().into_owned()],
        Runtime::Builtin => return Err("no built-in plugin with this id".to_owned()),
    };
    let config_path = dir.join("config.toml");
    if !config_path.is_file() {
        tracing::info!(plugin = %manifest.id, "no config.toml: no instances");
        status.note = Some("no instances: copy config.example.toml to config.toml".to_owned());
        return Ok(());
    }
    let config: PluginConfig = read_toml(&config_path)?;
    let process = Arc::new(ProcessPlugin::new(&manifest.id, dir, command));
    let mut used = false;
    for (name, table) in config.instances {
        if registry.channels.contains_key(&name) {
            let message = "defined twice; instance names must be unique across plugins".to_owned();
            registry.error(format!("instance `{name}`: {message}"));
            status
                .instances
                .push(InstanceStatus::new(&name, InstanceState::Error, Some(message)));
            continue;
        }
        match instance(&name, table, secrets_dir, &process) {
            Ok(None) => {
                tracing::info!(instance = %name, "instance disabled");
                status.instances.push(InstanceStatus::new(&name, InstanceState::Off, None));
            }
            Ok(Some(channel)) => {
                used = true;
                let mut instance_status = InstanceStatus::new(&name, InstanceState::On, None);
                instance_status.checked(channel.validate());
                instance_status.log(&manifest.id);
                for problem in instance_status.problems.iter().chain(&instance_status.error) {
                    registry.errors.push(format!("instance `{name}`: {problem}"));
                }
                status.instances.push(instance_status);
                registry.channels.insert(name, Arc::new(channel));
            }
            Err(message) => {
                registry.error(format!("instance `{name}`: {message}"));
                status
                    .instances
                    .push(InstanceStatus::new(&name, InstanceState::Error, Some(message)));
            }
        }
    }
    if used {
        registry.processes.push(process);
    }
    Ok(())
}

/// Build one instance from its config table, or `None` if it is disabled.
fn instance(
    name: &str,
    table: toml::Table,
    secrets_dir: &Path,
    process: &Arc<ProcessPlugin>,
) -> Result<Option<ProcessChannel>, String> {
    if table.get("enabled").and_then(toml::Value::as_bool) == Some(false) {
        return Ok(None);
    }
    let Value::Object(mut config) = resolve(toml::Value::Table(table), secrets_dir, "")? else {
        return Err("expected a table".to_owned());
    };
    config.remove("enabled");
    let allowed_responders = responders(&config)?;
    let poll_interval = match config.get("poll_interval") {
        None => DEFAULT_POLL_INTERVAL,
        Some(Value::String(text)) => {
            humantime::parse_duration(text).map_err(|error| format!("poll_interval: {error}"))?
        }
        Some(_) => return Err("poll_interval must be a duration such as \"20s\"".to_owned()),
    }
    .max(MIN_POLL_INTERVAL);
    let notify_on = match config.get("notify_on") {
        None => DEFAULT_NOTIFY_ON.iter().map(|event| (*event).to_owned()).collect(),
        Some(Value::Array(events)) => events
            .iter()
            .map(|event| {
                event
                    .as_str()
                    .filter(|event| DEFAULT_NOTIFY_ON.contains(event))
                    .map(str::to_owned)
                    .ok_or_else(|| format!("notify_on: unknown event {event}"))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("notify_on must be a list".to_owned()),
    };
    Ok(Some(ProcessChannel {
        instance: name.to_owned(),
        plugin: Arc::clone(process),
        config: Value::Object(config),
        allowed_responders,
        poll_interval,
        notify_on,
    }))
}

fn responders(config: &Map<String, Value>) -> Result<Vec<String>, String> {
    let Some(Value::Array(users)) = config.get("allowed_responders") else {
        return Err("allowed_responders must list at least one user id".to_owned());
    };
    let users: Vec<String> = users
        .iter()
        .map(|user| match user {
            Value::String(id) => Ok(id.clone()),
            Value::Number(id) => Ok(id.to_string()),
            _ => Err("allowed_responders entries must be user ids".to_owned()),
        })
        .collect::<Result<_, _>>()?;
    if users.is_empty() {
        return Err("allowed_responders must list at least one user id".to_owned());
    }
    Ok(users)
}

/// Convert a config value to JSON, replacing `{ secret = "name" }` and `{ env = "NAME" }`
/// tables by the secret's value. Error messages name the reference, never the value.
fn resolve(value: toml::Value, secrets_dir: &Path, path: &str) -> Result<Value, String> {
    let key = |name: &str| {
        if path.is_empty() {
            name.to_owned()
        } else {
            format!("{path}.{name}")
        }
    };
    Ok(match value {
        toml::Value::Table(table) => {
            if table.len() == 1
                && let Some((kind, toml::Value::String(reference))) = table.iter().next()
                && (kind == "secret" || kind == "env")
            {
                return read_secret(kind, reference, secrets_dir).map_err(|message| format!("{path}: {message}"));
            }
            let mut object = Map::new();
            for (name, value) in table {
                let resolved = resolve(value, secrets_dir, &key(&name))?;
                object.insert(name, resolved);
            }
            Value::Object(object)
        }
        toml::Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| resolve(item, secrets_dir, path))
                .collect::<Result<_, _>>()?,
        ),
        toml::Value::String(text) => Value::String(text),
        toml::Value::Integer(number) => Value::from(number),
        toml::Value::Float(number) => Value::from(number),
        toml::Value::Boolean(flag) => Value::Bool(flag),
        toml::Value::Datetime(time) => Value::String(time.to_string()),
    })
}

fn read_secret(kind: &str, reference: &str, secrets_dir: &Path) -> Result<Value, String> {
    let value = if kind == "env" {
        std::env::var(reference).map_err(|error| format!("env \"{reference}\": {error}"))?
    } else {
        if reference.is_empty() || reference.contains(['/', '\\']) || reference.starts_with('.') {
            return Err("invalid secret name (it should name a file, not hold the secret itself)".to_owned());
        }
        let path = secrets_dir.join(reference);
        std::fs::read_to_string(&path)
            .map_err(|error| format!("secret \"{reference}\": cannot read {}: {error}", path.display()))?
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{kind} \"{reference}\" is empty"));
    }
    Ok(Value::String(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const PLUGIN: &str = r#"
import json, sys
for line in sys.stdin:
    request = json.loads(line)
    method, params = request["method"], request["params"]
    if method == "initialize":
        result = {"protocol": 1}
    elif method == "validate_config":
        if params["config"]["channel_id"] == "bad":
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32000, "message": "401 Unauthorized"}}), flush=True)
            continue
        result = {"bot": "clankbot", "problems": ["Message Content intent: turn it on"], "warnings": ["no Manage Threads"]}
    elif method == "deliver":
        result = {"delivery": {"thread_id": "9", "token_seen": params["config"]["bot_token"] == "s3cret"}}
    elif method == "poll":
        result = {"replies": [{"request_id": params["open"][0]["request_id"], "external_id": "10", "responder": "42", "text": "yes"}], "cursor": {"9": "10"}}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#;

    fn plugin_dir(config: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("chat");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            "id = \"chat\"\nruntime = \"python\"\nprotocol = 1\nentrypoint = \"plugin.py\"\n[human_channel]\n",
        )
        .unwrap();
        std::fs::write(dir.join("plugin.py"), PLUGIN).unwrap();
        std::fs::write(dir.join("config.toml"), config).unwrap();
        std::fs::create_dir(root.path().join("secrets")).unwrap();
        std::fs::write(root.path().join("secrets/token"), "s3cret\n").unwrap();
        root
    }

    #[test]
    fn channel_instances_are_loaded_with_secrets_and_serve_calls() {
        // Arrange
        let root = plugin_dir(
            "[instances.chat_joe]\nbot_token = { secret = \"token\" }\nchannel_id = \"1\"\n\
             allowed_responders = [42]\npoll_interval = \"1s\"\nnotify_on = [\"failed\"]\n\
             [instances.chat_bad]\nbot_token = { secret = \"token\" }\nchannel_id = \"bad\"\nallowed_responders = [\"7\"]\n\
             [instances.chat_off]\nenabled = false\n",
        );

        // Act
        let loaded = load(root.path(), &root.path().join("secrets"));
        let channels = loaded.channels();
        let channel = channels.get("chat_joe").unwrap();
        let delivery = channel.deliver(&json!({ "kind": "question" })).unwrap();
        let open = [clankjob_core::channel::OpenDelivery {
            request_id: clankjob_core::ids::HumanRequestId::from_string("R1"),
            delivery: delivery.clone(),
        }];
        let polled = channel.poll(&open, &Value::Null).unwrap();
        let retested = loaded.test("chat_joe").unwrap();
        loaded.shutdown();

        // Assert
        assert!(retested.checked_at.is_some() && loaded.test("chat_off").is_none());
        assert_eq!(channels.keys().collect::<Vec<_>>(), ["chat_bad", "chat_joe"]);
        assert!(
            loaded
                .errors()
                .iter()
                .any(|error| error.contains("chat_bad") && error.contains("401"))
        );
        assert!(
            loaded
                .errors()
                .contains(&"instance `chat_joe`: Message Content intent: turn it on".to_owned())
        );
        let statuses = loaded.statuses();
        let states: Vec<_> = statuses[0].instances.iter().map(|i| (i.name.as_str(), i.state)).collect();
        assert_eq!(
            states,
            [
                ("chat_bad", InstanceState::On),
                ("chat_joe", InstanceState::On),
                ("chat_off", InstanceState::Off)
            ]
        );
        let joe = loaded.instance("chat_joe").unwrap();
        assert_eq!(joe.report.get("bot"), Some(&json!("clankbot")));
        assert_eq!((joe.problems.len(), joe.warnings.len(), joe.error), (1, 1, None));
        assert!(loaded.instance("chat_bad").unwrap().error.unwrap().contains("401"));
        assert_eq!(delivery, json!({ "thread_id": "9", "token_seen": true }));
        assert_eq!(polled.replies[0].request_id.as_str(), "R1");
        assert_eq!(polled.cursor, json!({ "9": "10" }));
        assert_eq!(channel.allowed_responders(), ["42"]);
        assert_eq!(channel.poll_interval(), MIN_POLL_INTERVAL);
        assert!(channel.notifies("failed") && !channel.notifies("completed"));
        assert_eq!(channel.plugin(), "chat");
    }

    #[test]
    fn a_token_used_as_a_secret_name_is_refused_without_echoing_it() {
        let error = resolve(
            toml::Value::Table(toml::toml! { bot_token = { secret = "MTU.abc/def" } }),
            Path::new("/nonexistent"),
            "",
        )
        .unwrap_err();

        assert!(error.starts_with("bot_token: invalid secret name"));
        assert!(!error.contains("MTU"));
    }

    #[test]
    fn missing_responders_and_bad_manifests_are_reported() {
        let root = plugin_dir("[instances.chat_joe]\nbot_token = \"x\"\nchannel_id = \"1\"\n");
        std::fs::write(
            root.path().join("chat/plugin.toml"),
            "id = \"other\"\nruntime = \"python\"\n",
        )
        .unwrap();

        let loaded = load(root.path(), root.path());

        assert!(loaded.channels().is_empty());
        assert!(loaded.errors()[0].contains("must match the directory name"));
        let status = &loaded.statuses()[0];
        assert!(status.error.as_deref().unwrap().contains("must match"));
    }
}
