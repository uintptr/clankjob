//! Command plugins (`runtime = "command"`): each tool is a command line, so any CLI
//! script can be offered to the LLM without writing a plugin protocol (design §9.9).
//!
//! Arguments are declared in the manifest and checked before anything runs; values are
//! placed into the command line as separate arguments, never through a shell, and may
//! not start with `-`, so the LLM cannot slip in options such as `--out /etc/passwd`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use clankjob_core::llm::ToolSpec;
use clankjob_core::tool::{CaseFileRef, CheckOutcome, PluginCondition, PluginTool, ToolContext, ToolOutput};
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
/// Longest accepted argument value, unless the argument sets `max_length`.
const MAX_ARG_CHARS: usize = 2_000;
/// Longest `max_length` an argument may set (e.g. an email body).
const MAX_LONG_ARG_CHARS: usize = 100_000;
/// Longest approval summary.
const MAX_SUMMARY_CHARS: usize = 300;
/// Default check interval of a condition.
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_mins(15);
/// Default shortest check interval of a condition.
const DEFAULT_MIN_CHECK_INTERVAL: Duration = Duration::from_mins(5);
/// Default time one check may run.
const DEFAULT_CHECK_TIMEOUT: Duration = Duration::from_mins(1);

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
    /// The command prints JSON, returned as the result.
    Json,
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
    /// The name of one of the case's files; the command gets a path to it, named as in
    /// the case (e.g. `/tmp/clankjob-…/quote.pdf`).
    File,
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
    /// Longest value accepted (default 2 000 characters, at most 100 000).
    #[serde(default)]
    pub max_length: Option<usize>,
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
    /// The owner must approve each call before it runs (design §9.7), e.g. sending email.
    #[serde(default)]
    pub requires_approval: bool,
    /// What a call does, shown to the owner for approval; `{name}` placeholders allowed,
    /// e.g. `"Email {to}: {subject}"`.
    #[serde(default)]
    pub approval: Option<String>,
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
    spec: ToolSpec,
    manifest: ToolManifest,
    runner: Runner,
    standalone: BTreeSet<String>,
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

/// Where file arguments are linked for one call; removed when dropped.
struct LinkedFiles {
    dir: Option<PathBuf>,
}

impl Drop for LinkedFiles {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            // Only links live here; the case's files themselves are untouched.
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Distinguishes the link directories of concurrent calls.
static NEXT_LINK_DIR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Find a case file the LLM named: exact name first, then ignoring case.
fn find_file<'a>(context: &'a ToolContext, wanted: &str) -> Result<&'a CaseFileRef, String> {
    context
        .files
        .iter()
        .find(|file| file.name == wanted)
        .or_else(|| context.files.iter().find(|file| file.name.eq_ignore_ascii_case(wanted)))
        .ok_or_else(|| {
            if context.files.is_empty() {
                format!("no case file named `{wanted}`: the case has no files")
            } else {
                let names: Vec<&str> = context.files.iter().map(|file| file.name.as_str()).collect();
                format!("no case file named `{wanted}`; the case's files: {}", names.join(", "))
            }
        })
}

/// Replace the value of every `file` argument with a path to that case file, linked
/// under its own name in a fresh private directory, since the store names files by id
/// and many tools go by the extension.
fn link_files(
    args: &BTreeMap<String, ArgManifest>,
    values: &mut BTreeMap<String, String>,
    context: &ToolContext,
) -> Result<LinkedFiles, String> {
    let mut linked = LinkedFiles { dir: None };
    for (name, declared) in args {
        if declared.kind != ArgType::File {
            continue;
        }
        let Some(value) = values.get_mut(name) else { continue };
        let file = find_file(context, value)?;
        let dir = if let Some(dir) = &linked.dir {
            dir.clone()
        } else {
            let number = NEXT_LINK_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("clankjob-{}-{number}", std::process::id()));
            std::fs::create_dir_all(&dir).map_err(|error| format!("cannot prepare `{name}`: {error}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
            linked.dir = Some(dir.clone());
            dir
        };
        // Case file names are already a single path component; stay safe regardless.
        let file_name = file_name_part(&file.name);
        let file_name = if file_name.is_empty() {
            "file".to_owned()
        } else {
            file_name
        };
        let link = dir.join(format!("{name}-{file_name}"));
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&file.path, &link);
        #[cfg(not(unix))]
        let made = std::fs::copy(&file.path, &link).map(drop);
        made.map_err(|error| format!("cannot prepare `{}`: {error}", file.name))?;
        *value = link.to_string_lossy().into_owned();
    }
    Ok(linked)
}

