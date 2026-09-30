//! Plugin wait-condition checks (design §6.2): cheap, deterministic checks such as
//! "has a reply to this email arrived?", run on their own thread so a slow mailbox never
//! delays timers or activations. The LLM is only involved once a condition fires.

use std::time::Duration;

use chrono::Utc;
use clankjob_core::tool::CheckOutcome;
use clankjob_storage::waits::DueCheck;
use clankjob_storage::{self as storage, Connection};
use serde_json::json;

use crate::transitions::{FireOutcome, fire_condition};
use crate::{Result, Shared, later};

/// Checks run per pass.
const BATCH: u32 = 20;
/// How long the check thread sleeps when nothing wakes it.
const IDLE_WAIT: Duration = Duration::from_secs(5);
/// Interval of a condition stored without one.
const DEFAULT_INTERVAL: Duration = Duration::from_mins(15);
/// Failed checks in a row after which the case is woken with the error.
pub(crate) const MAX_FAILURES: u32 = 5;

/// Run due checks until shutdown.
pub(crate) fn check_loop(shared: &Shared) {
    let mut connection = match shared.db.connect() {
        Ok(connection) => connection,
        Err(error) => {
            tracing::error!(%error, "check thread cannot open the database");
            return;
        }
    };
    while !shared.is_shutting_down() {
        let seen = shared.signal.generation();
        match run_due(shared, &mut connection) {
            Ok(0) => {}
            Ok(fired) => {
                tracing::info!(fired, "plugin wait conditions fired");
                shared.signal.notify();
            }
            Err(error) => tracing::error!(%error, "plugin checks failed"),
        }
        shared.signal.wait(seen, IDLE_WAIT);
    }
}

/// Delays between the first checks of a plugin condition. Answers often come within
/// minutes, so a condition is checked often at first and then slows down to its interval.
const RAMP: [Duration; 4] = [
    Duration::from_mins(1),
    Duration::from_mins(2),
    Duration::from_mins(5),
    Duration::from_mins(10),
];

/// When to check a pending condition again: 1, 2, 5, then 10 minutes apart while it is
/// young (the first check runs as soon as it is registered), then every `every`. Never
/// longer than `every`, never shorter than the plugin's `min_interval`.
///
/// The step is chosen from how long the condition has been waiting, not from a stored
/// count: with checks on time, waiting less than 1 minute means the first step is next,
/// less than 3 minutes the second, and so on.
pub(crate) fn next_delay(waiting: Duration, every: Duration, min_interval: Duration) -> Duration {
    let mut elapsed_by_step = Duration::ZERO;
    let step = RAMP
        .iter()
        .find(|step| {
            elapsed_by_step = elapsed_by_step.saturating_add(**step);
            waiting < elapsed_by_step
        })
        .copied()
        .unwrap_or(every);
    step.min(every).max(min_interval)
}

fn interval(due: &DueCheck) -> Duration {
    due.every_ms
        .and_then(|millis| u64::try_from(millis).ok())
        .map_or(DEFAULT_INTERVAL, Duration::from_millis)
}

