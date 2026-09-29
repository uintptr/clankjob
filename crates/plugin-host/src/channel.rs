//! A human channel instance served by an external plugin (design §10.3).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use clankjob_core::channel::{ChannelError, HumanChannel, OpenDelivery, PollResult};
use serde_json::{Map, Value, json};

use crate::process::ProcessPlugin;

/// Timeout of `poll`, which may make a couple of API calls per open question.
const POLL_TIMEOUT: Duration = Duration::from_mins(1);
/// Timeout of every other call.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Case events a channel posts about unless configured otherwise.
pub const DEFAULT_NOTIFY_ON: &[&str] = &["completed", "failed", "budget_exceeded"];

/// One configured instance of a channel plugin, e.g. `discord_joe`.
pub struct ProcessChannel {
    pub(crate) instance: String,
    pub(crate) plugin: Arc<ProcessPlugin>,
    /// Resolved configuration. Holds secrets: never logged or returned.
    pub(crate) config: Value,
    pub(crate) allowed_responders: Vec<String>,
    pub(crate) poll_interval: Duration,
    pub(crate) notify_on: Vec<String>,
}

impl fmt::Debug for ProcessChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `config` holds secrets, so it is left out.
        formatter
            .debug_struct("ProcessChannel")
            .field("instance", &self.instance)
            .field("plugin", &self.plugin.id())
            .field("allowed_responders", &self.allowed_responders)
            .field("poll_interval", &self.poll_interval)
            .field("notify_on", &self.notify_on)
            .finish_non_exhaustive()
    }
}

impl ProcessChannel {
    /// Call the plugin for this instance: `instance` and `config` are added to `params`.
    fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, ChannelError> {
        let mut object = match params {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        object.insert("instance".to_owned(), Value::String(self.instance.clone()));
        object.insert("config".to_owned(), self.config.clone());
        self.plugin.call(method, &Value::Object(object), timeout)
    }

    /// Check the configuration against the service (e.g. the token works).
    ///
    /// # Returns
    ///
    /// What the plugin reported, e.g. the bot and channel names
    ///
    /// # Errors
    ///
    /// Returns the plugin's error.
    pub fn validate(&self) -> Result<Value, ChannelError> {
        self.call("validate_config", json!({}), CALL_TIMEOUT)
    }
}

impl HumanChannel for ProcessChannel {
    fn plugin(&self) -> &str {
        self.plugin.id()
    }

    fn allowed_responders(&self) -> &[String] {
        &self.allowed_responders
    }

    fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    fn notifies(&self, event: &str) -> bool {
        self.notify_on.iter().any(|wanted| wanted == event)
    }

    fn deliver(&self, request: &Value) -> Result<Value, ChannelError> {
        let result = self.call("deliver", json!({ "request": request }), CALL_TIMEOUT)?;
        result
            .get("delivery")
            .cloned()
            .ok_or_else(|| ChannelError::fatal("`deliver` returned no `delivery`"))
    }

    fn poll(&self, open: &[OpenDelivery], cursor: &Value) -> Result<PollResult, ChannelError> {
        let result = self.call("poll", json!({ "open": open, "cursor": cursor }), POLL_TIMEOUT)?;
        serde_json::from_value(result).map_err(|error| ChannelError::fatal(format!("invalid `poll` result: {error}")))
    }

    fn on_resolved(&self, delivery: &Value, outcome: &Value) -> Result<(), ChannelError> {
        self.call(
            "on_resolved",
            json!({ "delivery": delivery, "outcome": outcome }),
            CALL_TIMEOUT,
        )
        .map(|_| ())
    }

    fn notify(&self, notification: &Value) -> Result<(), ChannelError> {
        self.call("notify", json!({ "notification": notification }), CALL_TIMEOUT)
            .map(|_| ())
    }
}