/// JSON Schema of declared arguments, as shown to the LLM.
fn schema_of(args: &BTreeMap<String, ArgManifest>) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (arg, declared) in args {
        let kind = match declared.kind {
            ArgType::String | ArgType::File => "string",
            ArgType::Integer => "integer",
            ArgType::Number => "number",
            ArgType::Boolean => "boolean",
        };
        let description = if declared.kind == ArgType::File {
            format!(
                "{} (the name of one of the case's files, as listed under Files)",
                declared.description
            )
        } else {
            declared.description.clone()
        };
        let mut schema = json!({ "type": kind, "description": description });
        if let Some(object) = schema.as_object_mut() {
            if !declared.choices.is_empty() {
                object.insert("enum".to_owned(), json!(declared.choices));
            }
            if let Some(max) = declared.max_length {
                object.insert("maxLength".to_owned(), json!(max));
            }
        }
        properties.insert(arg.clone(), schema);
        if declared.required {
            required.push(arg.clone());
        }
    }
    json!({ "type": "object", "properties": properties, "required": required })
}

/// Check a name used by the LLM: 1-64 letters, digits, `_` or `-`.
fn check_name(what: &str, name: &str) -> Result<(), String> {
    let valid =
        !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(format!("{what} name `{name}` must be 1-64 letters, digits, `_` or `-`"))
    }
}

fn parse_timeout(what: &str, text: Option<&str>, default: Duration) -> Result<Duration, String> {
    text.map_or(Ok(default), |text| {
        humantime::parse_duration(text).map_err(|error| format!("{what}: `{text}`: {error}"))
    })
}

