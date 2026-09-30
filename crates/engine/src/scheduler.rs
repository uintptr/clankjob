//! The scheduler tick (design §6.2): fire wait conditions that are due.
//!
//! Milestone 1 only has built-in kinds: `core.timer` fires at its check time, and every
//! kind times out at its deadline. Plugin checks are added with the plugin host.

use chrono::{DateTime, Utc};
use clankjob_core::wait::{TIMER_KIND, WaitCondition};
use clankjob_storage::{self as storage, Connection};

use crate::Result;
use crate::transitions::{FireOutcome, fire_condition};

/// Whether the deadline, rather than the check, is what came due first.
fn timed_out(condition: &WaitCondition, now: DateTime<Utc>) -> bool {
    condition.deadline_at.is_some_and(|deadline| {
        deadline <= now && condition.next_check_at.is_none_or(|next_check| deadline < next_check)
    })
}

/// Fire every due wait condition, up to `batch` of them.
///
/// # Arguments
///
/// * `connection` - Database connection
/// * `now` - Current time
/// * `batch` - Maximum conditions examined
///
/// # Returns
///
/// How many conditions fired or timed out
///
/// # Errors
///
/// Returns [`crate::EngineError::Storage`] if the database fails.
pub(crate) fn tick(connection: &mut Connection, now: DateTime<Utc>, batch: u32) -> Result<usize> {
    let mut fired: usize = 0;
    for condition in storage::waits::due_waits(connection, now, batch)? {
        let outcome = if timed_out(&condition, now) {
            FireOutcome::TimedOut
        } else if condition.kind == TIMER_KIND {
            FireOutcome::Fired(Vec::new())
        } else {
            tracing::warn!(kind = %condition.kind, "no checker for wait condition kind");
            continue;
        };
        // `false` means another condition of the same case already woke it.
        if fire_condition(connection, &condition, outcome, now)? {
            fired = fired.saturating_add(1);
        }
    }
    Ok(fired)
}

#[cfg(test)]
mod tests {
    use clankjob_core::case::{Budgets, CaseState, NewCase};
    use clankjob_core::event::{EventBody, WakeReason};
    use clankjob_core::ids::{CaseId, WaitConditionId};
    use clankjob_core::wait::{HUMAN_INPUT_KIND, WaitStatus};

    use super::*;
    use crate::test_support::{TestDb, time};
    use crate::transitions::{create_case, load_case};

    fn sleeping_case(connection: &mut Connection) -> CaseId {
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
        let case = create_case(connection, &new_case, time(0)).unwrap();
        storage::queue::remove(connection, &case.id).unwrap();
        storage::cases::update_state(connection, &case.id, CaseState::Sleeping, time(0)).unwrap();
        case.id
    }

    fn wait(case_id: &CaseId, kind: &str, next_check_at: Option<i64>, deadline_at: Option<i64>) -> WaitCondition {
        WaitCondition {
            id: WaitConditionId::generate(),
            case_id: case_id.clone(),
            kind: kind.to_owned(),
            params: serde_json::json!({}),
            next_check_at: next_check_at.map(time),
            deadline_at: deadline_at.map(time),
            status: WaitStatus::Active,
            created_at: time(0),
        }
    }

    fn last_wake(connection: &Connection, case_id: &CaseId) -> WakeReason {
        storage::events::list_events(connection, case_id, 0, None)
            .unwrap()
            .into_iter()
            .rev()
            .find_map(|event| match event.body {
                EventBody::Wake(reason) => Some(reason),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn due_timer_fires_and_wakes_the_case() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let case_id = sleeping_case(&mut connection);
        storage::waits::insert_wait(&connection, &wait(&case_id, TIMER_KIND, Some(10), Some(20))).unwrap();

        assert_eq!(tick(&mut connection, time(5), 10).unwrap(), 0);
        assert_eq!(tick(&mut connection, time(15), 10).unwrap(), 1);

        assert_eq!(load_case(&connection, &case_id).unwrap().state, CaseState::Pending);
        assert!(matches!(
            last_wake(&connection, &case_id),
            WakeReason::ConditionFired { .. }
        ));
    }

    #[test]
    fn human_input_deadline_times_out() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let case_id = sleeping_case(&mut connection);
        storage::waits::insert_wait(&connection, &wait(&case_id, HUMAN_INPUT_KIND, None, Some(10))).unwrap();

        assert_eq!(tick(&mut connection, time(10), 10).unwrap(), 1);

        assert!(matches!(last_wake(&connection, &case_id), WakeReason::TimedOut { .. }));
    }

    #[test]
    fn only_the_first_of_two_due_conditions_wakes_the_case() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let case_id = sleeping_case(&mut connection);
        storage::waits::insert_wait(&connection, &wait(&case_id, TIMER_KIND, Some(10), None)).unwrap();
        storage::waits::insert_wait(&connection, &wait(&case_id, TIMER_KIND, Some(11), None)).unwrap();

        assert_eq!(tick(&mut connection, time(20), 10).unwrap(), 1);
    }

    #[test]
    fn deadline_before_check_time_counts_as_timeout() {
        let case_id = CaseId::generate();

        assert!(timed_out(&wait(&case_id, TIMER_KIND, Some(20), Some(10)), time(30)));
        assert!(!timed_out(&wait(&case_id, TIMER_KIND, Some(10), Some(20)), time(30)));
        assert!(!timed_out(&wait(&case_id, TIMER_KIND, Some(10), Some(20)), time(15)));
    }
}
