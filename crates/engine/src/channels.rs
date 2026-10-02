//! Human channels (design §10.3): questions and notifications go out through an outbox,
//! and answers are polled back.
//!
//! Messages are queued in `channel_deliveries` in the same transaction as the question or
//! state change they are about, so a crash never loses one. The channel thread sends
//! them, retrying with backoff, and polls each channel for answers to its open
//! questions. An answer goes through the same path as one from the web UI: the first
//! answer wins.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use clankjob_core::case::{Case, CaseState};
use clankjob_core::channel::{ChannelDelivery, ChannelError, ChannelReply, DeliveryKind, DeliveryStatus, HumanChannel};
use clankjob_core::human::Decision;
use clankjob_core::human::HumanRequestStatus;
use clankjob_core::ids::{CaseId, HumanRequestId};
use clankjob_storage::human::{Answer, Verdict};
use clankjob_storage::{self as storage, Connection};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{EngineError, Result, Shared, later, transitions};

/// Messages sent per pass of the channel thread.
const DISPATCH_BATCH: u32 = 50;
/// Attempts before a message is given up.
const MAX_ATTEMPTS: u32 = 8;
/// Delay before the first retry; doubles each time.
const RETRY_BASE: Duration = Duration::from_secs(10);
/// Longest delay between retries.
const RETRY_MAX: Duration = Duration::from_mins(15);
/// How long the channel thread sleeps when nothing wakes it.
const IDLE_WAIT: Duration = Duration::from_secs(2);

/// The channels in use. Swapped as a whole when plugins are reloaded.
#[derive(Clone, Default)]
struct ChannelSet {
    instances: BTreeMap<String, Arc<dyn HumanChannel>>,
    default: Vec<String>,
}

/// How a channel has been doing since the server started, for the API.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelActivity {
    /// Last successful call (message sent or poll).
    pub last_ok_at: Option<DateTime<Utc>>,
    /// Last failed call.
    pub last_error: Option<String>,
    /// When it failed.
    pub last_error_at: Option<DateTime<Utc>>,
    /// Last warning the channel reported, e.g. a missing permission.
    pub last_warning: Option<String>,
    /// When it was reported.
    pub last_warning_at: Option<DateTime<Utc>>,
}

impl ChannelActivity {
    /// Whether the latest news is bad: an error newer than the last success.
    #[must_use]
    pub fn failing(&self) -> bool {
        match (self.last_error_at, self.last_ok_at) {
            (Some(error), Some(ok)) => error > ok,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
}

/// The configured human channels.
#[derive(Default)]
pub struct Channels {
    set: RwLock<ChannelSet>,
    public_url: Option<String>,
    activity: Mutex<BTreeMap<String, ChannelActivity>>,
}

/// A configured channel, as listed by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelInfo {
    /// Instance name.
    pub name: String,
    /// Plugin providing it.
    pub plugin: String,
    /// Whether new cases use it unless they choose otherwise.
    pub default: bool,
}

impl Channels {
    /// Channels to start with.
    ///
    /// # Arguments
    ///
    /// * `instances` - Channel instances by name, e.g. `discord_joe`
    /// * `default` - Channels used by cases that don't name their own
    /// * `public_url` - Where people reach the server, for links back to a case
    #[must_use]
    pub fn new(
        instances: BTreeMap<String, Arc<dyn HumanChannel>>,
        default: Vec<String>,
        public_url: Option<String>,
    ) -> Self {
        Self {
            set: RwLock::new(ChannelSet { instances, default }),
            public_url,
            activity: Mutex::default(),
        }
    }

    /// Swap in a new set of channels, e.g. after plugins were reloaded. Messages already
    /// queued for a channel that disappeared are retried, in case it comes back.
    pub fn replace(&self, instances: BTreeMap<String, Arc<dyn HumanChannel>>, default: Vec<String>) {
        *self.set.write().unwrap_or_else(PoisonError::into_inner) = ChannelSet { instances, default };
    }

