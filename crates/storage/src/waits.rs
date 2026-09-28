//! The `wait_conditions` table.

use chrono::{DateTime, Utc};
use clankjob_core::ids::{CaseId, WaitConditionId};
use clankjob_core::wait::{WaitCondition, WaitStatus};
use rusqlite::{Connection, Row, params};

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

/// Active conditions whose check time or deadline has passed, oldest first.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn due_waits(connection: &Connection, now: DateTime<Utc>, limit: u32) -> Result<Vec<WaitCondition>> {
    query_waits(
        connection,
        "status = 'active' AND (next_check_at <= ?1 OR deadline_at <= ?1) \
         ORDER BY MIN(COALESCE(next_check_at, deadline_at), COALESCE(deadline_at, next_check_at)) LIMIT ?2",
        params![to_millis(now), limit],
    )
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
