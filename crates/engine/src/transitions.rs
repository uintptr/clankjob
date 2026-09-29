//! State transitions shared by the API, the scheduler and the workers (design §4, §6, §10).
//!
//! Each public function runs in one write transaction, so a case is never left half-way
//! between two states.

use chrono::{DateTime, Utc};
use clankjob_core::case::{Case, CaseState, Instruction, NewCase, NewInstruction};
use clankjob_core::event::{EventBody, InstructionChange, WakeReason};
use clankjob_core::file::CaseFile;
use clankjob_core::human::{HumanRequest, HumanRequestStatus};
use clankjob_core::ids::{ActivationId, CaseId, HumanRequestId, InstructionId};
use clankjob_core::wait::{HUMAN_INPUT_KIND, WaitCondition, WaitStatus};
use clankjob_storage::human::Answer;
use clankjob_storage::{self as storage, Connection, begin_write, commit};
use serde_json::Value;

use crate::{EngineError, Result};

/// How a wait condition ended, for [`fire_condition`].
#[derive(Debug, Clone, PartialEq)]
pub enum FireOutcome {
    /// It happened, with kind-specific details.
    Fired(Vec<Value>),
    /// Its deadline passed first.
    TimedOut,
}

/// A case claimed by a worker for an activation.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedCase {
    /// The case, already in state `running`.
    pub case: Case,
    /// How many times this work was released for a retry.
    pub attempts: u32,
}

/// Load a case or fail with [`EngineError::CaseNotFound`].
pub(crate) fn load_case(connection: &Connection, id: &CaseId) -> Result<Case> {
    storage::cases::get_case(connection, id)?.ok_or_else(|| EngineError::CaseNotFound(id.clone()))
}

/// Change a case's state and record it in the timeline.
pub(crate) fn change_state(
    connection: &Connection,
    case: &Case,
    to: CaseState,
    activation_id: Option<&ActivationId>,
    now: DateTime<Utc>,
) -> Result<()> {
    if case.state != to {
        storage::cases::update_state(connection, &case.id, to, now)?;
        let body = EventBody::StateChanged { from: case.state, to };
        storage::events::append_event(connection, &case.id, activation_id, &body, now)?;
        if matches!(to, CaseState::Completed | CaseState::Failed) {
            crate::channels::queue_finished(connection, &case.id, now)?;
        }
    }
    Ok(())
}

/// Record why a case wakes up, drop whatever it was waiting for, and queue it.
fn wake(connection: &Connection, case: &Case, reason: WakeReason, now: DateTime<Utc>) -> Result<()> {
    match case.state {
        CaseState::Cancelled => {
            return Err(EngineError::InvalidState {
                id: case.id.clone(),
                state: case.state,
            });
        }
        CaseState::Sleeping | CaseState::WaitingForHuman | CaseState::Completed | CaseState::Failed => {
            change_state(connection, case, CaseState::Pending, None, now)?;
        }
        // Already queued or running: the queue marks it to run again after this activation.
        CaseState::Pending | CaseState::Running => {}
    }
    storage::events::append_event(connection, &case.id, None, &EventBody::Wake(reason), now)?;
    storage::waits::cancel_waits(connection, &case.id)?;
    crate::channels::close_requests(connection, &case.id, HumanRequestStatus::Superseded, now)?;
    storage::queue::enqueue(connection, &case.id, now)?;
    Ok(())
}

/// Answer an open request and wake its case (the first answer wins).
fn answer_open_request(
    connection: &Connection,
    request: &HumanRequest,
    answer: Answer<'_>,
    now: DateTime<Utc>,
) -> Result<()> {
    if !storage::human::answer_request(connection, &request.id, answer, now)? {
        return Err(EngineError::AlreadyResolved(request.id.clone()));
    }
    storage::waits::resolve_waits_of_kind(connection, &request.case_id, HUMAN_INPUT_KIND, WaitStatus::Fired)?;
    let outcome = serde_json::json!({ "status": "answered", "via": answer.via, "responder": answer.responder });
    crate::channels::queue_resolution(connection, &request.case_id, &request.id, &outcome, now)?;
    let case = load_case(connection, &request.case_id)?;
    let reason = WakeReason::HumanAnswer {
        request_id: request.id.clone(),
        question: request.question.clone(),
        answer: answer.text.to_owned(),
        via: Some(answer.via.to_owned()),
    };
    wake(connection, &case, reason, now)
}

