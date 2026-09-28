//! Core tools every case has (design §5.1): their specs and argument parsing.

use std::time::Duration;

use chrono::{DateTime, Utc};
use clankjob_core::llm::{ToolCall, ToolSpec};
use clankjob_core::wait::{HUMAN_INPUT_KIND, TIMER_KIND, WaitConditionSpec};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Arguments of `sleep`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SleepArgs {
    /// Wake up when any of these fires or times out.
    pub conditions: Vec<WaitConditionSpec>,
    /// Why the case is sleeping (shown in the timeline).
    pub reason: String,
}

/// Arguments of `ask_human`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskHumanArgs {
    /// The question for the owner.
    pub question: String,
    /// Stop waiting for an answer after this long.
    #[serde(default, with = "humantime_serde")]
    pub timeout: Option<Duration>,
    /// Also wake up if any of these fires first.
    #[serde(default)]
    pub also_wait_for: Vec<WaitConditionSpec>,
}

/// Arguments of `complete`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteArgs {
    /// Short summary of what was achieved.
    pub summary: String,
    /// Optional structured result.
    #[serde(default)]
    pub result: Option<Value>,
}

/// Arguments of `fail`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailArgs {
    /// Why the goal cannot be achieved.
    pub reason: String,
}

/// Arguments of `note_set`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteSetArgs {
    /// Note name.
    pub key: String,
    /// Note content.
    pub value: String,
}

/// Arguments of `note_delete`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteDeleteArgs {
    /// Note name.
    pub key: String,
}

/// A parsed call to a core tool.
#[derive(Debug, Clone, PartialEq)]
pub enum CoreTool {
    /// Suspend on wait conditions.
    Sleep(SleepArgs),
    /// Ask the owner a question and suspend.
    AskHuman(AskHumanArgs),
    /// Finish successfully.
    Complete(CompleteArgs),
    /// Finish unsuccessfully.
    Fail(FailArgs),
    /// Create or replace a note.
    NoteSet(NoteSetArgs),
    /// Delete a note.
    NoteDelete(NoteDeleteArgs),
}

fn parse_args<T>(call: &ToolCall) -> Result<T, String>
where
    T: DeserializeOwned,
{
    serde_json::from_value(call.arguments.clone())
        .map_err(|error| format!("invalid arguments for `{}`: {error}", call.name))
}

impl CoreTool {
    /// Parse a tool call requested by the LLM.
    ///
    /// # Errors
    ///
    /// Returns a message for the LLM if the tool is unknown or the arguments are invalid.
    pub fn parse(call: &ToolCall) -> Result<Self, String> {
        match call.name.as_str() {
            "sleep" => parse_args(call).map(Self::Sleep),
            "ask_human" => parse_args(call).map(Self::AskHuman),
            "complete" => parse_args(call).map(Self::Complete),
            "fail" => parse_args(call).map(Self::Fail),
            "note_set" => parse_args(call).map(Self::NoteSet),
            "note_delete" => parse_args(call).map(Self::NoteDelete),
            other => Err(format!("unknown tool `{other}`")),
        }
    }
}

/// Parameters of a `core.timer` condition.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimerParams {
    #[serde(default)]
    at: Option<DateTime<Utc>>,
    #[serde(default, with = "humantime_serde")]
    after: Option<Duration>,
}

/// When a wait condition should be evaluated and when it times out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    /// Next evaluation; `None` if never polled.
    pub next_check_at: Option<DateTime<Utc>>,
    /// Deadline; `None` for no timeout.
    pub deadline_at: Option<DateTime<Utc>>,
}

/// `now + duration`, or a message for the LLM if the result is out of range.
pub(crate) fn add(now: DateTime<Utc>, duration: Duration) -> Result<DateTime<Utc>, String> {
    chrono::Duration::from_std(duration)
        .ok()
        .and_then(|duration| now.checked_add_signed(duration))
        .ok_or_else(|| format!("duration {} is too long", humantime::format_duration(duration)))
}

