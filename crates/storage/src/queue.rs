//! The `work_queue` table: which cases should run, and which worker holds each one.
//!
//! There is at most one row per case. A worker claims a row by setting a lease; while the
//! lease is valid no other worker can claim it, which guarantees one activation per case.
//! A wake that arrives while the case is running sets `rerun`, so the case runs again once
//! the current activation finishes.

use chrono::{DateTime, Utc};
use clankjob_core::ids::CaseId;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, to_millis};

/// A queue row claimed by a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedWork {
    /// The case to run.
    pub case_id: CaseId,
    /// How many times this work was released for a retry.
    pub attempts: u32,
}

/// Ask for a case to run no earlier than `available_at`.
///
/// If the case is already queued, the earlier time wins. If it is running, it is marked to
/// run again when the current activation finishes.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the upsert fails.
pub fn enqueue(connection: &Connection, case_id: &CaseId, available_at: DateTime<Utc>) -> Result<()> {
    connection.execute(
        "INSERT INTO work_queue (case_id, available_at) VALUES (?1, ?2) \
         ON CONFLICT (case_id) DO UPDATE SET \
             available_at = MIN(available_at, excluded.available_at), \
             rerun = CASE WHEN lease_until IS NULL THEN rerun ELSE 1 END",
        params![case_id.as_str(), to_millis(available_at)],
    )?;
    Ok(())
}

/// Claim the next case that is due and not leased (or whose lease expired).
///
/// Call inside a write transaction so the select and the update are atomic.
///
/// # Arguments
///
/// * `connection` - A write transaction
/// * `now` - Current time
/// * `lease_until` - When the claim expires unless renewed
///
/// # Returns
///
/// The claimed work, or `None` if nothing is due
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if a query fails.
pub fn claim_next(
    connection: &Connection,
    now: DateTime<Utc>,
    lease_until: DateTime<Utc>,
) -> Result<Option<ClaimedWork>> {
    let now = to_millis(now);
    let claimed = connection
        .query_row(
            "SELECT case_id, attempts FROM work_queue \
             WHERE available_at <= ?1 AND (lease_until IS NULL OR lease_until < ?1) \
             ORDER BY available_at LIMIT 1",
            [now],
            |row| {
                Ok(ClaimedWork {
                    case_id: CaseId::from_string(row.get::<_, String>(0)?),
                    attempts: row.get(1)?,
                })
            },
        )
        .optional()?;
    if let Some(work) = &claimed {
        connection.execute(
            "UPDATE work_queue SET lease_until = ?2, rerun = 0 WHERE case_id = ?1",
            params![work.case_id.as_str(), to_millis(lease_until)],
        )?;
    }
    Ok(claimed)
}

/// Extend the lease on a claimed case.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn renew_lease(connection: &Connection, case_id: &CaseId, lease_until: DateTime<Utc>) -> Result<()> {
    connection.execute(
        "UPDATE work_queue SET lease_until = ?2 WHERE case_id = ?1",
        params![case_id.as_str(), to_millis(lease_until)],
    )?;
    Ok(())
}

/// Finish a claimed case: drop its row, unless a wake arrived meanwhile.
///
/// # Returns
///
/// `true` if the case was queued again because of a wake during the activation
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if a query fails.
pub fn finish(connection: &Connection, case_id: &CaseId, now: DateTime<Utc>) -> Result<bool> {
    let requeued = connection.execute(
        "UPDATE work_queue SET lease_until = NULL, rerun = 0, attempts = 0, available_at = ?2 \
         WHERE case_id = ?1 AND rerun = 1",
        params![case_id.as_str(), to_millis(now)],
    )? > 0;
    if !requeued {
        connection.execute("DELETE FROM work_queue WHERE case_id = ?1", [case_id.as_str()])?;
    }
    Ok(requeued)
}

/// Give a claimed case back to the queue for a retry at `available_at`.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn release_for_retry(connection: &Connection, case_id: &CaseId, available_at: DateTime<Utc>) -> Result<()> {
    connection.execute(
        "UPDATE work_queue SET lease_until = NULL, available_at = ?2, attempts = attempts + 1 WHERE case_id = ?1",
        params![case_id.as_str(), to_millis(available_at)],
    )?;
    Ok(())
}

/// Remove a case from the queue whether or not it is claimed (used by cancel).
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the delete fails.
pub fn remove(connection: &Connection, case_id: &CaseId) -> Result<()> {
    connection.execute("DELETE FROM work_queue WHERE case_id = ?1", [case_id.as_str()])?;
    Ok(())
}

/// Drop every lease. Called at startup: a fresh process owns no work (design §18.4).
///
/// # Returns
///
/// How many leases were cleared
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn clear_leases(connection: &Connection) -> Result<usize> {
    Ok(connection.execute(
        "UPDATE work_queue SET lease_until = NULL WHERE lease_until IS NOT NULL",
        [],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDb, insert_case, time};

    #[test]
    fn claim_respects_availability_and_leases() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        enqueue(&connection, &case_id, time(10)).unwrap();

        // Act / Assert: not due yet
        assert!(claim_next(&connection, time(5), time(100)).unwrap().is_none());
        // Due: claimed
        let work = claim_next(&connection, time(10), time(100)).unwrap().unwrap();
        assert_eq!(work.case_id, case_id);
        // Leased: nobody else can claim it
        assert!(claim_next(&connection, time(50), time(200)).unwrap().is_none());
        // Lease expired: claimable again
        assert!(claim_next(&connection, time(101), time(300)).unwrap().is_some());
    }

    #[test]
    fn enqueue_keeps_the_earliest_time() {
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);

        enqueue(&connection, &case_id, time(10)).unwrap();
        enqueue(&connection, &case_id, time(5)).unwrap();
        enqueue(&connection, &case_id, time(20)).unwrap();

        assert!(claim_next(&connection, time(5), time(100)).unwrap().is_some());
    }

    #[test]
    fn wake_during_activation_requeues_on_finish() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        enqueue(&connection, &case_id, time(0)).unwrap();
        claim_next(&connection, time(0), time(100)).unwrap().unwrap();

        // Act: a wake arrives while running, then the activation finishes
        enqueue(&connection, &case_id, time(1)).unwrap();
        let requeued = finish(&connection, &case_id, time(2)).unwrap();

        // Assert
        assert!(requeued);
        assert!(claim_next(&connection, time(2), time(100)).unwrap().is_some());
    }

    #[test]
    fn finish_without_wake_removes_the_row() {
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        enqueue(&connection, &case_id, time(0)).unwrap();
        claim_next(&connection, time(0), time(100)).unwrap().unwrap();

        let requeued = finish(&connection, &case_id, time(2)).unwrap();

        assert!(!requeued);
        assert!(claim_next(&connection, time(500), time(600)).unwrap().is_none());
    }

    #[test]
    fn retry_release_counts_attempts_and_delays() {
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        enqueue(&connection, &case_id, time(0)).unwrap();
        claim_next(&connection, time(0), time(100)).unwrap().unwrap();

        release_for_retry(&connection, &case_id, time(60)).unwrap();

        assert!(claim_next(&connection, time(30), time(100)).unwrap().is_none());
        assert_eq!(
            claim_next(&connection, time(60), time(100)).unwrap().unwrap().attempts,
            1
        );
    }

    #[test]
    fn clear_leases_makes_claimed_work_available() {
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        enqueue(&connection, &case_id, time(0)).unwrap();
        claim_next(&connection, time(0), time(100)).unwrap().unwrap();

        assert_eq!(clear_leases(&connection).unwrap(), 1);
        assert!(claim_next(&connection, time(1), time(100)).unwrap().is_some());
    }
}