/// Create a case and queue its first activation.
///
/// # Errors
///
/// Returns [`EngineError::Storage`] if the database fails.
pub fn create_case(connection: &mut Connection, new_case: &NewCase, now: DateTime<Utc>) -> Result<Case> {
    let transaction = begin_write(connection)?;
    let case = storage::cases::insert_case(&transaction, &CaseId::generate(), new_case, now)?;
    for instruction in &new_case.instructions {
        storage::instructions::insert_instruction(&transaction, &case.id, instruction, now)?;
    }
    storage::events::append_event(&transaction, &case.id, None, &EventBody::Wake(WakeReason::Created), now)?;
    storage::queue::enqueue(&transaction, &case.id, now)?;
    commit(transaction)?;
    Ok(case)
}

/// Post a message from the owner to a case.
///
/// If the case has an open question, the message answers it. Otherwise it wakes the case
/// as a plain message, reopening it if it had finished.
///
/// # Errors
///
/// Returns [`EngineError::CaseNotFound`], [`EngineError::InvalidState`] for a cancelled case,
/// or [`EngineError::Storage`].
pub fn post_message(connection: &mut Connection, case_id: &CaseId, text: &str, now: DateTime<Utc>) -> Result<()> {
    let transaction = begin_write(connection)?;
    let case = load_case(&transaction, case_id)?;
    let open = storage::human::list_requests(&transaction, Some(HumanRequestStatus::Open), Some(case_id))?;
    match open.first() {
        Some(request) => answer_open_request(
            &transaction,
            request,
            Answer {
                text,
                via: "web",
                responder: None,
            },
            now,
        )?,
        None => wake(
            &transaction,
            &case,
            WakeReason::HumanMessage { text: text.to_owned() },
            now,
        )?,
    }
    commit(transaction)?;
    Ok(())
}

/// Answer a human request from a channel.
///
/// # Arguments
///
/// * `connection` - Database connection
/// * `request_id` - The request
/// * `answer` - Answer text, channel and responder
/// * `now` - Current time
///
/// # Returns
///
/// The answered request
///
/// # Errors
///
/// Returns [`EngineError::RequestNotFound`], [`EngineError::AlreadyResolved`] if another
/// answer came first, or [`EngineError::Storage`].
pub fn answer_request(
    connection: &mut Connection,
    request_id: &HumanRequestId,
    answer: Answer<'_>,
    now: DateTime<Utc>,
) -> Result<HumanRequest> {
    let transaction = begin_write(connection)?;
    let request = storage::human::get_request(&transaction, request_id)?
        .ok_or_else(|| EngineError::RequestNotFound(request_id.clone()))?;
    answer_open_request(&transaction, &request, answer, now)?;
    let answered = storage::human::get_request(&transaction, request_id)?
        .ok_or_else(|| EngineError::RequestNotFound(request_id.clone()))?;
    commit(transaction)?;
    Ok(answered)
}

/// Store a file's row and wake its case, which reopens a finished case like a message.
///
/// # Errors
///
/// Returns [`EngineError::CaseNotFound`], [`EngineError::InvalidState`] for a cancelled
/// case, or [`EngineError::Storage`].
pub fn add_file(connection: &mut Connection, file: &CaseFile, now: DateTime<Utc>) -> Result<()> {
    let transaction = begin_write(connection)?;
    let case = load_case(&transaction, &file.case_id)?;
    storage::files::insert_file(&transaction, file)?;
    let reason = WakeReason::FileAdded {
        file_id: file.id.clone(),
        name: file.name.clone(),
        kind: file.kind,
    };
    wake(&transaction, &case, reason, now)?;
    commit(transaction)?;
    Ok(())
}

