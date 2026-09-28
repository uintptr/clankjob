//! Wait conditions: what a sleeping case is waiting for (design §6).

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{CaseId, WaitConditionId};

/// Built-in condition that fires at a point in time.
pub const TIMER_KIND: &str = "core.timer";

/// Built-in condition that fires when the case's open human request is answered.
///
/// The scheduler never polls it; only answering the request fires it.
pub const HUMAN_INPUT_KIND: &str = "core.human_input";

string_enum!(
    /// Lifecycle of a wait condition.
    WaitStatus {
        /// Still waiting.
        Active => "active",
        /// The condition happened.
        Fired => "fired",
        /// The deadline passed first.
        TimedOut => "timed_out",
        /// Another condition of the same case fired, or the case was woken otherwise.
        Cancelled => "cancelled",
    }
);

/// A condition as requested by the LLM through `sleep` or `ask_human`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitConditionSpec {
    /// Condition kind, e.g. `core.timer`.
    pub kind: String,
    /// Kind-specific parameters.
    #[serde(default)]
    pub params: Value,
    /// How often to check a polled condition, e.g. `"1h"`.
    #[serde(default, with = "humantime_serde")]
    pub check_every: Option<Duration>,
    /// Give up waiting after this long, e.g. `"3d"`.
    #[serde(default, with = "humantime_serde")]
    pub timeout: Option<Duration>,
}

/// A registered wait condition as stored.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WaitCondition {
    /// Unique id.
    pub id: WaitConditionId,
    /// The sleeping case.
    pub case_id: CaseId,
    /// Condition kind.
    pub kind: String,
    /// Kind-specific parameters.
    pub params: Value,
    /// When the scheduler should next evaluate it; `None` if it is never polled.
    pub next_check_at: Option<DateTime<Utc>>,
    /// When it times out; `None` for no timeout.
    pub deadline_at: Option<DateTime<Utc>>,
    /// Current status.
    pub status: WaitStatus,
    /// Registration time.
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_parses_human_readable_durations() {
        let spec: WaitConditionSpec =
            serde_json::from_str(r#"{"kind": "core.timer", "params": {"after": "2h"}, "timeout": "3d"}"#).unwrap();

        assert_eq!(spec.kind, TIMER_KIND);
        assert_eq!(spec.timeout, Some(Duration::from_hours(72)));
        assert_eq!(spec.check_every, None);
    }

    #[test]
    fn spec_rejects_unknown_fields() {
        let result = serde_json::from_str::<WaitConditionSpec>(r#"{"kind": "core.timer", "every": "1h"}"#);

        assert!(result.is_err());
    }
}