    fn set(&self) -> ChannelSet {
        self.set.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn get(&self, name: &str) -> Option<Arc<dyn HumanChannel>> {
        self.set
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .instances
            .get(name)
            .map(Arc::clone)
    }

    /// The configured channels, by name.
    #[must_use]
    pub fn list(&self) -> Vec<ChannelInfo> {
        let set = self.set();
        set.instances
            .iter()
            .map(|(name, channel)| ChannelInfo {
                name: name.clone(),
                plugin: channel.plugin().to_owned(),
                default: set.default.contains(name),
            })
            .collect()
    }

    /// How a channel has been doing since the server started.
    #[must_use]
    pub fn activity(&self, name: &str) -> ChannelActivity {
        self.activity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    fn record<F>(&self, name: &str, update: F)
    where
        F: FnOnce(&mut ChannelActivity),
    {
        let mut activity = self.activity.lock().unwrap_or_else(PoisonError::into_inner);
        update(activity.entry(name.to_owned()).or_default());
    }

    fn record_ok(&self, name: &str) {
        self.record(name, |activity| activity.last_ok_at = Some(Utc::now()));
    }

    fn record_error(&self, name: &str, error: &str) {
        self.record(name, |activity| {
            activity.last_error = Some(error.to_owned());
            activity.last_error_at = Some(Utc::now());
        });
    }

    fn record_warning(&self, name: &str, warning: &str) {
        self.record(name, |activity| {
            activity.last_warning = Some(warning.to_owned());
            activity.last_warning_at = Some(Utc::now());
        });
    }

    /// The channels a new case uses: the ones it names, or the default.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::UnknownChannel`] for a name that is not configured.
    pub fn resolve(&self, wanted: Option<&[String]>) -> Result<Vec<String>> {
        let set = self.set();
        let Some(wanted) = wanted else {
            return Ok(set.default);
        };
        let mut resolved: Vec<String> = Vec::with_capacity(wanted.len());
        for name in wanted {
            if !set.instances.contains_key(name) {
                return Err(EngineError::UnknownChannel(name.clone()));
            }
            if !resolved.contains(name) {
                resolved.push(name.clone());
            }
        }
        Ok(resolved)
    }

    /// Link to a case in the web UI, when the public URL is known.
    pub(crate) fn case_url(&self, case_id: &CaseId) -> Option<String> {
        self.public_url
            .as_deref()
            .map(|url| format!("{}/#/cases/{case_id}", url.trim_end_matches('/')))
    }
}

// ------------------------------------------------------------------ queueing

/// Queue a new question for every channel of its case.
pub(crate) fn queue_question(
    connection: &Connection,
    case: &Case,
    request_id: &HumanRequestId,
    question: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let payload = json!({ "kind": "question", "case_title": case.title, "text": question });
    queue_request(connection, case, request_id, &payload, now)
}

/// Queue a new approval for every channel of its case: what the call will do, and its
/// arguments as details.
pub(crate) fn queue_approval(
    connection: &Connection,
    case: &Case,
    request_id: &HumanRequestId,
    summary: &str,
    args: &Value,
    now: DateTime<Utc>,
) -> Result<()> {
    let payload = json!({ "kind": "approval", "case_title": case.title, "text": summary, "details": args });
    queue_request(connection, case, request_id, &payload, now)
}

fn queue_request(
    connection: &Connection,
    case: &Case,
    request_id: &HumanRequestId,
    payload: &Value,
    now: DateTime<Utc>,
) -> Result<()> {
    for channel in &case.human_channels {
        storage::channels::insert_delivery(
            connection,
            &case.id,
            Some(request_id),
            channel,
            DeliveryKind::Request,
            payload,
            now,
        )?;
    }
    Ok(())
}

/// Queue an update for every channel a question was posted to, now that it is settled.
pub(crate) fn queue_resolution(
    connection: &Connection,
    case_id: &CaseId,
    request_id: &HumanRequestId,
    outcome: &Value,
    now: DateTime<Utc>,
) -> Result<()> {
    for channel in storage::channels::request_channels(connection, request_id)? {
        storage::channels::insert_delivery(
            connection,
            case_id,
            Some(request_id),
            &channel,
            DeliveryKind::Resolution,
            outcome,
            now,
        )?;
    }
    Ok(())
}

/// Close a case's open questions without an answer, and queue the channel updates.
pub(crate) fn close_requests(
    connection: &Connection,
    case_id: &CaseId,
    status: HumanRequestStatus,
    now: DateTime<Utc>,
) -> Result<()> {
    let open = storage::human::list_requests(connection, Some(HumanRequestStatus::Open), Some(case_id))?;
    storage::human::close_open_requests(connection, case_id, status, now)?;
    let outcome = json!({ "status": status });
    for request in open {
        queue_resolution(connection, case_id, &request.id, &outcome, now)?;
    }
    Ok(())
}

/// Queue a notification for every channel of a case that just finished.
pub(crate) fn queue_finished(connection: &Connection, case_id: &CaseId, now: DateTime<Utc>) -> Result<()> {
    let case = transitions::load_case(connection, case_id)?;
    let outcome = case.outcome.as_deref().unwrap_or_default();
    let (event, text) = match case.state {
        CaseState::Completed => ("completed", format!("Completed. {outcome}")),
        CaseState::Failed if outcome.starts_with("budget exceeded") => {
            ("budget_exceeded", format!("Stopped: {outcome}"))
        }
        CaseState::Failed => ("failed", format!("Failed: {outcome}")),
        _ => return Ok(()),
    };
    let payload = json!({ "event": event, "case_title": case.title, "text": text.trim_end() });
    for channel in &case.human_channels {
        storage::channels::insert_delivery(
            connection,
            &case.id,
            None,
            channel,
            DeliveryKind::Notification,
            &payload,
            now,
        )?;
    }
    Ok(())
}

// ------------------------------------------------------------------ the channel thread

/// What happened to one queued message.
enum Outcome {
    Sent(Option<Value>),
    Skipped,
    Failed(ChannelError),
}

/// Send queued messages and poll channels for answers until shutdown.
pub(crate) fn channel_loop(shared: &Shared) {
    let mut connection = match shared.db.connect() {
        Ok(connection) => connection,
        Err(error) => {
            tracing::error!(%error, "channel thread cannot open the database");
            return;
        }
    };
    let mut next_poll: HashMap<String, Instant> = HashMap::new();
    while !shared.is_shutting_down() {
        let seen = shared.signal.generation();
        if let Err(error) = dispatch(shared, &connection) {
            tracing::error!(%error, "sending channel messages failed");
        }
        for (name, channel) in &shared.channels.set().instances {
            if shared.is_shutting_down() {
                break;
            }
            let now = Instant::now();
            if next_poll.get(name).is_some_and(|due| *due > now) {
                continue;
            }
            if let Err(error) = poll(shared, &mut connection, name, channel.as_ref()) {
                tracing::error!(channel = %name, %error, "polling the channel failed");
            }
            let due = Instant::now().checked_add(channel.poll_interval()).unwrap_or(now);
            next_poll.insert(name.clone(), due);
        }
        shared.signal.wait(seen, IDLE_WAIT);
    }
}

/// Add the link back to the case to a payload.
fn with_case_url(channels: &Channels, delivery: &ChannelDelivery) -> Value {
    let mut payload = delivery.payload.clone();
    if let (Some(url), Some(object)) = (channels.case_url(&delivery.case_id), payload.as_object_mut()) {
        object.insert("case_url".to_owned(), Value::String(url));
    }
    payload
}

/// Try to send one queued message.
fn send(shared: &Shared, connection: &Connection, delivery: &ChannelDelivery) -> Result<Outcome> {
    let Some(channel) = shared.channels.get(&delivery.channel) else {
        // Retried: the channel may be back after its plugin is fixed and reloaded.
        return Ok(Outcome::Failed(ChannelError::retryable(format!(
            "channel `{}` is not loaded",
            delivery.channel
        ))));
    };
    let Some(request_id) = &delivery.human_request_id else {
        // Only notifications are about no particular question.
        let event = delivery.payload.get("event").and_then(Value::as_str).unwrap_or_default();
        if delivery.kind != DeliveryKind::Notification || !channel.notifies(event) {
            return Ok(Outcome::Skipped);
        }
        return Ok(match channel.notify(&with_case_url(&shared.channels, delivery)) {
            Ok(()) => Outcome::Sent(None),
            Err(error) => Outcome::Failed(error),
        });
    };
    Ok(match delivery.kind {
        DeliveryKind::Request => {
            let open = storage::human::get_request(connection, request_id)?
                .is_some_and(|request| request.status == HumanRequestStatus::Open);
            if !open {
                // Answered or superseded before it could be posted.
                return Ok(Outcome::Skipped);
            }
            match channel.deliver(&with_case_url(&shared.channels, delivery)) {
                Ok(external) => Outcome::Sent(Some(external)),
                Err(error) => Outcome::Failed(error),
            }
        }
        DeliveryKind::Resolution => {
            match storage::channels::request_delivery(connection, request_id, &delivery.channel)? {
                Some(posted) if posted.status == DeliveryStatus::Sent => {
                    match channel.on_resolved(&posted.external.unwrap_or(Value::Null), &delivery.payload) {
                        Ok(()) => Outcome::Sent(None),
                        Err(error) => Outcome::Failed(error),
                    }
                }
                // Never posted, so there is nothing to update.
                _ => Outcome::Skipped,
            }
        }
        DeliveryKind::Notification => Outcome::Skipped,
    })
}

/// Delay before retry number `attempt` (0-based).
fn retry_delay(attempt: u32) -> Duration {
    RETRY_BASE.saturating_mul(2_u32.saturating_pow(attempt)).min(RETRY_MAX)
}

/// Send the messages that are due.
fn dispatch(shared: &Shared, connection: &Connection) -> Result<()> {
    for delivery in storage::channels::due_deliveries(connection, Utc::now(), DISPATCH_BATCH)? {
        if shared.is_shutting_down() {
            break;
        }
        let outcome = send(shared, connection, &delivery)?;
        let now = Utc::now();
        match outcome {
            Outcome::Sent(external) => {
                shared.channels.record_ok(&delivery.channel);
                storage::channels::mark_sent(connection, &delivery.id, external.as_ref(), now)?;
                tracing::info!(channel = %delivery.channel, case_id = %delivery.case_id, kind = %delivery.kind, "channel message sent");
            }
            Outcome::Skipped => {
                storage::channels::mark_finished(connection, &delivery.id, DeliveryStatus::Skipped, None, now)?;
            }
            Outcome::Failed(error) if error.retryable && delivery.attempts.saturating_add(1) < MAX_ATTEMPTS => {
                shared.channels.record_error(&delivery.channel, &error.message);
                let retry_at = later(now, retry_delay(delivery.attempts));
                tracing::warn!(channel = %delivery.channel, case_id = %delivery.case_id, %error, %retry_at, "channel message failed; will retry");
                storage::channels::mark_retry(connection, &delivery.id, &error.message, retry_at, now)?;
            }
            Outcome::Failed(error) => {
                shared.channels.record_error(&delivery.channel, &error.message);
                tracing::error!(channel = %delivery.channel, case_id = %delivery.case_id, %error, "channel message failed; giving up");
                let status = DeliveryStatus::Failed;
                storage::channels::mark_finished(connection, &delivery.id, status, Some(&error.message), now)?;
            }
        }
    }
    Ok(())
}

/// The answer text for a reply, or `None` if it carries none (e.g. an approval decision).
fn reply_text(channel_name: &str, reply: &ChannelReply) -> Option<String> {
    let text = reply.text.as_deref().map(str::trim).unwrap_or_default();
    if reply.attachments.is_empty() {
        return (!text.is_empty()).then(|| text.to_owned());
    }
    let names: Vec<&str> = reply
        .attachments
        .iter()
        .filter_map(|attachment| attachment.get("filename").and_then(Value::as_str))
        .collect();
    let note = format!(
        "({} attachment(s) sent on {channel_name} were not imported: {}. Ask the owner to upload them in the web UI if needed.)",
        reply.attachments.len(),
        names.join(", ")
    );
    Some(if text.is_empty() {
        note
    } else {
        format!("{text}\n\n{note}")
    })
}

/// Look for answers on one channel and record them.
fn poll(shared: &Shared, connection: &mut Connection, name: &str, channel: &dyn HumanChannel) -> Result<()> {
    let open = storage::channels::open_deliveries(connection, name)?;
    if open.is_empty() {
        return Ok(());
    }
    let cursor = storage::channels::get_cursor(connection, name)?;
    let result = match channel.poll(&open, &cursor) {
        Ok(result) => result,
        Err(error) => {
            shared.channels.record_error(name, &error.message);
            tracing::warn!(channel = %name, %error, "polling for answers failed");
            return Ok(());
        }
    };
    shared.channels.record_ok(name);
    for warning in &result.warnings {
        shared.channels.record_warning(name, warning);
        tracing::warn!(channel = %name, warning = %warning, "channel warning");
    }
    for reply in &result.replies {
        // The plugin already filters, but a channel answer carries the owner's authority,
        // so it is checked again here.
        if !open.iter().any(|delivery| delivery.request_id == reply.request_id) {
            tracing::warn!(channel = %name, request_id = %reply.request_id, "reply to a question that is not open here; ignored");
            continue;
        }
        if !channel.allowed_responders().contains(&reply.responder) {
            tracing::warn!(channel = %name, responder = %reply.responder, "reply from someone not allowed to answer; ignored");
            continue;
        }
        let outcome = match (reply.decision.as_deref(), reply_text(name, reply)) {
            (Some(decision), _) => {
                let decision = match decision {
                    "approve" => Decision::Approve,
                    "reject" => Decision::Reject,
                    other => {
                        tracing::warn!(channel = %name, decision = other, "unknown decision; ignored");
                        continue;
                    }
                };
                let verdict = Verdict {
                    decision,
                    args: None,
                    comment: None,
                    via: name,
                    responder: Some(&reply.responder),
                };
                transitions::decide_approval(connection, &reply.request_id, verdict, Utc::now())
            }
            (None, Some(text)) => {
                let answer = Answer {
                    text: &text,
                    via: name,
                    responder: Some(&reply.responder),
                };
                transitions::answer_request(connection, &reply.request_id, answer, Utc::now())
            }
            (None, None) => {
                tracing::warn!(channel = %name, request_id = %reply.request_id, "reply without text or decision; ignored");
                continue;
            }
        };
        match outcome {
            Ok(_) => {
                tracing::info!(channel = %name, request_id = %reply.request_id, "request answered from the channel");
                shared.signal.notify();
            }
            Err(EngineError::NotAQuestion(_) | EngineError::NotAnApproval(_)) => {
                tracing::warn!(channel = %name, request_id = %reply.request_id, "reply does not fit the request (text for an approval, or a decision for a question); ignored");
            }
            Err(
                EngineError::AlreadyResolved(_) | EngineError::RequestNotFound(_) | EngineError::InvalidState { .. },
            ) => {
                tracing::debug!(channel = %name, request_id = %reply.request_id, "question already settled");
            }
            Err(error) => return Err(error),
        }
    }
    storage::channels::set_cursor(connection, name, &result.cursor, Utc::now())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use clankjob_core::case::{Budgets, NewCase};
    use clankjob_core::channel::{OpenDelivery, PollResult};
    use clankjob_core::llm::{AssistantMessage, CompletionResponse, TokenUsage, ToolCall};

    use super::*;
    use crate::test_support::{ScriptedProvider, TestDb, engine_with_channels};

    /// A channel that records calls and answers polls from a script. Only user `42` may
    /// answer.
    struct FakeChannel {
        allowed: Vec<String>,
        calls: Mutex<Vec<(String, Value)>>,
        replies: Mutex<Vec<ChannelReply>>,
        fail_deliver: Mutex<Option<ChannelError>>,
    }

    impl FakeChannel {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                allowed: vec!["42".to_owned()],
                calls: Mutex::default(),
                replies: Mutex::default(),
                fail_deliver: Mutex::default(),
            })
        }