/// Add, edit or remove an instruction and wake the case; see
/// [`crate::Engine::change_instruction`]. Limits are checked by the caller.
///
/// # Errors
///
/// Returns [`EngineError::InstructionNotFound`], [`EngineError::CaseNotFound`],
/// [`EngineError::InvalidState`] for a cancelled case, or [`EngineError::Storage`].
pub fn change_instruction(
    connection: &mut Connection,
    case_id: &CaseId,
    id: Option<&InstructionId>,
    instruction: Option<&NewInstruction>,
    now: DateTime<Utc>,
) -> Result<Option<Instruction>> {
    let transaction = begin_write(connection)?;
    let case = load_case(&transaction, case_id)?;
    let not_found = |id: &InstructionId| EngineError::InstructionNotFound(id.clone());
    let (stored, reason) = match (id, instruction) {
        (None, Some(instruction)) => {
            let stored = storage::instructions::insert_instruction(&transaction, case_id, instruction, now)?;
            let reason = WakeReason::InstructionsChanged {
                instruction_id: stored.id.clone(),
                name: stored.name.clone(),
                change: InstructionChange::Added,
            };
            (Some(stored), reason)
        }
        (Some(id), Some(instruction)) => {
            if !storage::instructions::update_instruction(&transaction, case_id, id, instruction, now)? {
                return Err(not_found(id));
            }
            let stored =
                storage::instructions::get_instruction(&transaction, case_id, id)?.ok_or_else(|| not_found(id))?;
            let reason = WakeReason::InstructionsChanged {
                instruction_id: id.clone(),
                name: stored.name.clone(),
                change: InstructionChange::Updated,
            };
            (Some(stored), reason)
        }
        (Some(id), None) => {
            let removed =
                storage::instructions::get_instruction(&transaction, case_id, id)?.ok_or_else(|| not_found(id))?;
            storage::instructions::delete_instruction(&transaction, case_id, id)?;
            let reason = WakeReason::InstructionsChanged {
                instruction_id: id.clone(),
                name: removed.name,
                change: InstructionChange::Removed,
            };
            (None, reason)
        }
        (None, None) => return Ok(None),
    };
    wake(&transaction, &case, reason, now)?;
    commit(transaction)?;
    Ok(stored)
}

/// Wake a sleeping or waiting case by hand.
///
/// # Errors
///
/// Returns [`EngineError::CaseNotFound`], [`EngineError::InvalidState`] unless the case is
/// sleeping or waiting for a human, or [`EngineError::Storage`].
pub fn wake_case(connection: &mut Connection, case_id: &CaseId, now: DateTime<Utc>) -> Result<()> {
    let transaction = begin_write(connection)?;
    let case = load_case(&transaction, case_id)?;
    if !matches!(case.state, CaseState::Sleeping | CaseState::WaitingForHuman) {
        return Err(EngineError::InvalidState {
            id: case.id,
            state: case.state,
        });
    }
    wake(&transaction, &case, WakeReason::Manual, now)?;
    commit(transaction)?;
    Ok(())
}

/// Cancel a case. A running activation stops at its next step.
///
/// # Errors
///
/// Returns [`EngineError::CaseNotFound`], [`EngineError::InvalidState`] if the case already
/// finished, or [`EngineError::Storage`].
pub fn cancel_case(connection: &mut Connection, case_id: &CaseId, now: DateTime<Utc>) -> Result<()> {
    let transaction = begin_write(connection)?;
    let case = load_case(&transaction, case_id)?;
    if case.state.is_terminal() {
        return Err(EngineError::InvalidState {
            id: case.id,
            state: case.state,
        });
    }
    storage::waits::cancel_waits(&transaction, case_id)?;
    crate::channels::close_requests(&transaction, case_id, HumanRequestStatus::Cancelled, now)?;
    storage::queue::remove(&transaction, case_id)?;
    change_state(&transaction, &case, CaseState::Cancelled, None, now)?;
    commit(transaction)?;
    Ok(())
}

