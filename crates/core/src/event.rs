//! Events: the append-only log of everything that happened to a case (design §7).
//!
//! The LLM context is rebuilt from this log on every activation, so it is the case's
//! memory as well as its audit trail.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::case::CaseState;
use crate::ids::{ActivationId, CaseId, HumanRequestId, WaitConditionId};
use crate::llm::AssistantMessage;

/// Why a case was woken up. Rendered into the conversation through the `wake` prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum WakeReason {
    /// The case was just created.
    Created,
    /// The owner posted a message that was not an answer to a question.
    HumanMessage {
        /// Message text.
        text: String,
    },
    /// The owner answered a question the case asked.
    HumanAnswer {
        /// The request that was answered.
        request_id: HumanRequestId,
        /// The question as asked.
        question: String,
        /// The answer.
        answer: String,
    },
    /// A wait condition fired.
    ConditionFired {
        /// The condition.
        condition_id: WaitConditionId,
        /// Its kind.
        kind: String,
        /// Kind-specific details, e.g. the emails that arrived.
        details: Vec<Value>,
    },
    /// A wait condition reached its deadline.
    TimedOut {
        /// The condition.
        condition_id: WaitConditionId,
        /// Its kind.
        kind: String,
    },
    /// The owner woke the case up by hand.
    Manual,
}

/// The result of one tool call, as recorded and shown to the LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Id of the tool call this answers.
    pub tool_call_id: String,
    /// Name of the tool that ran.
    pub tool_name: String,
    /// What the tool returned, or the error.
    pub content: Value,
    /// Whether `content` describes an error.
    pub is_error: bool,
}

/// What happened. The variant name is stored as the event `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// Adjacent tagging serializes as `{"kind": "wake", "payload": {...}}`.
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum EventBody {
    /// The case was woken up (becomes a user message).
    Wake(WakeReason),
    /// The LLM produced a turn (becomes an assistant message).
    LlmMessage(AssistantMessage),
    /// A tool call finished (becomes a tool message).
    ToolResult(ToolResult),
    /// The LLM was reminded to call a tool (becomes a user message).
    Nudge,
    /// The case changed state (timeline only).
    StateChanged {
        /// Previous state.
        from: CaseState,
        /// New state.
        to: CaseState,
    },
    /// Something went wrong outside the LLM's control (timeline only).
    Error {
        /// What went wrong.
        message: String,
    },
}

impl EventBody {
    /// The stable `kind` string stored with the event.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Wake(_) => "wake",
            Self::LlmMessage(_) => "llm_message",
            Self::ToolResult(_) => "tool_result",
            Self::Nudge => "nudge",
            Self::StateChanged { .. } => "state_changed",
            Self::Error { .. } => "error",
        }
    }
}

/// A recorded event.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    /// Position in the global log; strictly increasing.
    pub seq: i64,
    /// The case it belongs to.
    pub case_id: CaseId,
    /// The activation that produced it, if any.
    pub activation_id: Option<ActivationId>,
    /// What happened.
    // `flatten` puts `kind` and `payload` next to the other fields in JSON.
    #[serde(flatten)]
    pub body: EventBody,
    /// When it was recorded.
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_matches_the_serialized_tag() {
        let bodies = [
            EventBody::Wake(WakeReason::Manual),
            EventBody::LlmMessage(AssistantMessage::default()),
            EventBody::Nudge,
            EventBody::StateChanged {
                from: CaseState::Pending,
                to: CaseState::Running,
            },
            EventBody::Error {
                message: "boom".to_owned(),
            },
        ];

        for body in bodies {
            let json = serde_json::to_value(&body).unwrap();
            assert_eq!(json["kind"], body.kind());
        }
    }

    #[test]
    fn wake_reason_round_trips() {
        let body = EventBody::Wake(WakeReason::HumanMessage {
            text: "hello".to_owned(),
        });

        let json = serde_json::to_value(&body).unwrap();
        let back: EventBody = serde_json::from_value(json.clone()).unwrap();

        assert_eq!(json["payload"]["reason"], "human_message");
        assert_eq!(back, body);
    }
}