        fn calls(&self, method: &str) -> Vec<Value> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| name == method)
                .map(|(_, value)| value.clone())
                .collect()
        }

        fn record(&self, method: &str, value: Value) {
            self.calls.lock().unwrap().push((method.to_owned(), value));
        }
    }

    impl HumanChannel for FakeChannel {
        fn plugin(&self) -> &'static str {
            "fake"
        }

        fn allowed_responders(&self) -> &[String] {
            &self.allowed
        }

        fn poll_interval(&self) -> Duration {
            Duration::from_millis(10)
        }

        fn notifies(&self, event: &str) -> bool {
            event != "budget_exceeded"
        }

        fn deliver(&self, request: &Value) -> std::result::Result<Value, ChannelError> {
            self.record("deliver", request.clone());
            if let Some(error) = self.fail_deliver.lock().unwrap().take() {
                return Err(error);
            }
            Ok(json!({ "thread_id": "900" }))
        }

        fn poll(&self, open: &[OpenDelivery], cursor: &Value) -> std::result::Result<PollResult, ChannelError> {
            self.record("poll", json!({ "open": open, "cursor": cursor }));
            Ok(PollResult {
                replies: std::mem::take(&mut *self.replies.lock().unwrap()),
                cursor: json!({ "900": "901" }),
                warnings: Vec::new(),
            })
        }

        fn on_resolved(&self, delivery: &Value, outcome: &Value) -> std::result::Result<(), ChannelError> {
            self.record("on_resolved", json!({ "delivery": delivery, "outcome": outcome }));
            Ok(())
        }

        fn notify(&self, notification: &Value) -> std::result::Result<(), ChannelError> {
            self.record("notify", notification.clone());
            Ok(())
        }
    }

    fn call(name: &str, arguments: Value) -> CompletionResponse {
        CompletionResponse {
            message: AssistantMessage {
                text: None,
                tool_calls: vec![ToolCall {
                    id: format!("call_{name}"),
                    name: name.to_owned(),
                    arguments,
                }],
            },
            usage: TokenUsage::default(),
        }
    }

    fn new_case(channels: Option<Vec<String>>) -> NewCase {
        NewCase {
            title: "Electrician".to_owned(),
            goal: "Get a quote".to_owned(),
            owner: None,
            profile: None,
            llm: "default".to_owned(),
            model: None,
            budgets: Budgets::default(),
            instructions: Vec::new(),
            human_channels: channels,
            approvals: clankjob_core::case::ApprovalPolicy::default(),
        }
    }

    fn wait_until<F>(mut condition: F)
    where
        F: FnMut() -> bool,
    {
        let deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
        while !condition() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_question_is_posted_answered_from_the_channel_and_the_case_notified() {
        // Arrange
        let test_db = TestDb::new();
        let provider = ScriptedProvider::new([
            Ok(call("ask_human", json!({ "question": "Send a panel photo?" }))),
            Ok(call("complete", json!({ "summary": "Quote is $1,450." }))),
        ]);
        let channel = FakeChannel::new();
        let engine = engine_with_channels(&test_db, provider, Arc::clone(&channel) as Arc<dyn HumanChannel>);
        let mut connection = test_db.connect();
        let case = engine.create_case(&mut connection, &new_case(None)).unwrap();
        let handles = engine.start().unwrap();

        // Act
        wait_until(|| !channel.calls("deliver").is_empty());
        let request = storage::human::list_requests(&connection, Some(HumanRequestStatus::Open), Some(&case.id))
            .unwrap()
            .remove(0);
        channel.replies.lock().unwrap().push(ChannelReply {
            request_id: request.id.clone(),
            external_id: "901".to_owned(),
            responder: "42".to_owned(),
            text: Some("Sent it.".to_owned()),
            decision: None,
            attachments: Vec::new(),
        });
        wait_until(|| !channel.calls("notify").is_empty());
        engine.shutdown();
        for handle in handles {
            handle.join().unwrap();
        }

        // Assert
        assert_eq!(
            channel.calls("deliver")[0],
            json!({
                "kind": "question",
                "case_title": "Electrician",
                "text": "Send a panel photo?",
                "case_url": format!("https://clank.example/#/cases/{}", case.id),
            })
        );
        let answered = storage::human::get_request(&connection, &request.id).unwrap().unwrap();
        assert_eq!(answered.answer.as_deref(), Some("Sent it."));
        assert_eq!(answered.answered_via.as_deref(), Some("discord_joe"));
        assert_eq!(answered.responder.as_deref(), Some("42"));
        let resolved = channel.calls("on_resolved");
        assert_eq!(
            resolved[0],
            json!({
                "delivery": { "thread_id": "900" },
                "outcome": { "status": "answered", "via": "discord_joe", "responder": "42" },
            })
        );
        assert_eq!(channel.calls("notify")[0]["text"], "Completed. Quote is $1,450.");
        let activity = engine.channels().activity("discord_joe");
        assert!(activity.last_ok_at.is_some() && !activity.failing());
        assert_eq!(
            storage::channels::get_cursor(&connection, "discord_joe").unwrap(),
            json!({ "900": "901" })
        );
    }

    #[test]
    fn replies_from_strangers_are_ignored_and_failed_deliveries_retried() {
        // Arrange
        let test_db = TestDb::new();
        let provider = ScriptedProvider::new([Ok(call("ask_human", json!({ "question": "Proceed?" })))]);
        let channel = FakeChannel::new();
        *channel.fail_deliver.lock().unwrap() = Some(ChannelError::retryable("rate limited"));
        let engine = engine_with_channels(&test_db, provider, Arc::clone(&channel) as Arc<dyn HumanChannel>);
        let mut connection = test_db.connect();
        let case = engine.create_case(&mut connection, &new_case(None)).unwrap();
        let handles = engine.start().unwrap();
        wait_until(|| !channel.calls("deliver").is_empty());
        let request = storage::human::list_requests(&connection, Some(HumanRequestStatus::Open), Some(&case.id))
            .unwrap()
            .remove(0);

        // Act
        let deliveries = storage::channels::list_deliveries(&connection, &case.id).unwrap();
        storage::channels::mark_retry(&connection, &deliveries[0].id, "again", Utc::now(), Utc::now()).unwrap();
        wait_until(|| channel.calls("deliver").len() >= 2);
        channel.replies.lock().unwrap().push(ChannelReply {
            request_id: request.id.clone(),
            external_id: "5".to_owned(),
            responder: "666".to_owned(),
            text: Some("Yes!".to_owned()),
            decision: None,
            attachments: Vec::new(),
        });
        wait_until(|| channel.calls("poll").len() >= 2);
        engine.shutdown();
        for handle in handles {
            handle.join().unwrap();
        }

        // Assert
        let still_open = storage::human::get_request(&connection, &request.id).unwrap().unwrap();
        assert_eq!(still_open.status, HumanRequestStatus::Open);
        let sent = storage::channels::request_delivery(&connection, &request.id, "discord_joe")
            .unwrap()
            .unwrap();
        assert_eq!(sent.status, DeliveryStatus::Sent);
    }

    #[test]
    fn unknown_channels_are_rejected_and_the_default_applies() {
        // Arrange
        let test_db = TestDb::new();
        let channel: Arc<dyn HumanChannel> = FakeChannel::new();
        let engine = engine_with_channels(&test_db, ScriptedProvider::new([]), channel);
        let mut connection = test_db.connect();

        // Act
        let unknown = engine.create_case(&mut connection, &new_case(Some(vec!["slack".to_owned()])));
        let none = engine.create_case(&mut connection, &new_case(Some(Vec::new()))).unwrap();
        let default = engine.create_case(&mut connection, &new_case(None)).unwrap();

        // Assert
        assert!(matches!(unknown, Err(EngineError::UnknownChannel(name)) if name == "slack"));
        assert_eq!(none.human_channels, [] as [std::string::String; 0]);
        assert_eq!(default.human_channels, ["discord_joe"]);
    }

    #[test]
    fn the_owner_turns_a_cases_channels_on_and_off() {
        // Arrange: a case on the web client only
        let test_db = TestDb::new();
        let channel: Arc<dyn HumanChannel> = FakeChannel::new();
        let engine = engine_with_channels(&test_db, ScriptedProvider::new([]), channel);
        let mut connection = test_db.connect();
        let case = engine.create_case(&mut connection, &new_case(Some(Vec::new()))).unwrap();

        // Act
        let on = engine
            .set_human_channels(
                &mut connection,
                &case.id,
                &["discord_joe".to_owned(), "discord_joe".to_owned()],
            )
            .unwrap();
        let unknown = engine.set_human_channels(&mut connection, &case.id, &["slack".to_owned()]);
        let off = engine.set_human_channels(&mut connection, &case.id, &[]).unwrap();

        // Assert
        assert_eq!(on.human_channels, ["discord_joe"]);
        assert!(matches!(unknown, Err(EngineError::UnknownChannel(name)) if name == "slack"));
        assert_eq!(off.human_channels, [] as [std::string::String; 0]);
    }

    #[test]
    fn channels_can_be_swapped_and_activity_says_when_one_is_failing() {
        // Arrange
        let channels = Channels::new(BTreeMap::new(), Vec::new(), None);
        let channel: Arc<dyn HumanChannel> = FakeChannel::new();

        // Act
        let before = channels.resolve(Some(&["discord_joe".to_owned()]));
        channels.replace(
            BTreeMap::from([("discord_joe".to_owned(), channel)]),
            vec!["discord_joe".to_owned()],
        );
        channels.record_ok("discord_joe");
        channels.record_error("discord_joe", "401 Unauthorized");

        // Assert
        assert!(matches!(before, Err(EngineError::UnknownChannel(_))));
        assert_eq!(channels.resolve(None).unwrap(), ["discord_joe"]);
        assert_eq!(channels.list()[0].plugin, "fake");
        let activity = channels.activity("discord_joe");
        assert!(activity.failing());
        assert_eq!(activity.last_error.as_deref(), Some("401 Unauthorized"));
        assert!(!channels.activity("other").failing());
    }

    #[test]
    fn attachments_are_mentioned_in_the_answer() {
        let reply = ChannelReply {
            request_id: HumanRequestId::generate(),
            external_id: "1".to_owned(),
            responder: "42".to_owned(),
            text: Some(String::new()),
            decision: None,
            attachments: vec![json!({ "filename": "panel.jpg" })],
        };

        let text = reply_text("discord_joe", &reply).unwrap();

        assert!(text.starts_with("(1 attachment(s) sent on discord_joe were not imported: panel.jpg."));
        assert_eq!(retry_delay(0), RETRY_BASE);
        assert_eq!(retry_delay(20), RETRY_MAX);
    }
}