/// Resolve a wait condition and wake its case.
///
/// # Returns
///
/// `false` if the condition was no longer active (another condition already won)
///
/// # Errors
///
/// Returns [`EngineError::Storage`] if the database fails.
pub fn fire_condition(
    connection: &mut Connection,
    condition: &WaitCondition,
    outcome: FireOutcome,
    now: DateTime<Utc>,
) -> Result<bool> {
    let transaction = begin_write(connection)?;
    let (status, reason) = match outcome {
        FireOutcome::Fired(details) => (
            WaitStatus::Fired,
            WakeReason::ConditionFired {
                condition_id: condition.id.clone(),
                kind: condition.kind.clone(),
                details,
            },
        ),
        FireOutcome::TimedOut => (
            WaitStatus::TimedOut,
            WakeReason::TimedOut {
                condition_id: condition.id.clone(),
                kind: condition.kind.clone(),
            },
        ),
    };
    if !storage::waits::resolve_wait(&transaction, &condition.id, status)? {
        return Ok(false);
    }
    let case = load_case(&transaction, &condition.case_id)?;
    wake(&transaction, &case, reason, now)?;
    commit(transaction)?;
    Ok(true)
}

/// Claim the next case that should run and mark it `running`.
///
/// Queue rows of cancelled cases are dropped on the way.
///
/// # Arguments
///
/// * `connection` - Database connection
/// * `now` - Current time
/// * `lease_until` - When the claim expires unless renewed
///
/// # Returns
///
/// The claimed case, or `None` if nothing is due
///
/// # Errors
///
/// Returns [`EngineError::Storage`] if the database fails.
pub fn claim_next(
    connection: &mut Connection,
    now: DateTime<Utc>,
    lease_until: DateTime<Utc>,
) -> Result<Option<ClaimedCase>> {
    let transaction = begin_write(connection)?;
    let claimed = loop {
        let Some(work) = storage::queue::claim_next(&transaction, now, lease_until)? else {
            break None;
        };
        let case = load_case(&transaction, &work.case_id)?;
        if case.state == CaseState::Cancelled {
            storage::queue::remove(&transaction, &case.id)?;
            continue;
        }
        // A case queued again while it was sleeping or finished was woken by something
        // newer (e.g. a message during its last activation): what it waited for is moot.
        storage::waits::cancel_waits(&transaction, &case.id)?;
        crate::channels::close_requests(&transaction, &case.id, HumanRequestStatus::Superseded, now)?;
        change_state(&transaction, &case, CaseState::Running, None, now)?;
        let mut case = load_case(&transaction, &case.id)?;
        case.usage.activations = case.usage.activations.saturating_add(1);
        storage::cases::update_usage(&transaction, &case.id, &case.usage, now)?;
        break Some(ClaimedCase {
            case,
            attempts: work.attempts,
        });
    };
    commit(transaction)?;
    Ok(claimed)
}

#[cfg(test)]
mod tests {
    use clankjob_core::case::Budgets;
    use clankjob_core::ids::WaitConditionId;
    use clankjob_core::wait::TIMER_KIND;

    use super::*;
    use crate::test_support::{TestDb, time};

    fn new_case() -> NewCase {
        NewCase {
            title: "Quote".to_owned(),
            goal: "Get a quote".to_owned(),
            owner: None,
            profile: None,
            llm: "default".to_owned(),
            model: None,
            budgets: Budgets::default(),
            instructions: Vec::new(),
            human_channels: None,
        }
    }

    fn put_to_sleep(connection: &Connection, case: &Case) -> WaitCondition {
        let condition = WaitCondition {
            id: WaitConditionId::generate(),
            case_id: case.id.clone(),
            kind: TIMER_KIND.to_owned(),
            params: serde_json::json!({}),
            next_check_at: Some(time(100)),
            deadline_at: None,
            status: WaitStatus::Active,
            created_at: time(1),
        };
        storage::waits::insert_wait(connection, &condition).unwrap();
        storage::cases::update_state(connection, &case.id, CaseState::Sleeping, time(1)).unwrap();
        storage::queue::remove(connection, &case.id).unwrap();
        condition
    }

    fn state(connection: &Connection, id: &CaseId) -> CaseState {
        load_case(connection, id).unwrap().state
    }

    #[test]
    fn created_case_is_claimed_once_and_counts_an_activation() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let case = create_case(&mut connection, &new_case(), time(0)).unwrap();

        // Act
        let claimed = claim_next(&mut connection, time(0), time(60)).unwrap().unwrap();

