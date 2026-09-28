//! Rebuilding the LLM conversation from a case's event log (design §7.1).

use std::collections::HashSet;

use clankjob_core::event::{Event, EventBody};
use clankjob_core::llm::{Message, ToolCall};

use crate::prompts::{NUDGE, PromptContext, PromptSet, RenderError, WAKE};

/// Tool calls of the latest LLM turn that have no recorded result yet.
///
/// These are executed before the LLM is called again. That is how an activation that
/// crashed between recording the LLM's answer and running its tools resumes without
/// asking the LLM twice.
#[must_use]
pub fn pending_tool_calls(events: &[Event]) -> Vec<ToolCall> {
    // `rposition` searches from the end, returning the index of the last LLM message.
    let Some(last_turn) = events.iter().rposition(|event| matches!(event.body, EventBody::LlmMessage(_))) else {
        return Vec::new();
    };
    // `.get()` returns `None` instead of panicking on a bad index; both are always `Some`.
    let (
        Some(Event {
            body: EventBody::LlmMessage(message),
            ..
        }),
        Some(later),
    ) = (events.get(last_turn), events.get(last_turn.saturating_add(1)..))
    else {
        return Vec::new();
    };
    let answered: HashSet<&str> = later
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ToolResult(result) => Some(result.tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    message
        .tool_calls
        .iter()
        .filter(|call| !answered.contains(call.id.as_str()))
        .cloned()
        .collect()
}

/// Turn the event log into LLM messages.
///
/// A wake that arrives while tool calls are still unanswered (e.g. the owner posts a
/// message during an activation) is held back until the results are in, because providers
/// require tool results to directly follow the turn that requested them.
///
/// # Arguments
///
/// * `events` - The case's full event log, in order
/// * `prompts` - Templates for wake and nudge messages
/// * `context` - Template variables; `now` is replaced by each event's own time
///
/// # Errors
///
/// Returns [`RenderError`] if a wake or nudge template fails to render.
pub fn build_messages(
    events: &[Event],
    prompts: &PromptSet,
    context: &PromptContext<'_>,
) -> Result<Vec<Message>, RenderError> {
    let mut messages = Vec::with_capacity(events.len());
    let mut deferred = Vec::new();
    let mut unanswered: HashSet<&str> = HashSet::new();
    for event in events {
        // Each event is rendered with its own timestamp so past messages never change,
        // which keeps the rebuilt conversation stable across activations.
        let at = PromptContext {
            now: event.created_at.to_rfc3339(),
            ..context.clone()
        };
        let user_message = match &event.body {
            EventBody::Wake(reason) => Some(Message::User {
                text: prompts.render(
                    WAKE,
                    &PromptContext {
                        wake: Some(reason),
                        ..at
                    },
                )?,
            }),
            EventBody::Nudge => Some(Message::User {
                text: prompts.render(NUDGE, &at)?,
            }),
            EventBody::LlmMessage(message) => {
                unanswered = message.tool_calls.iter().map(|call| call.id.as_str()).collect();
                messages.push(Message::Assistant(message.clone()));
                None
            }
            EventBody::ToolResult(result) => {
                unanswered.remove(result.tool_call_id.as_str());
                messages.push(Message::Tool {
                    tool_call_id: result.tool_call_id.clone(),
                    content: result.content.to_string(),
                });
                if unanswered.is_empty() {
                    messages.append(&mut deferred);
                }
                None
            }
            EventBody::StateChanged { .. } | EventBody::Error { .. } => None,
        };
        match user_message {
            Some(message) if unanswered.is_empty() => messages.push(message),
            Some(message) => deferred.push(message),
            None => {}
        }
    }
    messages.append(&mut deferred);
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use clankjob_core::case::{Budgets, Usage};
    use clankjob_core::event::{ToolResult, WakeReason};
    use clankjob_core::ids::CaseId;
    use clankjob_core::llm::AssistantMessage;
    use serde_json::json;

    use super::*;
    use crate::prompts::CaseView;

    fn event(seq: i64, body: EventBody) -> Event {
        Event {
            seq,
            case_id: CaseId::from_string("case"),
            activation_id: None,
            body,
            created_at: Utc::now(),
        }
    }

    fn turn(ids: &[&str]) -> EventBody {
        EventBody::LlmMessage(AssistantMessage {
            text: None,
            tool_calls: ids
                .iter()
                .map(|id| ToolCall {
                    id: (*id).to_owned(),
                    name: "note_set".to_owned(),
                    arguments: json!({}),
                })
                .collect(),
        })
    }

    fn result(id: &str) -> EventBody {
        EventBody::ToolResult(ToolResult {
            tool_call_id: id.to_owned(),
            tool_name: "note_set".to_owned(),
            content: json!({"ok": true}),
            is_error: false,
        })
    }

    fn build(events: &[Event]) -> Vec<Message> {
        let (budgets, usage) = (Budgets::default(), Usage::default());
        let context = PromptContext {
            now: String::new(),
            case: CaseView {
                title: "T",
                goal: "G",
                owner: None,
                created_at: String::new(),
            },
            budgets: &budgets,
            usage: &usage,
            notes: &[],
            tools: &[],
            wake: None,
        };
        build_messages(events, &PromptSet::builtin(), &context).unwrap()
    }

    #[test]
    fn pending_calls_are_the_unanswered_calls_of_the_last_turn() {
        let events = [
            event(1, turn(&["old"])),
            event(2, result("old")),
            event(3, turn(&["a", "b"])),
            event(4, result("a")),
        ];

        let pending = pending_tool_calls(&events);

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "b");
    }

    #[test]
    fn no_turn_means_no_pending_calls() {
        assert!(pending_tool_calls(&[event(1, EventBody::Wake(WakeReason::Created))]).is_empty());
    }

    #[test]
    fn events_map_to_messages_and_timeline_events_are_skipped() {
        let events = [
            event(1, EventBody::Wake(WakeReason::Created)),
            event(2, turn(&["a"])),
            event(3, result("a")),
            event(
                4,
                EventBody::Error {
                    message: "x".to_owned(),
                },
            ),
            event(5, EventBody::Nudge),
        ];

        let messages = build(&events);

        assert_eq!(messages.len(), 4);
        assert!(matches!(&messages[0], Message::User { text } if text.contains("just created")));
        assert!(matches!(&messages[1], Message::Assistant(_)));
        assert!(matches!(&messages[2], Message::Tool { tool_call_id, .. } if tool_call_id == "a"));
        assert!(matches!(&messages[3], Message::User { text } if text.contains("without calling a tool")));
    }

    #[test]
    fn wake_during_unanswered_calls_is_deferred_until_results_are_in() {
        let events = [
            event(1, turn(&["a", "b"])),
            event(2, result("a")),
            event(3, EventBody::Wake(WakeReason::HumanMessage { text: "hi".to_owned() })),
            event(4, result("b")),
        ];

        let messages = build(&events);

        assert!(matches!(&messages[2], Message::Tool { tool_call_id, .. } if tool_call_id == "b"));
        assert!(matches!(&messages[3], Message::User { text } if text.contains("hi")));
    }
}
