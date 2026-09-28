//! The `case_notes` table: durable facts the LLM keeps about a case.

use chrono::{DateTime, Utc};
use clankjob_core::case::CaseNote;
use clankjob_core::ids::CaseId;
use rusqlite::{Connection, params};

use crate::{Result, from_millis, to_millis};

/// Create or replace a note.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the upsert fails.
pub fn set_note(connection: &Connection, case_id: &CaseId, key: &str, value: &str, now: DateTime<Utc>) -> Result<()> {
    connection.execute(
        "INSERT INTO case_notes (case_id, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (case_id, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![case_id.as_str(), key, value, to_millis(now)],
    )?;
    Ok(())
}

/// Delete a note.
///
/// # Returns
///
/// `true` if the note existed
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the delete fails.
pub fn delete_note(connection: &Connection, case_id: &CaseId, key: &str) -> Result<bool> {
    Ok(connection.execute(
        "DELETE FROM case_notes WHERE case_id = ?1 AND key = ?2",
        params![case_id.as_str(), key],
    )? > 0)
}

/// All notes of a case, sorted by key.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn list_notes(connection: &Connection, case_id: &CaseId) -> Result<Vec<CaseNote>> {
    let mut statement =
        connection.prepare_cached("SELECT key, value, updated_at FROM case_notes WHERE case_id = ?1 ORDER BY key")?;
    let rows = statement.query_map([case_id.as_str()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (key, value, updated_at) = row?;
        Ok(CaseNote {
            key,
            value,
            updated_at: from_millis(updated_at)?,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDb, insert_case, time};

    #[test]
    fn notes_are_upserted_listed_and_deleted() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);

        // Act
        set_note(&connection, &case_id, "thread_ref", "a1", time(1)).unwrap();
        set_note(&connection, &case_id, "price", "100", time(2)).unwrap();
        set_note(&connection, &case_id, "price", "120", time(3)).unwrap();

        // Assert
        let notes = list_notes(&connection, &case_id).unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!((notes[0].key.as_str(), notes[0].value.as_str()), ("price", "120"));
        assert_eq!(notes[0].updated_at, time(3));
        assert!(delete_note(&connection, &case_id, "price").unwrap());
        assert!(!delete_note(&connection, &case_id, "price").unwrap());
        assert_eq!(list_notes(&connection, &case_id).unwrap().len(), 1);
    }
}
