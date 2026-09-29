//! The `wait_conditions` table.

use chrono::{DateTime, Utc};
use clankjob_core::ids::{CaseId, WaitConditionId};
use clankjob_core::wait::{WaitCondition, WaitStatus};
use rusqlite::{Connection, Row, params};
use serde_json::Value;

use crate::{Result, from_millis, from_optional_millis, parse_enum, to_millis};

const COLUMNS: &str = "id, case_id, kind, params, next_check_at, deadline_at, status, created_at";

fn wait_from_row(row: &Row<'_>) -> Result<WaitCondition> {
    let params: String = row.get(3)?;
    let status: String = row.get(6)?;
    Ok(WaitCondition {
        id: WaitConditionId::from_string(row.get::<_, String>(0)?),
        case_id: CaseId::from_string(row.get::<_, String>(1)?),
        kind: row.get(2)?,
        params: serde_json::from_str(&params)?,
        next_check_at: from_optional_millis(row.get(4)?)?,
        deadline_at: from_optional_millis(row.get(5)?)?,
        status: parse_enum(&status)?,
        created_at: from_millis(row.get(7)?)?,
    })
}

fn query_waits<P>(connection: &Connection, filter: &str, params: P) -> Result<Vec<WaitCondition>>
where
    P: rusqlite::Params,
{
    let mut statement = connection.prepare_cached(&format!("SELECT {COLUMNS} FROM wait_conditions WHERE {filter}"))?;
    let rows = statement.query_map(params, |row| Ok(wait_from_row(row)))?;
    rows.map(|row| row?).collect()
}

/// Store a new wait condition.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the insert fails.
pub fn insert_wait(connection: &Connection, wait: &WaitCondition) -> Result<()> {
    connection.execute(
        &format!("INSERT INTO wait_conditions ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
        params![
            wait.id.as_str(),
            wait.case_id.as_str(),
            wait.kind,
            serde_json::to_string(&wait.params)?,
            wait.next_check_at.map(to_millis),
            wait.deadline_at.map(to_millis),
            wait.status.as_str(),
            to_millis(wait.created_at),
        ],
    )?;
    Ok(())
}

/// Active conditions the scheduler must act on, oldest first: built-in ones (`core.*`)
/// whose check time has passed, and any condition whose deadline has passed. Plugin
/// conditions are checked separately ([`due_checks`]).
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn due_waits(connection: &Connection, now: DateTime<Utc>, limit: u32) -> Result<Vec<WaitCondition>> {
    query_waits(
        connection,
        "status = 'active' AND ((kind LIKE 'core.%' AND next_check_at <= ?1) OR deadline_at <= ?1) \
         ORDER BY MIN(COALESCE(next_check_at, deadline_at), COALESCE(deadline_at, next_check_at)) LIMIT ?2",
        params![to_millis(now), limit],
    )
}

/// A plugin condition due for a check, with where its check continues.
#[derive(Debug, Clone, PartialEq)]
pub struct DueCheck {
    /// The condition.
    pub condition: WaitCondition,
    /// What the last check handed back.
    pub cursor: Option<Value>,
    /// How often it is checked, in milliseconds.
    pub every_ms: Option<i64>,
    /// Checks in a row that failed.
    pub failures: u32,
}

/// Active plugin conditions (not `core.*`) whose check time has passed, oldest first.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn due_checks(connection: &Connection, now: DateTime<Utc>, limit: u32) -> Result<Vec<DueCheck>> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {COLUMNS}, cursor, check_every_ms, failures FROM wait_conditions \
         WHERE status = 'active' AND kind NOT LIKE 'core.%' AND next_check_at <= ?1 \
         ORDER BY next_check_at LIMIT ?2"
    ))?;
    let rows = statement.query_map(params![to_millis(now), limit], |row| {
        let cursor: Option<String> = row.get(8)?;
        Ok((
            wait_from_row(row),
            cursor,
            row.get::<_, Option<i64>>(9)?,
            row.get::<_, u32>(10)?,
        ))
    })?;
    rows.map(|row| {
        let (condition, cursor, every_ms, failures) = row?;
        Ok(DueCheck {
            condition: condition?,
            cursor: cursor.as_deref().map(serde_json::from_str).transpose()?,
            every_ms,
            failures,
        })
    })
    .collect()
}

/// Set how often a plugin condition is checked.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn set_check_interval(connection: &Connection, id: &WaitConditionId, every_ms: i64) -> Result<()> {
    connection.execute(
        "UPDATE wait_conditions SET check_every_ms = ?2 WHERE id = ?1",
        params![id.as_str(), every_ms],
    )?;
    Ok(())
}

/// Record a check that did not fire: where to continue, when to check next, and how many
/// checks in a row failed.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn record_check(
    connection: &Connection,
    id: &WaitConditionId,
    cursor: Option<&Value>,
    next_check_at: DateTime<Utc>,
    failures: u32,
) -> Result<()> {
    connection.execute(
        "UPDATE wait_conditions SET cursor = COALESCE(?2, cursor), next_check_at = ?3, failures = ?4 \
         WHERE id = ?1 AND status = 'active'",
        params![
            id.as_str(),
            cursor.map(serde_json::to_string).transpose()?,
            to_millis(next_check_at),
            failures
        ],
    )?;
    Ok(())
}

