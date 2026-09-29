//! Plugin tools: functions a plugin offers the LLM, next to the core tools (design §9).
//!
//! The engine only knows the [`PluginTool`] trait; the plugin host implements it, e.g. by
//! running a command line.

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

/// A tool offered by a plugin.
pub trait PluginTool: Send + Sync {
    /// Plugin providing it.
    fn plugin(&self) -> &str;

    /// Name, description and argument schema, as shown to the LLM.
    fn spec(&self) -> &ToolSpec;

    /// Run it. Blocks until done or until the tool's timeout. MUST NOT call the LLM.
    ///
    /// # Errors
    ///
    /// Returns a message for the LLM (invalid arguments, the command failed, …).
    fn run(&self, arguments: &Value) -> Result<ToolOutput, String>;
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
