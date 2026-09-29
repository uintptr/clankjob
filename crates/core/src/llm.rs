//! Provider-agnostic LLM interface (design §8).
//!
//! Adapters translate these normalized types to and from each provider's wire format.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool the LLM may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name, matching `^[a-zA-Z0-9_-]+$`.
    pub name: String,
    /// What the tool does, shown to the LLM.
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: Value,
}

/// A tool invocation requested by the LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned id, echoed back in the matching tool result.
    pub id: String,
    /// Name of the tool to call.
    pub name: String,
    /// Arguments. A string holds arguments the provider returned as invalid JSON.
    pub arguments: Value,
}

/// What the LLM said in one turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    /// Free text, if any.
    pub text: Option<String>,
    /// Tool calls, in the order they should run.
    pub tool_calls: Vec<ToolCall>,
}

/// One message of the conversation sent to the LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    /// Input from the platform or the human.
    User {
        /// Message text.
        text: String,
        /// Images shown with the text, for models that can see them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageData>,
    },
    /// A previous LLM turn.
    Assistant(AssistantMessage),
    /// The result of one tool call.
    Tool {
        /// Id of the [`ToolCall`] this answers.
        tool_call_id: String,
        /// Result, usually JSON text.
        content: String,
    },
}

/// An image sent to the LLM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageData {
    /// Media type, e.g. `image/png`.
    pub media_type: String,
    /// The image bytes, base64 encoded.
    pub base64: String,
}

/// A request for the next LLM turn.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionRequest {
    /// Model name.
    pub model: String,
    /// System prompt.
    pub system: String,
    /// Conversation so far.
    pub messages: Vec<Message>,
    /// Tools the LLM may call.
    pub tools: Vec<ToolSpec>,
}

/// Tokens consumed by one completion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Prompt tokens.
    pub input_tokens: u64,
    /// Completion tokens.
    pub output_tokens: u64,
}

/// The LLM's answer to a [`CompletionRequest`].
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionResponse {
    /// What the LLM said.
    pub message: AssistantMessage,
    /// Tokens consumed.
    pub usage: TokenUsage,
}

/// A model an LLM endpoint offers, as reported by the provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model id to put in requests, e.g. `openai/gpt-4.1-mini`.
    pub id: String,
    /// Human-readable name, if the provider gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Maximum context size in tokens, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    /// US dollars per million input tokens, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_price: Option<f64>,
    /// US dollars per million output tokens, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_price: Option<f64>,
}

impl ModelInfo {
    /// A model known only by its id.
    #[must_use]
    pub fn from_id<S>(id: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            id: id.into(),
            name: None,
            context_length: None,
            input_price: None,
            output_price: None,
        }
    }
}

/// Why a completion failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    /// Worth retrying later: rate limit, server error, timeout, connection failure.
    #[error("retryable LLM error: {0}")]
    Retryable(String),
    /// Retrying will not help: bad credentials, invalid request, unparsable response.
    #[error("fatal LLM error: {0}")]
    Fatal(String),
}

/// A configured LLM backend.
///
/// `Send + Sync` lets one provider be shared by every worker thread behind an `Arc`.
pub trait LlmProvider: Send + Sync {
    /// Model used when a case does not override it.
    fn default_model(&self) -> &str;

    /// Run one completion. Blocks until the provider answers.
    ///
    /// # Arguments
    ///
    /// * `request` - Model, prompt, conversation and tools
    ///
    /// # Returns
    ///
    /// The assistant message and token usage
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::Retryable`] for transient failures and [`LlmError::Fatal`]
    /// otherwise.
    fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, LlmError>;

    /// Models this endpoint offers that can call tools, when the provider can say so.
    ///
    /// The default returns an empty list, meaning "unknown". Blocks on the network, so
    /// call it from a background thread, never from a request handler.
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] if the provider cannot be reached or answers badly.
    fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(Vec::new())
    }

    /// Whether the model can be shown images. Off unless the provider says so.
    fn supports_images(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_serialize_with_a_role_tag() {
        let message = Message::Tool {
            tool_call_id: "call_1".to_owned(),
            content: "{}".to_owned(),
        };

        let json = serde_json::to_value(&message).unwrap();

        assert_eq!(
            json,
            serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": "{}"})
        );
    }
}