/// Validate a condition requested through `sleep` or `ask_human` and compute its schedule.
///
/// # Arguments
///
/// * `spec` - The condition as requested by the LLM
/// * `now` - Current time
///
/// # Errors
///
/// Returns a message for the LLM if the kind is unknown or the parameters are invalid.
pub fn schedule(spec: &WaitConditionSpec, now: DateTime<Utc>) -> Result<Schedule, String> {
    let deadline_at = spec.timeout.map(|timeout| add(now, timeout)).transpose()?;
    match spec.kind.as_str() {
        TIMER_KIND => {
            let params: TimerParams = serde_json::from_value(spec.params.clone())
                .map_err(|error| format!("invalid params for `{TIMER_KIND}`: {error}"))?;
            let at = match (params.at, params.after) {
                (Some(at), None) => at,
                (None, Some(after)) => add(now, after)?,
                _ => return Err(format!("`{TIMER_KIND}` needs exactly one of `at` or `after`")),
            };
            Ok(Schedule {
                next_check_at: Some(at),
                deadline_at,
            })
        }
        HUMAN_INPUT_KIND => Err(format!(
            "`{HUMAN_INPUT_KIND}` cannot be requested directly; call `ask_human`"
        )),
        other => Err(format!(
            "unknown wait condition kind `{other}`; available: `{TIMER_KIND}`"
        )),
    }
}