        // Assert
        assert_eq!(claimed.case.id, case.id);
        assert_eq!(claimed.case.state, CaseState::Running);
        assert_eq!(claimed.case.usage.activations, 1);
        assert!(claim_next(&mut connection, time(1), time(60)).unwrap().is_none());
    }

    #[test]
    fn firing_a_condition_wakes_the_case_once() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let case = create_case(&mut connection, &new_case(), time(0)).unwrap();
        let condition = put_to_sleep(&connection, &case);

        // Act
        let first = fire_condition(&mut connection, &condition, FireOutcome::Fired(vec![]), time(100)).unwrap();
        let second = fire_condition(&mut connection, &condition, FireOutcome::TimedOut, time(101)).unwrap();

        // Assert
        assert!(first);
        assert!(!second);
        assert_eq!(state(&connection, &case.id), CaseState::Pending);
        assert!(claim_next(&mut connection, time(100), time(160)).unwrap().is_some());
    }

    #[test]
    fn message_answers_the_open_question_and_a_second_answer_is_rejected() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let case = create_case(&mut connection, &new_case(), time(0)).unwrap();
        let request_id = HumanRequestId::generate();
        storage::human::insert_request(&connection, &request_id, &case.id, "Photo?", time(1)).unwrap();
        storage::cases::update_state(&connection, &case.id, CaseState::WaitingForHuman, time(1)).unwrap();

        // Act
        post_message(&mut connection, &case.id, "Here it is", time(2)).unwrap();
        let late = answer_request(
            &mut connection,
            &request_id,
            Answer {
                text: "Too late",
                via: "discord",
                responder: None,
            },
            time(3),
        );

        // Assert
        assert!(matches!(late, Err(EngineError::AlreadyResolved(_))));
        let request = storage::human::get_request(&connection, &request_id).unwrap().unwrap();
        assert_eq!(request.answer.as_deref(), Some("Here it is"));
        assert_eq!(state(&connection, &case.id), CaseState::Pending);
    }

    #[test]
    fn message_to_a_finished_case_reopens_it() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let case = create_case(&mut connection, &new_case(), time(0)).unwrap();
        storage::queue::remove(&connection, &case.id).unwrap();
        storage::cases::update_state(&connection, &case.id, CaseState::Completed, time(1)).unwrap();

        post_message(&mut connection, &case.id, "One more thing", time(2)).unwrap();

        assert_eq!(state(&connection, &case.id), CaseState::Pending);
        assert!(claim_next(&mut connection, time(2), time(60)).unwrap().is_some());
    }

    #[test]
    fn manual_wake_only_applies_to_suspended_cases() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let case = create_case(&mut connection, &new_case(), time(0)).unwrap();

        assert!(matches!(
            wake_case(&mut connection, &case.id, time(1)),
            Err(EngineError::InvalidState { .. })
        ));
        put_to_sleep(&connection, &case);
        wake_case(&mut connection, &case.id, time(2)).unwrap();
        assert!(storage::waits::active_waits(&connection, &case.id).unwrap().is_empty());
        assert_eq!(state(&connection, &case.id), CaseState::Pending);
    }

    #[test]
    fn cancelled_case_is_never_claimed_and_rejects_messages() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let case = create_case(&mut connection, &new_case(), time(0)).unwrap();

        cancel_case(&mut connection, &case.id, time(1)).unwrap();

        assert!(claim_next(&mut connection, time(2), time(60)).unwrap().is_none());
        assert!(matches!(
            post_message(&mut connection, &case.id, "hi", time(3)),
            Err(EngineError::InvalidState { .. })
        ));
        assert!(matches!(
            cancel_case(&mut connection, &case.id, time(4)),
            Err(EngineError::InvalidState { .. })
        ));
    }

    #[test]
    fn claiming_a_case_rewoken_while_sleeping_cancels_its_waits() {
        // Arrange: the case went to sleep but a wake arrived during its activation.
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let case = create_case(&mut connection, &new_case(), time(0)).unwrap();
        put_to_sleep(&connection, &case);
        storage::queue::enqueue(&connection, &case.id, time(2)).unwrap();

        // Act
        let claimed = claim_next(&mut connection, time(2), time(60)).unwrap().unwrap();

        // Assert
        assert_eq!(claimed.case.state, CaseState::Running);
        assert!(storage::waits::active_waits(&connection, &case.id).unwrap().is_empty());
    }
}
