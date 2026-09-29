//! Plugin tools: functions a plugin offers the LLM, next to the core tools (design §9).
//!
//! The engine only knows the [`PluginTool`] trait; the plugin host implements it, e.g. by
//! running a command line.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::llm::ToolSpec;

/// What a plugin tool produced.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutput {
    /// A result shown to the LLM as is.
    Json(Value),
    /// Output too long to show at once, e.g. a transcript: stored as a case file that the
    /// LLM reads with `read_file`, with `summary` shown as the result.
    File {
        /// File name to store it under.
        name: String,
        /// The content.
        content: Vec<u8>,
        /// Shown to the LLM, e.g. a preview.
        summary: Value,
    },
}

/// A file of the case, as a plugin tool can use it (e.g. to OCR a scanned PDF).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseFileRef {
    /// Its name in the case, e.g. `quote.pdf`; what the LLM refers to it by.
    pub name: String,
    /// Where its bytes are on disk (named by id, without extension).
    pub path: PathBuf,
    /// Its detected media type, e.g. `application/pdf`.
    pub media_type: String,
}

/// What a plugin tool call can see of its case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolContext {
    /// The case's files, for arguments of type `file`.
    pub files: Vec<CaseFileRef>,
}

/// A tool offered by a plugin.
pub trait PluginTool: Send + Sync {
    /// Plugin providing it.
    fn plugin(&self) -> &str;

    /// Name, description and argument schema, as shown to the LLM.
    fn spec(&self) -> &ToolSpec;

    /// For a tool with outside effects (sending an email): what a call will do, shown to
    /// the owner, who must approve it before it runs (design §9.7). `None` for tools that
    /// run without approval.
    fn approval_summary(&self, _arguments: &Value) -> Option<String> {
        None
    }

    /// Check the arguments without running anything, e.g. before asking for approval.
    ///
    /// # Errors
    ///
    /// Returns a message for the LLM.
    fn validate(&self, _arguments: &Value) -> Result<(), String> {
        Ok(())
    }

    /// Run it. Blocks until done or until the tool's timeout. MUST NOT call the LLM.
    ///
    /// # Arguments
    ///
    /// * `arguments` - As given by the LLM
    /// * `context` - The case's files, for arguments that name one
    ///
    /// # Errors
    ///
    /// Returns a message for the LLM (invalid arguments, the command failed, …).
    fn run(&self, arguments: &Value, context: &ToolContext) -> Result<ToolOutput, String>;
}

/// Instructions a plugin offers for a kind of task, read by the LLM on demand with
/// `read_guide` (e.g. how to analyse an earnings call transcript).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Guide {
    /// Plugin providing it.
    pub plugin: String,
    /// Name the LLM asks for.
    pub name: String,
    /// When it applies, shown in the system prompt.
    pub description: String,
    /// The instructions, usually markdown.
    #[serde(skip)]
    pub content: String,
}

/// What a condition check found.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckOutcome {
    /// Nothing yet; check again later, from `cursor`.
    Pending {
        /// Where to continue, handed back next time.
        cursor: Option<Value>,
    },
    /// It happened: wake the case with these details.
    Fired {
        /// What happened, e.g. the replies found.
        events: Vec<Value>,
        /// Where to continue.
        cursor: Option<Value>,
    },
}

/// A wait condition a plugin offers `sleep` (design §6), e.g. `email_reply_received`.
pub trait PluginCondition: Send + Sync {
    /// Plugin providing it.
    fn plugin(&self) -> &str;

    /// Kind name used in `sleep`, e.g. `email_reply_received`.
    fn name(&self) -> &str;

    /// When to use it, shown to the LLM.
    fn description(&self) -> &str;

    /// JSON Schema of its `params`.
    fn params_schema(&self) -> &Value;

    /// Check interval when `sleep` sets none.
    fn default_interval(&self) -> Duration;

    /// Shortest interval allowed, to keep the load on the service reasonable.
    fn min_interval(&self) -> Duration;

    /// Check `params` when the LLM requests the condition.
    ///
    /// # Errors
    ///
    /// Returns a message for the LLM.
    fn validate(&self, params: &Value) -> Result<(), String>;

    /// Cheap, deterministic check. Blocks until done or until its timeout. MUST NOT call
    /// the LLM.
    ///
    /// # Errors
    ///
    /// Returns what went wrong; the engine retries with backoff.
    fn check(&self, params: &Value, cursor: Option<&Value>) -> Result<CheckOutcome, String>;
}