/// Specs of the core tools, shown to the LLM.
#[must_use]
pub fn core_tool_specs() -> Vec<ToolSpec> {
    let condition = json!({
        "type": "object",
        "properties": {
            "kind": {"type": "string", "description": "Condition kind, e.g. `core.timer`."},
            "params": {
                "type": "object",
                "description": "For `core.timer`: `{\"after\": \"2h\"}` or `{\"at\": \"<RFC 3339 time>\"}`."
            },
            "check_every": {"type": "string", "description": "How often to check, e.g. `1h` (polled kinds only)."},
            "timeout": {"type": "string", "description": "Give up waiting after this long, e.g. `3d`."}
        },
        "required": ["kind"]
    });
    let spec = |name: &str, description: &str, parameters: Value| ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
    };
    vec![
        spec(
            "sleep",
            "Suspend the case until any of the conditions fires or times out. Costs nothing while asleep.",
            json!({
                "type": "object",
                "properties": {
                    "conditions": {"type": "array", "items": condition, "minItems": 1},
                    "reason": {"type": "string", "description": "What you are waiting for."}
                },
                "required": ["conditions", "reason"]
            }),
        ),
        spec(
            "ask_human",
            "Ask the owner one clear question and suspend until they answer.",
            json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string"},
                    "timeout": {"type": "string", "description": "Stop waiting after this long, e.g. `2d`."},
                    "also_wait_for": {
                        "type": "array",
                        "items": condition,
                        "description": "Also wake up if any of these fires before the answer arrives."
                    }
                },
                "required": ["question"]
            }),
        ),
        spec(
            "complete",
            "Finish the case successfully.",
            json!({
                "type": "object",
                "properties": {
                    "summary": {"type": "string", "description": "What was achieved."},
                    "result": {"description": "Optional structured result."}
                },
                "required": ["summary"]
            }),
        ),
        spec(
            "fail",
            "Finish the case unsuccessfully when the goal cannot be achieved.",
            json!({
                "type": "object",
                "properties": {"reason": {"type": "string"}},
                "required": ["reason"]
            }),
        ),
        spec(
            "note_set",
            "Save a durable fact about the case. Notes are always shown to you.",
            json!({
                "type": "object",
                "properties": {"key": {"type": "string"}, "value": {"type": "string"}},
                "required": ["key", "value"]
            }),
        ),
        spec(
            "note_delete",
            "Delete a note that is no longer relevant.",
            json!({
                "type": "object",
                "properties": {"key": {"type": "string"}},
                "required": ["key"]
            }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: "call_1".to_owned(),
            name: name.to_owned(),
            arguments,
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 28, 12, 0, 0).unwrap()
    }

    fn timer(params: Value, timeout: Option<Duration>) -> WaitConditionSpec {
        WaitConditionSpec {
            kind: TIMER_KIND.to_owned(),
            params,
            check_every: None,
            timeout,
        }
    }

    #[test]
    fn parses_every_core_tool() {
        let sleep = CoreTool::parse(&call(
            "sleep",
            json!({"conditions": [{"kind": "core.timer", "params": {"after": "1h"}}], "reason": "wait"}),
        ))
        .unwrap();
        assert!(matches!(sleep, CoreTool::Sleep(args) if args.conditions.len() == 1));
        assert!(matches!(
            CoreTool::parse(&call("ask_human", json!({"question": "Ok?", "timeout": "2d"}))).unwrap(),
            CoreTool::AskHuman(AskHumanArgs { timeout: Some(_), .. })
        ));
        assert!(matches!(
            CoreTool::parse(&call("complete", json!({"summary": "done"}))).unwrap(),
            CoreTool::Complete(_)
        ));
        assert!(matches!(
            CoreTool::parse(&call("fail", json!({"reason": "no"}))).unwrap(),
            CoreTool::Fail(_)
        ));
        assert!(matches!(
            CoreTool::parse(&call("note_set", json!({"key": "k", "value": "v"}))).unwrap(),
            CoreTool::NoteSet(_)
        ));
        assert!(matches!(
            CoreTool::parse(&call("note_delete", json!({"key": "k"}))).unwrap(),
            CoreTool::NoteDelete(_)
        ));
    }

    #[test]
    fn rejects_unknown_tools_and_bad_arguments() {
        assert_eq!(
            CoreTool::parse(&call("dance", json!({}))).unwrap_err(),
            "unknown tool `dance`"
        );
        assert!(
            CoreTool::parse(&call("fail", json!({})))
                .unwrap_err()
                .starts_with("invalid arguments for `fail`")
        );
        assert!(CoreTool::parse(&call("fail", json!("not json"))).is_err());
    }

    #[test]
    fn timer_after_and_at_compute_the_check_time() {
        let after = schedule(&timer(json!({"after": "2h"}), Some(Duration::from_secs(60))), now()).unwrap();
        let at = schedule(&timer(json!({"at": "2026-10-01T09:00:00Z"}), None), now()).unwrap();

        assert_eq!(after.next_check_at, Some(now() + chrono::Duration::hours(2)));
        assert_eq!(after.deadline_at, Some(now() + chrono::Duration::minutes(1)));
        assert_eq!(
            at.next_check_at,
            Some(Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap())
        );
        assert_eq!(at.deadline_at, None);
    }

    #[test]
    fn timer_needs_exactly_one_of_at_or_after() {
        assert!(schedule(&timer(json!({}), None), now()).is_err());
        assert!(
            schedule(
                &timer(json!({"after": "1h", "at": "2026-10-01T09:00:00Z"}), None),
                now()
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_and_internal_kinds_are_rejected() {
        let unknown = WaitConditionSpec {
            kind: "email.reply_received".to_owned(),
            ..timer(json!({}), None)
        };
        let human = WaitConditionSpec {
            kind: HUMAN_INPUT_KIND.to_owned(),
            ..timer(json!({}), None)
        };

        assert!(schedule(&unknown, now()).unwrap_err().contains("unknown wait condition kind"));
        assert!(schedule(&human, now()).unwrap_err().contains("call `ask_human`"));
    }

    #[test]
    fn specs_cover_every_parsable_tool() {
        let names: Vec<String> = core_tool_specs().into_iter().map(|spec| spec.name).collect();

        assert_eq!(
            names,
            ["sleep", "ask_human", "complete", "fail", "note_set", "note_delete"]
        );
    }
}
