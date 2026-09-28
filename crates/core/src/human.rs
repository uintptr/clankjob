//! Human requests: questions a case asks its owner (design §10).

use chrono::{DateTime, Utc};
use serde::Serialize;

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

/// A question a case asked its owner.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HumanRequest {
    /// Unique id.
    pub id: HumanRequestId,
    /// The case that asked.
    pub case_id: CaseId,
    /// The question.
    pub question: String,
    /// Current status.
    pub status: HumanRequestStatus,
    /// The answer, once answered.
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