/// Check the LLM's arguments against their declarations and turn them into text values.
///
/// `standalone` names the arguments that fill a whole command-line element: those may not
/// start with `-`, so they can never be taken for an option. An argument embedded in a
/// larger element (`--body={body}`) may.
fn check_values(
    declared_args: &BTreeMap<String, ArgManifest>,
    standalone: &BTreeSet<String>,
    arguments: &Value,
) -> Result<BTreeMap<String, String>, String> {
    let empty = Map::new();
    let given = match arguments {
        Value::Object(object) => object,
        Value::Null => &empty,
        _ => return Err("arguments must be an object".to_owned()),
    };
    if let Some(unknown) = given.keys().find(|key| !declared_args.contains_key(*key)) {
        let known: Vec<&str> = declared_args.keys().map(String::as_str).collect();
        return Err(format!(
            "unknown argument `{unknown}`; this takes: {}",
            known.join(", ")
        ));
    }
    let mut values = BTreeMap::new();
    for (name, declared) in declared_args {
        let value = match given.get(name) {
            None | Some(Value::Null) if declared.required => return Err(format!("`{name}` is required")),
            None | Some(Value::Null) => continue,
            Some(value) => value,
        };
        let text = match (declared.kind, value) {
            (ArgType::String | ArgType::File, Value::String(text)) => text.trim().to_owned(),
            (ArgType::Integer, Value::Number(number)) if number.is_i64() || number.is_u64() => number.to_string(),
            (ArgType::Number, Value::Number(number)) => number.to_string(),
            (ArgType::Boolean, Value::Bool(flag)) => flag.to_string(),
            (kind, _) => return Err(format!("`{name}` must be a {kind:?}").to_lowercase()),
        };
        if text.is_empty() {
            // Models often fill optional fields with "" rather than leaving them out;
            // that means "not given", not a value.
            if declared.required {
                return Err(format!("`{name}` is required and cannot be empty"));
            }
            continue;
        }
        if text.starts_with('-') && standalone.contains(name) {
            return Err(format!("`{name}` cannot start with `-`"));
        }
        let max = declared.max_length.unwrap_or(MAX_ARG_CHARS).min(MAX_LONG_ARG_CHARS);
        if text.contains('\0') || text.chars().count() > max {
            return Err(format!(
                "`{name}` is longer than {max} characters or contains a NUL byte"
            ));
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

/// Check a command template against the declared arguments.
///
/// # Returns
///
/// The arguments that fill a whole command-line element
fn check_command(
    what: &str,
    command: &[String],
    options: &BTreeMap<String, Vec<String>>,
    args: &BTreeMap<String, ArgManifest>,
) -> Result<BTreeSet<String>, String> {
    let Some(program) = command.first() else {
        return Err(format!("{what}: `command` is empty"));
    };
    if !placeholders(program).is_empty() {
        return Err(format!("{what}: the program cannot be an argument"));
    }
    for used in command.iter().flat_map(|part| placeholders(part)) {
        match args.get(used) {
            Some(arg) if arg.required => {}
            Some(_) => {
                return Err(format!(
                    "{what}: `{{{used}}}` in `command` must be a required argument; put optional ones in `options`"
                ));
            }
            None => return Err(format!("{what}: `{{{used}}}` is not a declared argument")),
        }
    }
    for (option, parts) in options {
        if !args.contains_key(option) {
            return Err(format!("{what}: option `{option}` is not a declared argument"));
        }
        if let Some(other) = parts.iter().flat_map(|part| placeholders(part)).find(|used| used != option) {
            return Err(format!(
                "{what}: option `{option}` can only use `{{{option}}}`, not `{{{other}}}`"
            ));
        }
    }
    Ok(command
        .iter()
        .chain(options.values().flatten())
        .filter_map(|part| part.strip_prefix('{').and_then(|rest| rest.strip_suffix('}')))
        .filter(|name| args.contains_key(*name))
        .map(str::to_owned)
        .collect())
}

/// The argument list after the program.
fn argv_of(
    command: &[String],
    options: &BTreeMap<String, Vec<String>>,
    values: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut argv: Vec<String> = command.iter().skip(1).map(|part| fill(part, values)).collect();
    for (option, parts) in options {
        if values.contains_key(option) {
            argv.extend(parts.iter().map(|part| fill(part, values)));
        }
    }
    argv
}

/// `ETXTBSY`: the program file is still open for writing somewhere.
const TEXT_FILE_BUSY: i32 = 26;

/// Start a command, retrying briefly while its program is "text file busy". That happens
/// when a script was just written and another thread forked meanwhile, so the child
/// briefly holds the file open; it clears within milliseconds.
fn spawn(command: &mut Command) -> std::io::Result<std::process::Child> {
    let mut attempts = 0_u32;
    loop {
        match command.spawn() {
            Err(error) if error.raw_os_error() == Some(TEXT_FILE_BUSY) && attempts < 20 => {
                attempts = attempts.saturating_add(1);
                thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

/// How to run a command.
struct Runner {
    name: String,
    dir: PathBuf,
    program: PathBuf,
    env: BTreeMap<String, String>,
    timeout: Duration,
}

impl Runner {
    /// Run the command and wait for it, within the timeout, feeding `stdin` if given.
    fn run(&self, argv: &[String], stdin: Option<String>) -> Result<String, String> {
        let name = &self.name;
        let mut command = Command::new(&self.program);
        command
            .args(argv)
            .current_dir(&self.dir)
            .env_clear()
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::process::own_process_group(&mut command);
        for variable in PASSED_ENV {
            if let Some(value) = std::env::var_os(variable) {
                command.env(variable, value);
            }
        }
        command.envs(&self.env);
        let mut child = spawn(&mut command).map_err(|error| format!("`{name}` could not start: {error}"))?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            // Written from a thread so a child that does not read stdin cannot block us.
            thread::spawn(move || {
                let _ = pipe.write_all(input.as_bytes());
            });
        }
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
        let name = manifest.name.clone();
        check_name("tool", &name)?;
        let what = format!("tool `{name}`");
        let standalone = check_command(&what, &manifest.command, &manifest.options, &manifest.args)?;
        let timeout = parse_timeout(&what, manifest.timeout.as_deref(), DEFAULT_TIMEOUT)?;
        let program = manifest.command.first().map(String::as_str).unwrap_or_default();
        let program = locate(program, dir).map_err(|error| format!("{what}: {error}"))?;
        let spec = ToolSpec {
            name: name.clone(),
            description: manifest.description.clone(),
            parameters: schema_of(&manifest.args),
        };
        Ok(Self {
            plugin: plugin.to_owned(),
            spec,
            runner: Runner {
                name,
                dir: dir.to_path_buf(),
                program,
                env,
                timeout,
            },
            standalone,
            manifest,
        })
    }

    fn values(&self, arguments: &Value) -> Result<BTreeMap<String, String>, String> {
        check_values(&self.manifest.args, &self.standalone, arguments)
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

    fn approval_summary(&self, arguments: &Value) -> Option<String> {
        if !self.manifest.requires_approval {
            return None;
        }
        let values = self.values(arguments).unwrap_or_default();
        let summary = match &self.manifest.approval {
            Some(template) => {
                let filled = fill(template, &values);
                // Placeholders of optional arguments that were not given are dropped.
                placeholders(template)
                    .into_iter()
                    .fold(filled, |text, name| text.replace(&format!("{{{name}}}"), ""))
            }
            None => format!("Run `{}`", self.spec.name),
        };
        Some(summary.chars().take(MAX_SUMMARY_CHARS).collect())
    }

    fn validate(&self, arguments: &Value) -> Result<(), String> {
        self.values(arguments).map(drop)
    }

    fn run(&self, arguments: &Value, context: &ToolContext) -> Result<ToolOutput, String> {
        let names = self.values(arguments)?;
        let mut values = names.clone();
        // Kept alive until the command has finished; removes the links when dropped.
        let _links = link_files(&self.manifest.args, &mut values, context)?;
        let output = self
            .runner
            .run(&argv_of(&self.manifest.command, &self.manifest.options, &values), None)?;
        let values = names;
        let chars = output.chars().count();
        let inline = match self.manifest.output {
            OutputMode::Json => {
                return serde_json::from_str(&output)
                    .map(ToolOutput::Json)
                    .map_err(|error| format!("`{}` printed invalid JSON: {error}", self.spec.name));
            }
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

/// A wait condition: `[[conditions]]` in `plugin.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionManifest {
    /// Kind name used in `sleep`, e.g. `email_reply_received`.
    pub name: String,
    /// When to use it, shown to the LLM.
    pub description: String,
    /// Program and arguments. It reads `{"params": …, "cursor": …}` on stdin and prints
    /// `{"status": "pending" | "fired", "events": […], "cursor": …}`.
    pub command: Vec<String>,
    /// Declared params, checked when `sleep` asks for the condition.
    #[serde(default)]
    pub params: BTreeMap<String, ArgManifest>,
    /// Check interval when `sleep` sets none (default 15m).
    #[serde(default)]
    pub interval: Option<String>,
    /// Shortest interval allowed (default 5m).
    #[serde(default)]
    pub min_interval: Option<String>,
    /// How long one check may run (default 1m).
    #[serde(default)]
    pub timeout: Option<String>,
}

/// A command wait condition, ready to check.
pub struct CommandCondition {
    plugin: String,
    name: String,
    description: String,
    schema: Value,
    params: BTreeMap<String, ArgManifest>,
    argv: Vec<String>,
    interval: Duration,
    min_interval: Duration,
    runner: Runner,
}

impl CommandCondition {
    /// Check a condition's manifest and build it.
    ///
    /// # Errors
    ///
    /// Returns what is wrong with the manifest, or that the program cannot be found.
    pub fn new(
        plugin: &str,
        dir: &Path,
        manifest: ConditionManifest,
        env: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        check_name("condition", &manifest.name)?;
        let what = format!("condition `{}`", manifest.name);
        if manifest.command.iter().any(|part| !placeholders(part).is_empty()) {
            return Err(format!(
                "{what}: params reach the command on stdin, not as `{{…}}` in `command`"
            ));
        }
        let program = manifest.command.first().ok_or_else(|| format!("{what}: `command` is empty"))?;
        let program = locate(program, dir).map_err(|error| format!("{what}: {error}"))?;
        let interval = parse_timeout(&what, manifest.interval.as_deref(), DEFAULT_CHECK_INTERVAL)?;
        let min_interval = parse_timeout(&what, manifest.min_interval.as_deref(), DEFAULT_MIN_CHECK_INTERVAL)?;
        let timeout = parse_timeout(&what, manifest.timeout.as_deref(), DEFAULT_CHECK_TIMEOUT)?;
        Ok(Self {
            plugin: plugin.to_owned(),
            schema: schema_of(&manifest.params),
            argv: manifest.command.iter().skip(1).cloned().collect(),
            interval: interval.max(min_interval),
            min_interval,
            runner: Runner {
                name: manifest.name.clone(),
                dir: dir.to_path_buf(),
                program,
                env,
                timeout,
            },
            name: manifest.name,
            description: manifest.description,
            params: manifest.params,
        })
    }
}

/// What a check command prints.
#[derive(Deserialize)]
struct CheckReport {
    status: String,
    #[serde(default)]
    events: Vec<Value>,
    #[serde(default)]
    cursor: Option<Value>,
}

impl PluginCondition for CommandCondition {
    fn plugin(&self) -> &str {
        &self.plugin
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn params_schema(&self) -> &Value {
        &self.schema
    }

    fn default_interval(&self) -> Duration {
        self.interval
    }

    fn min_interval(&self) -> Duration {
        self.min_interval
    }

    fn validate(&self, params: &Value) -> Result<(), String> {
        check_values(&self.params, &BTreeSet::new(), params).map(drop)
    }

    fn check(&self, params: &Value, cursor: Option<&Value>) -> Result<CheckOutcome, String> {
        let input = json!({ "params": params, "cursor": cursor }).to_string();
        let output = self.runner.run(&self.argv, Some(input))?;
        let report: CheckReport = serde_json::from_str(&output)
            .map_err(|error| format!("`{}` printed an invalid report: {error}", self.name))?;
        match report.status.as_str() {
            "pending" => Ok(CheckOutcome::Pending { cursor: report.cursor }),
            "fired" => Ok(CheckOutcome::Fired {
                events: report.events,
                cursor: report.cursor,
            }),
            other => Err(format!(
                "`{}` reported status `{other}`; expected pending or fired",
                self.name
            )),
        }
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
        let short = tool
            .run(&json!({ "video": "abc def", "lang": "de" }), &ToolContext::default())
            .unwrap();

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
    fn empty_optional_arguments_count_as_not_given() {
        let (_dir, tool) = tool("");

        let blank_option = tool
            .run(&json!({ "video": "abc", "lang": " " }), &ToolContext::default())
            .unwrap();
        let blank_required = tool.run(&json!({ "video": "" }), &ToolContext::default()).unwrap_err();

        let ToolOutput::Json(value) = blank_option else {
            unreachable!("expected inline output")
        };
        assert!(
            value["output"].as_str().unwrap().contains("args: abc --format stamped\n"),
            "no --lang added"
        );
        assert_eq!(blank_required, "`video` is required and cannot be empty");
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
        .map(|arguments| tool.run(arguments, &ToolContext::default()).unwrap_err())
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
        let long = tool.run(&json!({ "video": "long" }), &ToolContext::default()).unwrap();
        let failed = tool.run(&json!({ "video": "fail" }), &ToolContext::default()).unwrap_err();
        let slow = tool.run(&json!({ "video": "slow" }), &ToolContext::default()).unwrap_err();

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
            tool.run(&json!({}), &ToolContext::default()).unwrap(),
            ToolOutput::Json(json!({ "output": "hello\n", "truncated": false }))
        );
    }

    fn script(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn approval_tools_summarise_calls_and_print_json() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "send.sh",
            "#!/bin/sh\nprintf '{\"sent_to\": \"%s\", \"body\": \"%s\"}' \"$1\" \"${2#--body=}\"\n",
        );
        let text = "name = \"send_email\"\ndescription = \"Send.\"\ncommand = [\"./send.sh\", \"{to}\", \"--body={body}\"]\n\
                    output = \"json\"\nrequires_approval = true\napproval = \"Email {to}{cc}: {body}\"\n\
                    [args.to]\ndescription = \"To\"\nrequired = true\n\
                    [args.body]\ndescription = \"Body\"\nrequired = true\nmax_length = 5000\n\
                    [args.cc]\ndescription = \"Cc\"\n";
        let tool = CommandTool::new("email", dir.path(), manifest(text), BTreeMap::new()).unwrap();
        let args = json!({ "to": "bob@x.ca", "body": "- first point" });

        // Act
        let summary = tool.approval_summary(&args);
        let output = tool.run(&args, &ToolContext::default()).unwrap();
        let long = tool.validate(&json!({ "to": "bob@x.ca", "body": "x".repeat(5001) }));
        let dash = tool.validate(&json!({ "to": "-oProxyCommand=evil", "body": "hi" }));

        // Assert
        assert_eq!(summary.as_deref(), Some("Email bob@x.ca: - first point"));
        assert_eq!(
            output,
            ToolOutput::Json(json!({ "sent_to": "bob@x.ca", "body": "- first point" }))
        );
        assert!(long.unwrap_err().contains("longer than 5000"));
        assert_eq!(
            dash.unwrap_err(),
            "`to` cannot start with `-`",
            "a whole-element value cannot be an option"
        );
    }

    #[test]
    fn conditions_get_params_and_cursor_on_stdin_and_report_back() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "check.py",
            "#!/usr/bin/env python3\nimport json, sys\nrequest = json.load(sys.stdin)\n\
             seen = (request['cursor'] or {}).get('seen', 0)\n\
             if seen >= 1:\n    print(json.dumps({'status': 'fired', 'events': [{'thread': request['params']['thread']}], 'cursor': {'seen': seen + 1}}))\n\
             else:\n    print(json.dumps({'status': 'pending', 'cursor': {'seen': seen + 1}}))\n",
        );
        let text = "name = \"reply_received\"\ndescription = \"A reply.\"\ncommand = [\"./check.py\"]\n\
                    interval = \"1m\"\nmin_interval = \"5m\"\n[params.thread]\ndescription = \"Thread\"\nrequired = true\n";
        let manifest: ConditionManifest = toml::from_str(text).unwrap();
        let condition = CommandCondition::new("email", dir.path(), manifest, BTreeMap::new()).unwrap();
        let params = json!({ "thread": "<1@x>" });

        // Act
        let first = condition.check(&params, None).unwrap();
        let second = condition.check(&params, Some(&json!({ "seen": 1 }))).unwrap();

        // Assert
        assert_eq!(
            first,
            CheckOutcome::Pending {
                cursor: Some(json!({ "seen": 1 }))
            }
        );
        assert_eq!(
            second,
            CheckOutcome::Fired {
                events: vec![json!({ "thread": "<1@x>" })],
                cursor: Some(json!({ "seen": 2 }))
            }
        );
        assert_eq!(
            condition.default_interval(),
            Duration::from_mins(5),
            "clamped to the minimum"
        );
        assert_eq!(condition.validate(&json!({})).unwrap_err(), "`thread` is required");
        assert_eq!(condition.params_schema()["required"], json!(["thread"]));
    }

    #[test]
    fn file_arguments_get_a_path_named_like_the_case_file() {
        // Arrange: the store names files by id, without extension
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "show.sh",
            "#!/bin/sh\nbasename \"$1\"\ncat \"$1\"\necho\necho \"$1\" > seen_path\n",
        );
        let stored = dir.path().join("01J9STOREDBYID");
        std::fs::write(&stored, "Total: $1,450").unwrap();
        let context = ToolContext {
            files: vec![CaseFileRef {
                name: "Quote.pdf".to_owned(),
                path: stored,
                media_type: "application/pdf".to_owned(),
            }],
        };
        let text = "name = \"show\"\ndescription = \"d\"\ncommand = [\"./show.sh\", \"{document}\"]\n\
                    [args.document]\ntype = \"file\"\ndescription = \"The document\"\nrequired = true\n";
        let tool = CommandTool::new("docs", dir.path(), manifest(text), BTreeMap::new()).unwrap();

        // Act
        let output = tool.run(&json!({ "document": "quote.pdf" }), &context).unwrap();
        let missing = tool.run(&json!({ "document": "invoice.pdf" }), &context).unwrap_err();

        // Assert
        let ToolOutput::Json(value) = output else {
            unreachable!("expected inline output")
        };
        assert_eq!(value["output"], "document-Quote.pdf\nTotal: $1,450\n");
        let seen = std::fs::read_to_string(dir.path().join("seen_path")).unwrap();
        assert!(!Path::new(seen.trim()).exists(), "the link is removed after the call");
        assert_eq!(missing, "no case file named `invoice.pdf`; the case's files: Quote.pdf");
        assert!(
            tool.spec().parameters["properties"]["document"]["description"]
                .as_str()
                .unwrap()
                .contains("the name of one of the case's files")
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
