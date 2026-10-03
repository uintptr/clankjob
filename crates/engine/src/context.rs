//! Rebuilding the LLM conversation from a case's event log (design §7.1).

use std::collections::HashSet;

use clankjob_core::event::{Event, EventBody, ToolResult};
use clankjob_core::ids::FileId;
use clankjob_core::llm::{ImageData, Message, ToolCall};
use serde_json::Value;

use crate::prompts::{NUDGE, PromptContext, PromptSet, RenderError, WAKE};

/// Images shown with `view_image` stay in the conversation for this many views; older
/// ones are replaced by a note, since every image is resent with every LLM turn.
const MAX_IMAGES_IN_CONTEXT: usize = 4;

/// The file a successful `view_image` result refers to.
fn viewed_image(result: &ToolResult) -> Option<(FileId, String)> {
    if result.is_error {
        return None;
    }
    let id = result.content.get("image_file_id")?.as_str()?;
    let name = result.content.get("file")?.as_str()?;
    Some((FileId::from_string(id), name.to_owned()))
}

/// Whether an event is a `sleep` that asked to wake up with a fresh conversation.
fn is_fresh_sleep(event: &Event) -> bool {
    matches!(&event.body, EventBody::ToolResult(result)
        if result.tool_name == "sleep" && !result.is_error && result.content.get("fresh") == Some(&Value::Bool(true)))
}