/// Active conditions of one case.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn active_waits(connection: &Connection, case_id: &CaseId) -> Result<Vec<WaitCondition>> {
    query_waits(
        connection,
        "case_id = ?1 AND status = 'active' ORDER BY created_at",
        [case_id.as_str()],
    )
}

/// Set the status of an active condition.
///
/// # Returns
///
/// `true` if the condition was active and was updated
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn resolve_wait(connection: &Connection, id: &WaitConditionId, status: WaitStatus) -> Result<bool> {
    Ok(connection.execute(
        "UPDATE wait_conditions SET status = ?2 WHERE id = ?1 AND status = 'active'",
        params![id.as_str(), status.as_str()],
    )? > 0)
}

/// Resolve every active condition of a case of the given kind (e.g. `core.human_input`).
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn resolve_waits_of_kind(connection: &Connection, case_id: &CaseId, kind: &str, status: WaitStatus) -> Result<()> {
    connection.execute(
        "UPDATE wait_conditions SET status = ?3 WHERE case_id = ?1 AND kind = ?2 AND status = 'active'",
        params![case_id.as_str(), kind, status.as_str()],
    )?;
    Ok(())
}

/// Cancel every active condition of a case.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn cancel_waits(connection: &Connection, case_id: &CaseId) -> Result<()> {
    connection.execute(
        "UPDATE wait_conditions SET status = 'cancelled' WHERE case_id = ?1 AND status = 'active'",
        [case_id.as_str()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use clankjob_core::wait::{HUMAN_INPUT_KIND, TIMER_KIND};

    use super::*;
    use crate::test_support::{TestDb, insert_case, time};

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

    #[test]
    fn plugin_conditions_are_checked_separately_and_keep_their_cursor() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let reply = wait(&case_id, "email_reply_received", Some(10), None);
        insert_wait(&connection, &reply).unwrap();
        set_check_interval(&connection, &reply.id, 900_000).unwrap();

        // Act
        let scheduler_sees = due_waits(&connection, time(20), 10).unwrap();
        let due = due_checks(&connection, time(20), 10).unwrap();
        record_check(
            &connection,
            &reply.id,
            Some(&serde_json::json!({ "uid": 7 })),
            time(40),
            1,
        )
        .unwrap();
        let not_yet = due_checks(&connection, time(30), 10).unwrap();
        let later = due_checks(&connection, time(40), 10).unwrap();

        // Assert
        assert!(scheduler_sees.is_empty(), "the scheduler only fires built-in kinds");
        assert_eq!(
            (due.len(), due[0].every_ms, due[0].cursor.clone()),
            (1, Some(900_000), None)
        );
        assert!(not_yet.is_empty());
        assert_eq!(
            (later[0].cursor.clone(), later[0].failures),
            (Some(serde_json::json!({ "uid": 7 })), 1)
        );
    }

    #[test]
    fn due_waits_include_checks_and_deadlines_but_not_future_ones() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let timer = wait(&case_id, TIMER_KIND, Some(10), None);
        let human = wait(&case_id, HUMAN_INPUT_KIND, None, Some(20));
        let later = wait(&case_id, TIMER_KIND, Some(100), None);
        for condition in [&timer, &human, &later] {
            insert_wait(&connection, condition).unwrap();
        }

        // Act
        let due = due_waits(&connection, time(30), 10).unwrap();

        // Assert
        let ids: Vec<WaitConditionId> = due.into_iter().map(|condition| condition.id).collect();
        assert_eq!(ids, vec![timer.id, human.id]);
    }

    #[test]
    fn resolving_only_affects_active_conditions() {
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let condition = wait(&case_id, TIMER_KIND, Some(10), None);
        insert_wait(&connection, &condition).unwrap();

        assert!(resolve_wait(&connection, &condition.id, WaitStatus::Fired).unwrap());
        assert!(!resolve_wait(&connection, &condition.id, WaitStatus::TimedOut).unwrap());
        assert!(active_waits(&connection, &case_id).unwrap().is_empty());
    }

    #[test]
    fn cancel_and_kind_resolution_update_the_right_rows() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        insert_wait(&connection, &wait(&case_id, HUMAN_INPUT_KIND, None, None)).unwrap();
        let timer = wait(&case_id, TIMER_KIND, Some(10), None);
        insert_wait(&connection, &timer).unwrap();

        // Act
        resolve_waits_of_kind(&connection, &case_id, HUMAN_INPUT_KIND, WaitStatus::Fired).unwrap();

        // Assert
        let active = active_waits(&connection, &case_id).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, timer.id);
        cancel_waits(&connection, &case_id).unwrap();
        assert!(active_waits(&connection, &case_id).unwrap().is_empty());
    }
}
