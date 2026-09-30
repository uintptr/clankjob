//! Cases: long-running tasks with a goal, a state, budgets and usage counters.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{CaseId, InstructionId};

string_enum!(
    /// Lifecycle state of a case (design §4).
    CaseState {
        /// Waiting for a worker to run its next activation.
        Pending => "pending",
        /// An activation is in progress.
        Running => "running",
        /// Suspended on one or more wait conditions.
        Sleeping => "sleeping",
        /// Suspended until its owner answers a human request.
        WaitingForHuman => "waiting_for_human",
        /// Finished successfully (terminal, can be reopened by a message).
        Completed => "completed",
        /// Finished unsuccessfully (terminal, can be reopened by a message).
        Failed => "failed",
        /// Stopped by a human (terminal, cannot be reopened).
        Cancelled => "cancelled",
    }
);

impl CaseState {
    /// Whether the case has finished and has no activation or wait pending.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        match self {
            Self::Completed | Self::Failed | Self::Cancelled => true,
            Self::Pending | Self::Running | Self::Sleeping | Self::WaitingForHuman => false,
        }
    }
}

string_enum!(
    /// Whether a case's tool calls that need approval (sending email) wait for the owner.
    /// Only the owner sets it, through the API or the web UI; the LLM cannot.
    #[derive(Default)]
    ApprovalPolicy {
        /// Ask, unless the tool's own check says the call is safe (e.g. an email to
        /// trusted contacts only).
        #[default]
        Default => "default",
        /// Never ask: every call runs at once, and says so in the timeline.
        Never => "never",
        /// Always ask, even for trusted contacts.
        Always => "always",
    }
);

/// Limits that protect against runaway loops and runaway cost (design §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budgets {
    /// Maximum number of activations (wake-ups that reach the LLM) over the case's life.
    pub max_activations: u32,
    /// Maximum LLM turns within a single activation.
    pub max_turns_per_activation: u32,
    /// Maximum input plus output tokens over the case's life.
    pub max_total_tokens: u64,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            max_activations: 20,
            max_turns_per_activation: 30,
            max_total_tokens: 2_000_000,
        }
    }
}

/// Resources a case has consumed so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    /// Number of activations started.
    pub activations: u32,
    /// Prompt tokens sent to the LLM.
    pub input_tokens: u64,
    /// Completion tokens received from the LLM.
    pub output_tokens: u64,
}

impl Usage {
    /// Input plus output tokens.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// Everything needed to create a case.
#[derive(Debug, Clone, PartialEq)]
pub struct NewCase {
    /// Short human-readable title.
    pub title: String,
    /// What the case must achieve, in natural language.
    pub goal: String,
    /// The person running the case.
    pub owner: Option<String>,
    /// Name of the prompt profile appended to the system prompt (design §7.4).
    pub profile: Option<String>,
    /// Name of the configured LLM to use.
    pub llm: String,
    /// Model override; `None` uses the LLM's configured default.
    pub model: Option<String>,
    /// Budgets for this case.
    pub budgets: Budgets,
    /// Instructions the first activation already follows (design §7.5).
    pub instructions: Vec<NewInstruction>,
    /// Channels its questions go to besides the web UI (design §10.4); `None` uses the
    /// server's default.
    pub human_channels: Option<Vec<String>>,
    /// When calls that need approval wait for the owner.
    pub approvals: ApprovalPolicy,
}

/// Instruction text to add to a case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewInstruction {
    /// Name shown to the owner and the LLM, e.g. `tone.md`.
    pub name: String,
    /// The instruction text, usually markdown.
    pub content: String,
}

/// Owner-written guidance for a case, always part of its system prompt (design §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Instruction {
    /// Unique id.
    pub id: InstructionId,
    /// The case it belongs to.
    pub case_id: CaseId,
    /// Name, e.g. `tone.md`.
    pub name: String,
    /// The instruction text.
    pub content: String,
    /// When it was added.
    pub created_at: DateTime<Utc>,
    /// When it was last edited.
    pub updated_at: DateTime<Utc>,
}

/// A case as stored.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Case {
    /// Unique id.
    pub id: CaseId,
    /// Short human-readable title.
    pub title: String,
    /// What the case must achieve.
    pub goal: String,
    /// The person running the case.
    pub owner: Option<String>,
    /// Prompt profile name.
    pub profile: Option<String>,
    /// Name of the configured LLM to use.
    pub llm: String,
    /// Model override.
    pub model: Option<String>,
    /// Current lifecycle state.
    pub state: CaseState,
    /// Limits for this case.
    pub budgets: Budgets,
    /// Resources consumed so far.
    pub usage: Usage,
    /// Structured result given to `complete`.
    pub result: Option<Value>,
    /// Summary given to `complete`, or reason given to `fail`.
    pub outcome: Option<String>,
    /// Channels its questions and notifications go to besides the web UI.
    pub human_channels: Vec<String>,
    /// When calls that need approval wait for the owner.
    pub approvals: ApprovalPolicy,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}

/// A durable fact the LLM keeps about a case (design §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaseNote {
    /// Note name, e.g. `thread_ref`.
    pub key: String,
    /// Note content.
    pub value: String,
    /// Last time it was written.
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_its_string_form() {
        for state in [
            CaseState::Pending,
            CaseState::Running,
            CaseState::Sleeping,
            CaseState::WaitingForHuman,
            CaseState::Completed,
            CaseState::Failed,
            CaseState::Cancelled,
        ] {
            assert_eq!(state.as_str().parse::<CaseState>().unwrap(), state);
            assert_eq!(serde_json::to_value(state).unwrap(), state.as_str());
        }
    }

    #[test]
    fn unknown_state_is_rejected() {
        let error = "asleep".parse::<CaseState>().unwrap_err();

        assert_eq!(error.to_string(), "unknown CaseState `asleep`");
    }

    #[test]
    fn only_finished_states_are_terminal() {
        assert!(CaseState::Completed.is_terminal());
        assert!(CaseState::Cancelled.is_terminal());
        assert!(!CaseState::Sleeping.is_terminal());
    }

    #[test]
    fn budgets_fill_missing_fields_with_defaults() {
        let budgets: Budgets = serde_json::from_str(r#"{"max_activations": 3}"#).unwrap();

        assert_eq!(budgets.max_activations, 3);
        assert_eq!(
            budgets.max_turns_per_activation,
            Budgets::default().max_turns_per_activation
        );
    }

    #[test]
    fn total_tokens_adds_input_and_output() {
        let usage = Usage {
            activations: 1,
            input_tokens: 10,
            output_tokens: 5,
        };

        assert_eq!(usage.total_tokens(), 15);
    }
}
