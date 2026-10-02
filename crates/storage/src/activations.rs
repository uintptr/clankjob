//! The `activations` table: one row per awake period of a case.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use clankjob_core::case::CaseState;
use clankjob_core::ids::{ActivationId, CaseId};
use clankjob_core::llm::TokenUsage;
use rusqlite::{Connection, params};

use crate::{Result, to_millis};

/// Record the start of an activation.
///
/// # Arguments
///
/// * `connection` - Connection or transaction
/// * `id` - The new activation's id
/// * `case_id` - The case being run
/// * `prompt_hashes` - Content hash of every prompt template in use (design §7.4)
/// * `now` - Start time
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the insert fails.
pub fn start_activation(
    connection: &Connection,
    id: &ActivationId,
    case_id: &CaseId,
    prompt_hashes: &BTreeMap<String, String>,
    now: DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "INSERT INTO activations (id, case_id, started_at, prompt_hashes) VALUES (?1, ?2, ?3, ?4)",
        params![
            id.as_str(),
            case_id.as_str(),
            to_millis(now),
            serde_json::to_string(prompt_hashes)?
        ],
    )?;
    Ok(())
}

/// Record the end of an activation.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn end_activation(
    connection: &Connection,
    id: &ActivationId,
    end_state: CaseState,
    usage: TokenUsage,
    now: DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "UPDATE activations SET ended_at = ?2, end_state = ?3, usage = ?4 WHERE id = ?1",
        params![id.as_str(), to_millis(now), end_state.as_str(), serde_json::to_string(&usage)?],
    )?;
    Ok(())
}

/// How many activations of a case started at or after `since`, for the daily budget.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails.
pub fn count_started_since(connection: &Connection, case_id: &CaseId, since: DateTime<Utc>) -> Result<u32> {
    // Served by the (case_id, started_at) index.
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM activations WHERE case_id = ?1 AND started_at >= ?2",
        params![case_id.as_str(), to_millis(since)],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDb, insert_case, time};

    #[test]
    fn activation_start_and_end_are_recorded() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let id = ActivationId::generate();
        let hashes = BTreeMap::from([("system".to_owned(), "abc".to_owned())]);

        // Act
        start_activation(&connection, &id, &case_id, &hashes, time(1)).unwrap();
        end_activation(
            &connection,
            &id,
            CaseState::Sleeping,
            TokenUsage {
                input_tokens: 5,
                output_tokens: 2,
            },
            time(9),
        )
        .unwrap();

        // Assert
        let (end_state, stored_hashes): (String, String) = connection
            .query_row(
                "SELECT end_state, prompt_hashes FROM activations WHERE id = ?1",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(end_state, "sleeping");
        assert_eq!(stored_hashes, r#"{"system":"abc"}"#);
    }

    #[test]
    fn activations_are_counted_from_a_point_in_time() {
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let other = insert_case(&connection);
        for (case, at) in [(&case_id, 1), (&case_id, 5), (&case_id, 9), (&other, 9)] {
            start_activation(&connection, &ActivationId::generate(), case, &BTreeMap::new(), time(at)).unwrap();
        }

        assert_eq!(count_started_since(&connection, &case_id, time(5)).unwrap(), 2);
        assert_eq!(count_started_since(&connection, &case_id, time(10)).unwrap(), 0);
    }
}
