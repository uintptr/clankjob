//! Human channels: where questions are delivered and answered besides the web UI
//! (design §10.3). Discord is the first one.
//!
//! The engine only knows the [`HumanChannel`] trait. Payloads are JSON because they are
//! passed through unchanged to the channel's plugin, which is usually an external process.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{CaseId, DeliveryId, HumanRequestId};

string_enum!(
    /// What a queued channel message is for.
    DeliveryKind {
        /// A question to post; answers come back through [`HumanChannel::poll`].
        Request => "request",
        /// An informational message (case completed, failed, …).
        Notification => "notification",
        /// Mark an earlier request message as answered, superseded or cancelled.
        Resolution => "resolution",
    }
);

string_enum!(
    /// Progress of a queued channel message.
    DeliveryStatus {
        /// Not sent yet, or waiting to be retried.
        Pending => "pending",
        /// Sent.
        Sent => "sent",
        /// Gave up after an error.
        Failed => "failed",
        /// No longer needed, e.g. the question was answered before it could be posted.
        Skipped => "skipped",
    }
);

/// A message queued for a human channel (the `channel_deliveries` outbox).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChannelDelivery {
    /// Unique id.
    pub id: DeliveryId,
    /// The case it is about.
    pub case_id: CaseId,
    /// The question it posts or resolves, if any.
    pub human_request_id: Option<HumanRequestId>,
    /// Channel instance name, e.g. `discord_joe`.
    pub channel: String,
    /// What it is for.
    pub kind: DeliveryKind,
    /// What to send, as given to the channel.
    pub payload: Value,
    /// Progress.
    pub status: DeliveryStatus,
    /// What the channel returned once sent (e.g. Discord message and thread ids).
    pub external: Option<Value>,
    /// Failed attempts so far.
    pub attempts: u32,
    /// When to try next while pending.
    pub next_attempt_at: DateTime<Utc>,
    /// Last error, if any.
    pub last_error: Option<String>,
    /// When it was queued.
    pub created_at: DateTime<Utc>,
}

/// A failed channel call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ChannelError {
    /// What went wrong. Never contains secrets.
    pub message: String,
    /// Whether trying again later can help (network, rate limit, restarting plugin).
    pub retryable: bool,
}

impl ChannelError {
    /// An error worth retrying.
    #[must_use]
    pub fn retryable<S>(message: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    /// An error that will not go away by itself.
    #[must_use]
    pub fn fatal<S>(message: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

/// A request still open on a channel, passed to [`HumanChannel::poll`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenDelivery {
    /// The question.
    pub request_id: HumanRequestId,
    /// What [`HumanChannel::deliver`] returned for it.
    pub delivery: Value,
}

/// An answer found by [`HumanChannel::poll`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChannelReply {
    /// The question it answers.
    pub request_id: HumanRequestId,
    /// The channel's id for the reply, e.g. a Discord message id.
    pub external_id: String,
    /// Who answered, as the channel identifies them.
    pub responder: String,
    /// The answer text.
    #[serde(default)]
    pub text: Option<String>,
    /// An approval decision (`approve` / `reject`); approvals are not supported yet.
    #[serde(default)]
    pub decision: Option<String>,
    /// Attachment links sent with the reply.
    #[serde(default)]
    pub attachments: Vec<Value>,
}

/// What [`HumanChannel::poll`] found.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PollResult {
    /// Answers, oldest first.
    #[serde(default)]
    pub replies: Vec<ChannelReply>,
    /// Where to continue next time; stored by the engine and handed back.
    #[serde(default)]
    pub cursor: Value,
    /// Problems worth logging, e.g. a missing permission.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// A configured human channel instance, e.g. `discord_joe`.
///
/// Every method blocks until the channel answers or its timeout passes. None of them may
/// call the LLM.
pub trait HumanChannel: Send + Sync {
    /// Plugin providing the channel, e.g. `discord`.
    fn plugin(&self) -> &str;

    /// Channel user ids whose answers count as the owner's.
    fn allowed_responders(&self) -> &[String];

    /// How often to look for answers.
    fn poll_interval(&self) -> Duration;

    /// Whether to post a notification for a case event (`completed`, `failed`,
    /// `budget_exceeded`).
    fn notifies(&self, event: &str) -> bool;

    /// Post a question.
    ///
    /// # Arguments
    ///
    /// * `request` - `{ kind, case_title, text, case_url }`
    ///
    /// # Returns
    ///
    /// What identifies the posted message, handed back to [`HumanChannel::poll`] and
    /// [`HumanChannel::on_resolved`]
    fn deliver(&self, request: &Value) -> Result<Value, ChannelError>;

    /// Look for answers to the requests still open on this channel.
    fn poll(&self, open: &[OpenDelivery], cursor: &Value) -> Result<PollResult, ChannelError>;

    /// Mark a posted request as settled.
    ///
    /// # Arguments
    ///
    /// * `delivery` - What [`HumanChannel::deliver`] returned
    /// * `outcome` - `{ status, via?, responder? }`
    fn on_resolved(&self, delivery: &Value, outcome: &Value) -> Result<(), ChannelError>;

    /// Post an informational message: `{ case_title, text, case_url }`.
    fn notify(&self, notification: &Value) -> Result<(), ChannelError>;
}