/// Run every due check once.
///
/// # Returns
///
/// How many conditions fired
pub(crate) fn run_due(shared: &Shared, connection: &mut Connection) -> Result<usize> {
    let mut fired: usize = 0;
    for due in storage::waits::due_checks(connection, Utc::now(), BATCH)? {
        if shared.is_shutting_down() {
            break;
        }
        let condition = &due.condition;
        let every = interval(&due);
        // Runs outside any transaction: a check may take seconds (IMAP, HTTP).
        let plugin = shared.plugin_tools.condition(&condition.kind);
        let outcome = match &plugin {
            Some(plugin) => plugin.check(&condition.params, due.cursor.as_ref()),
            None => Err(format!("the plugin providing `{}` is not loaded", condition.kind)),
        };
        let now = Utc::now();
        match outcome {
            Ok(CheckOutcome::Pending { cursor }) => {
                let waiting = now.signed_duration_since(condition.created_at).to_std().unwrap_or_default();
                let min_interval = plugin.as_ref().map_or(Duration::ZERO, |plugin| plugin.min_interval());
                let next_check_at = later(now, next_delay(waiting, every, min_interval));
                storage::waits::record_check(connection, &condition.id, cursor.as_ref(), next_check_at, 0)?;
            }
            Ok(CheckOutcome::Fired { events, cursor }) => {
                storage::waits::record_check(connection, &condition.id, cursor.as_ref(), later(now, every), 0)?;
                if fire_condition(connection, condition, FireOutcome::Fired(events), now)? {
                    fired = fired.saturating_add(1);
                }
            }
            Err(error) => {
                let failures = due.failures.saturating_add(1);
                tracing::warn!(case_id = %condition.case_id, kind = %condition.kind, failures, %error, "wait condition check failed");
                if failures >= MAX_FAILURES {
                    // Tell the case, so the LLM (or the owner) can react instead of it
                    // sleeping on a check that keeps failing.
                    let details = vec![json!({ "error": error, "failed_checks": failures })];
                    if fire_condition(connection, condition, FireOutcome::Fired(details), now)? {
                        fired = fired.saturating_add(1);
                    }
                } else {
                    // Back off: twice the interval, then up to four times it.
                    let factor = 2_u32.saturating_pow(failures.saturating_sub(1)).min(4);
                    let retry_at = later(now, every.saturating_mul(factor));
                    storage::waits::record_check(connection, &condition.id, None, retry_at, failures)?;
                }
            }
        }
    }
    Ok(fired)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use clankjob_core::case::{Budgets, CaseState, NewCase};
    use clankjob_core::event::{EventBody, WakeReason};
    use clankjob_core::ids::WaitConditionId;
    use clankjob_core::tool::PluginCondition;
    use clankjob_core::wait::{WaitCondition, WaitStatus};
    use serde_json::Value;

    use super::*;
    use crate::test_support::{ScriptedProvider, TestDb, engine};
    use crate::transitions::{create_case, load_case};

    /// A condition answering from a script of outcomes, recording the cursors it gets.
    struct Scripted {
        outcomes: Mutex<Vec<std::result::Result<CheckOutcome, String>>>,
        cursors: Mutex<Vec<Option<Value>>>,
        schema: Value,
    }

    impl PluginCondition for Scripted {
        fn plugin(&self) -> &'static str {
            "fake"
        }
        fn name(&self) -> &'static str {
            "fake_reply"
        }
        fn description(&self) -> &'static str {
            "A reply arrived."
        }
        fn params_schema(&self) -> &Value {
            &self.schema
        }
        fn default_interval(&self) -> Duration {
            Duration::ZERO
        }
        fn min_interval(&self) -> Duration {
            Duration::ZERO
        }
        fn validate(&self, _params: &Value) -> std::result::Result<(), String> {
            Ok(())
        }
        fn check(&self, _params: &Value, cursor: Option<&Value>) -> std::result::Result<CheckOutcome, String> {
            self.cursors.lock().unwrap().push(cursor.cloned());
            self.outcomes.lock().unwrap().remove(0)
        }
    }

    fn sleeping_case_waiting_on(test_db: &TestDb, connection: &mut Connection) -> WaitCondition {
        let new_case = NewCase {
            title: "T".to_owned(),
            goal: "G".to_owned(),
            owner: None,
            profile: None,
            llm: "default".to_owned(),
            model: None,
            budgets: Budgets::default(),
            instructions: Vec::new(),
            human_channels: None,
            approvals: clankjob_core::case::ApprovalPolicy::default(),
        };
        let case = create_case(connection, &new_case, Utc::now()).unwrap();
        storage::queue::remove(connection, &case.id).unwrap();
        storage::cases::update_state(connection, &case.id, CaseState::Sleeping, Utc::now()).unwrap();
        let condition = WaitCondition {
            id: WaitConditionId::generate(),
            case_id: case.id,
            kind: "fake_reply".to_owned(),
            params: serde_json::json!({ "thread": "<1@x>" }),
            next_check_at: Some(Utc::now()),
            deadline_at: None,
            status: WaitStatus::Active,
            created_at: Utc::now(),
        };
        storage::waits::insert_wait(connection, &condition).unwrap();
        storage::waits::set_check_interval(connection, &condition.id, 0).unwrap();
        let _ = test_db;
        condition
    }

    fn register(engine: &crate::Engine, outcomes: Vec<std::result::Result<CheckOutcome, String>>) -> Arc<Scripted> {
        let condition = Arc::new(Scripted {
            outcomes: Mutex::new(outcomes),
            cursors: Mutex::default(),
            schema: serde_json::json!({ "type": "object" }),
        });
        engine.plugin_tools().replace(
            Vec::new(),
            Vec::new(),
            vec![Arc::clone(&condition) as Arc<dyn PluginCondition>],
        );
        condition
    }

    #[test]
    fn checks_start_often_and_slow_down_to_the_interval() {
        let mins = Duration::from_mins;
        let every = mins(15);
        // Registered at 0 and checked right away, then at +1, +3, +8, +18, +33, …
        let schedule: Vec<Duration> = [0, 1, 3, 8, 18, 33, 48]
            .into_iter()
            .map(|at| next_delay(mins(at), every, mins(1)))
            .collect();

        assert_eq!(schedule, [mins(1), mins(2), mins(5), mins(10), every, every, every]);
        assert_eq!(next_delay(mins(0), mins(3), Duration::ZERO), mins(1));
        assert_eq!(
            next_delay(mins(8), mins(3), Duration::ZERO),
            mins(3),
            "never longer than the interval"
        );
        assert_eq!(
            next_delay(mins(0), every, mins(5)),
            mins(5),
            "never shorter than the plugin allows"
        );
    }

    #[test]
    fn a_condition_fires_after_pending_checks_and_hands_its_cursor_back() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let engine = engine(&test_db, ScriptedProvider::new([]));
        let reply = serde_json::json!({ "from": "bob@x.ca", "subject": "Re: quote" });
        let script = register(
            &engine,
            vec![
                Ok(CheckOutcome::Pending {
                    cursor: Some(serde_json::json!({ "uid": 5 })),
                }),
                Ok(CheckOutcome::Fired {
                    events: vec![reply.clone()],
                    cursor: None,
                }),
            ],
        );
        let condition = sleeping_case_waiting_on(&test_db, &mut connection);

        // Act
        let first = run_due(&engine.shared, &mut connection).unwrap();
        let second = run_due(&engine.shared, &mut connection).unwrap();

        // Assert
        assert_eq!((first, second), (0, 1));
        assert_eq!(
            *script.cursors.lock().unwrap(),
            [None, Some(serde_json::json!({ "uid": 5 }))]
        );
        assert_eq!(
            load_case(&connection, &condition.case_id).unwrap().state,
            CaseState::Pending
        );
        let events = storage::events::list_events(&connection, &condition.case_id, 0, None).unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.body,
            EventBody::Wake(WakeReason::ConditionFired { kind, details, .. }) if kind == "fake_reply" && details == std::slice::from_ref(&reply)
        )));
    }

    #[test]
    fn a_check_that_keeps_failing_wakes_the_case_with_the_error() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let engine = engine(&test_db, ScriptedProvider::new([]));
        register(
            &engine,
            (0..MAX_FAILURES).map(|_| Err("IMAP login failed".to_owned())).collect(),
        );
        let condition = sleeping_case_waiting_on(&test_db, &mut connection);

        // Act
        let fired: usize = (0..MAX_FAILURES)
            .map(|_| run_due(&engine.shared, &mut connection).unwrap())
            .sum();

        // Assert
        assert_eq!(fired, 1);
        let events = storage::events::list_events(&connection, &condition.case_id, 0, None).unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.body,
            EventBody::Wake(WakeReason::ConditionFired { details, .. }) if details[0]["error"] == "IMAP login failed"
        )));
    }
}