/// The events the conversation is built from: those from the first wake after the latest
/// `sleep` with `fresh`, or all of them. Earlier events stay in the log and the timeline.
fn since_fresh_start(events: &[Event]) -> &[Event] {
    let Some(sleep) = events.iter().rposition(is_fresh_sleep) else {
        return events;
    };
    events
        .iter()
        .skip(sleep)
        .position(|event| matches!(event.body, EventBody::Wake(_)))
        .and_then(|offset| events.get(sleep.saturating_add(offset)..))
        .unwrap_or(events)
}

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
/// After a `sleep` with `fresh`, the conversation starts again at the next wake.
///
/// # Arguments
///
/// * `events` - The case's full event log, in order
/// * `prompts` - Templates for wake and nudge messages
/// * `context` - Template variables; `now` is replaced by each event's own time
/// * `image` - Loads an image shown with `view_image`
///
/// # Errors
///
/// Returns [`RenderError`] if a wake or nudge template fails to render.
pub fn build_messages(
    events: &[Event],
    prompts: &PromptSet,
    context: &PromptContext<'_>,
    image: &dyn Fn(&FileId) -> Option<ImageData>,
) -> Result<Vec<Message>, RenderError> {
    let events = since_fresh_start(events);
    let viewed = events
        .iter()
        .filter(|event| matches!(&event.body, EventBody::ToolResult(result) if viewed_image(result).is_some()))
        .count();
    let mut to_omit = viewed.saturating_sub(MAX_IMAGES_IN_CONTEXT);
    let mut messages = Vec::with_capacity(events.len());
    let mut deferred = Vec::new();
    let mut unanswered: HashSet<&str> = HashSet::new();
    for event in events {
        // Each event is rendered with its own timestamp so past messages never change,
        // which keeps the rebuilt conversation stable across activations.
        let at = PromptContext {
            now: crate::prompts::local_time(event.created_at, context.timezone),
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
                images: Vec::new(),
            }),
            EventBody::Nudge => Some(Message::User {
                text: prompts.render(NUDGE, &at)?,
                images: Vec::new(),
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
                // Chat APIs only accept images in user messages, so a viewed image
                // follows the tool results as one.
                viewed_image(result).map(|(id, name)| {
                    let loaded = if to_omit > 0 {
                        to_omit = to_omit.saturating_sub(1);
                        None
                    } else {
                        image(&id)
                    };
                    match loaded {
                        Some(data) => Message::User {
                            text: format!("Image `{name}`, as requested with view_image:"),
                            images: vec![data],
                        },
                        None => Message::User {
                            text: format!(
                                "[Image `{name}` left out to save context; call view_image again to see it.]"
                            ),
                            images: Vec::new(),
                        },
                    }
                })
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
            timezone: chrono_tz::Tz::UTC,
            case: CaseView {
                title: "T",
                goal: "G",
                owner: None,
                created_at: String::new(),
            },
            budgets: &budgets,
            usage: &usage,
            activations_today: 1,
            notes: &[],
            tools: &[],
            instructions: &[],
            files: &[],
            guides: &[],
            plugins: &[],
            user_prompt: None,
            wake: None,
        };
        build_messages(events, &PromptSet::builtin(), &context, &|_| None).unwrap()
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
        assert_eq!(
            pending_tool_calls(&[event(1, EventBody::Wake(WakeReason::Created))]),
            [] as [clankjob_core::llm::ToolCall; 0]
        );
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
        assert!(matches!(&messages[0], Message::User { text, .. } if text.contains("just created")));
        assert!(matches!(&messages[1], Message::Assistant(_)));
        assert!(matches!(&messages[2], Message::Tool { tool_call_id, .. } if tool_call_id == "a"));
        assert!(matches!(&messages[3], Message::User { text, .. } if text.contains("without calling a tool")));
    }

    #[test]
    fn viewed_images_follow_their_results_and_only_the_latest_are_kept() {
        // Arrange: six views, each its own LLM turn and tool result.
        let mut events = Vec::new();
        for index in 0..6_i64 {
            let id = format!("v{index}");
            events.push(event(index * 2, turn(&[id.as_str()])));
            events.push(event(
                index * 2 + 1,
                EventBody::ToolResult(ToolResult {
                    tool_call_id: id,
                    tool_name: "view_image".to_owned(),
                    content: json!({"status": "shown", "file": format!("{index}.png"), "image_file_id": format!("f{index}")}),
                    is_error: false,
                }),
            ));
        }
        let (budgets, usage) = (Budgets::default(), Usage::default());
        let context = PromptContext {
            now: String::new(),
            timezone: chrono_tz::Tz::UTC,
            case: CaseView {
                title: "T",
                goal: "G",
                owner: None,
                created_at: String::new(),
            },
            budgets: &budgets,
            usage: &usage,
            activations_today: 1,
            notes: &[],
            tools: &[],
            instructions: &[],
            files: &[],
            guides: &[],
            plugins: &[],
            user_prompt: None,
            wake: None,
        };
        let loader = |id: &FileId| {
            Some(ImageData {
                media_type: "image/png".to_owned(),
                base64: id.to_string(),
            })
        };

        // Act
        let messages = build_messages(&events, &PromptSet::builtin(), &context, &loader).unwrap();

        // Assert: after each tool result comes a user message; the first two are notes.
        let images: Vec<Option<String>> = messages
            .iter()
            .filter_map(|message| match message {
                Message::User { images, .. } => Some(images.first().map(|image| image.base64.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            images,
            [
                None,
                None,
                Some("f2".to_owned()),
                Some("f3".to_owned()),
                Some("f4".to_owned()),
                Some("f5".to_owned())
            ]
        );
        assert!(matches!(&messages[2], Message::User { text, .. } if text.contains("left out to save context")));
    }

    fn sleep(id: &str, fresh: bool) -> EventBody {
        EventBody::ToolResult(ToolResult {
            tool_call_id: id.to_owned(),
            tool_name: "sleep".to_owned(),
            content: json!({"status": "sleeping", "conditions": [], "fresh": fresh}),
            is_error: false,
        })
    }

    fn timer() -> EventBody {
        EventBody::Wake(WakeReason::HumanMessage {
            text: "tick".to_owned(),
        })
    }

    #[test]
    fn a_fresh_sleep_starts_the_conversation_again_at_the_next_wake() {
        let events = [
            event(1, EventBody::Wake(WakeReason::Created)),
            event(2, turn(&["s1", "late"])),
            event(3, sleep("s1", true)),
            event(4, result("late")),
            event(5, timer()),
            event(6, turn(&["s2"])),
            event(7, sleep("s2", true)),
            event(8, timer()),
        ];

        let messages = build(&events);

        assert_eq!(messages.len(), 1);
        assert!(matches!(&messages[0], Message::User { text, .. } if text.contains("tick")));
    }

    #[test]
    fn a_sleep_without_fresh_keeps_the_conversation() {
        let events = [
            event(1, EventBody::Wake(WakeReason::Created)),
            event(2, turn(&["s1"])),
            event(3, sleep("s1", false)),
            event(4, timer()),
        ];

        assert_eq!(build(&events).len(), 4);
    }

    #[test]
    fn a_fresh_sleep_not_yet_woken_keeps_the_conversation() {
        let events = [
            event(1, EventBody::Wake(WakeReason::Created)),
            event(2, turn(&["s1"])),
            event(3, sleep("s1", true)),
        ];

        assert_eq!(build(&events).len(), 3);
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
        assert!(matches!(&messages[3], Message::User { text, .. } if text.contains("hi")));
    }
}
