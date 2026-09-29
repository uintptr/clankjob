//! Human requests: questions a case asks its owner, and approvals it needs before a tool
//! with outside effects runs (design §10).

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::ids::{CaseId, HumanRequestId};

string_enum!(
    /// Lifecycle of a human request.
    HumanRequestStatus {
        /// Waiting for an answer.
        Open => "open",
        /// Answered from some channel.
        Answered => "answered",
        /// No longer needed: something else woke the case first.
        Superseded => "superseded",
        /// The case was cancelled.
        Cancelled => "cancelled",
    }
);

string_enum!(
    /// What a human request asks for.
    HumanRequestKind {
        /// A free-text answer to a question.
        Question => "question",
        /// Approve or reject a tool call before it runs (design §9.7).
        Approval => "approval",
    }
);

string_enum!(
    /// The owner's decision on an approval.
    Decision {
        /// Run the tool call.
        Approve => "approve",
        /// Do not run it.
        Reject => "reject",
    }
);

string_enum!(
    /// Progress of an approved tool call. It moves to `running` in its own transaction
    /// before the tool runs, so a crash can never run it twice.
    Execution {
        /// Approved; runs at the start of the case's next activation.
        Pending => "pending",
        /// Started; if the server stops now, the outcome is unknown and it is not re-run.
        Running => "running",
        /// Finished; the result is in the case's events.
        Done => "done",
    }
);

/// A question a case asked its owner, or an approval it needs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HumanRequest {
    /// Unique id.
    pub id: HumanRequestId,
    /// The case that asked.
    pub case_id: CaseId,
    /// Question or approval.
    pub kind: HumanRequestKind,
    /// The question, or for an approval what the tool call will do.
    pub question: String,
    /// For an approval: the tool to run.
    pub tool: Option<String>,
    /// For an approval: its arguments (as edited by the owner, once approved).
    pub args: Option<Value>,
    /// For an approval: the owner's decision.
    pub decision: Option<Decision>,
    /// For an approved tool call: whether it ran.
    pub execution: Option<Execution>,
    /// Current status.
    pub status: HumanRequestStatus,
    /// The answer, once answered; for an approval, the owner's comment, if any.
    pub answer: Option<String>,
    /// Channel the answer came from, e.g. `web`.
    pub answered_via: Option<String>,
    /// Who answered, as identified by the channel.
    pub responder: Option<String>,
    /// When the question was asked.
    pub created_at: DateTime<Utc>,
    /// When it was answered, superseded or cancelled.
    pub resolved_at: Option<DateTime<Utc>>,
}
