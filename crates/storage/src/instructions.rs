//! The `instructions` table: owner-written guidance, always in a case's system prompt.

use chrono::{DateTime, Utc};
use clankjob_core::case::{Instruction, NewInstruction};
use clankjob_core::ids::{CaseId, InstructionId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Result, from_millis, to_millis};

const COLUMNS: &str = "id, case_id, name, content, created_at, updated_at";

fn instruction_from_row(row: &Row<'_>) -> Result<Instruction> {
    Ok(Instruction {
        id: InstructionId::from_string(row.get::<_, String>(0)?),
        case_id: CaseId::from_string(row.get::<_, String>(1)?),
        name: row.get(2)?,
        content: row.get(3)?,
        created_at: from_millis(row.get(4)?)?,
        updated_at: from_millis(row.get(5)?)?,
    })
}

/// Add an instruction to a case.
///
/// # Returns
///
/// The stored instruction
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the insert fails.
pub fn insert_instruction(
    connection: &Connection,
    case_id: &CaseId,
    instruction: &NewInstruction,
    now: DateTime<Utc>,
) -> Result<Instruction> {
    let stored = Instruction {
        id: InstructionId::generate(),
        case_id: case_id.clone(),
        name: instruction.name.clone(),
        content: instruction.content.clone(),
        created_at: now,
        updated_at: now,
    };
    connection.execute(
        &format!("INSERT INTO instructions ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?5)"),
        params![
            stored.id.as_str(),
            case_id.as_str(),
            stored.name,
            stored.content,
            to_millis(now)
        ],
    )?;
    Ok(stored)
}

/// Replace an instruction's name and text.
///
/// # Returns
///
/// `false` if the case has no instruction with this id
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn update_instruction(
    connection: &Connection,
    case_id: &CaseId,
    id: &InstructionId,
    instruction: &NewInstruction,
    now: DateTime<Utc>,
) -> Result<bool> {
    Ok(connection.execute(
        "UPDATE instructions SET name = ?3, content = ?4, updated_at = ?5 WHERE case_id = ?1 AND id = ?2",
        params![
            case_id.as_str(),
            id.as_str(),
            instruction.name,
            instruction.content,
            to_millis(now)
        ],
    )? > 0)
}

/// Remove an instruction.
///
/// # Returns
///
/// `false` if the case has no instruction with this id
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the delete fails.
pub fn delete_instruction(connection: &Connection, case_id: &CaseId, id: &InstructionId) -> Result<bool> {
    Ok(connection.execute(
        "DELETE FROM instructions WHERE case_id = ?1 AND id = ?2",
        params![case_id.as_str(), id.as_str()],
    )? > 0)
}

/// One instruction of a case.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or the row is corrupt.
pub fn get_instruction(connection: &Connection, case_id: &CaseId, id: &InstructionId) -> Result<Option<Instruction>> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM instructions WHERE case_id = ?1 AND id = ?2"
    ))?;
    statement
        .query_row(params![case_id.as_str(), id.as_str()], |row| {
            Ok(instruction_from_row(row))
        })
        .optional()?
        .transpose()
}

/// Every instruction of a case, in the order they were added.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn list_instructions(connection: &Connection, case_id: &CaseId) -> Result<Vec<Instruction>> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM instructions WHERE case_id = ?1 ORDER BY rowid"
    ))?;
    let rows = statement.query_map([case_id.as_str()], |row| Ok(instruction_from_row(row)))?;
    rows.map(|row| row?).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDb, insert_case, time};

    fn instruction(name: &str, content: &str) -> NewInstruction {
        NewInstruction {
            name: name.to_owned(),
            content: content.to_owned(),
        }
    }

    #[test]
    fn instructions_are_added_edited_removed_and_scoped_to_their_case() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let other_case = insert_case(&connection);
        let tone = insert_instruction(&connection, &case_id, &instruction("tone.md", "Be polite."), time(1)).unwrap();
        let budget =
            insert_instruction(&connection, &case_id, &instruction("budget.md", "Max $1,500."), time(2)).unwrap();

        // Act
        let edited = update_instruction(
            &connection,
            &case_id,
            &tone.id,
            &instruction("tone.md", "Be firm."),
            time(3),
        )
        .unwrap();
        let wrong_case =
            update_instruction(&connection, &other_case, &tone.id, &instruction("x", "y"), time(4)).unwrap();
        let removed = delete_instruction(&connection, &case_id, &budget.id).unwrap();

        // Assert
        assert!(edited && removed && !wrong_case);
        let listed = list_instructions(&connection, &case_id).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            (listed[0].content.as_str(), listed[0].updated_at),
            ("Be firm.", time(3))
        );
        assert_eq!(listed[0].created_at, time(1));
        assert_eq!(
            get_instruction(&connection, &case_id, &tone.id).unwrap(),
            listed.first().cloned()
        );
        assert!(get_instruction(&connection, &other_case, &tone.id).unwrap().is_none());
        assert!(!delete_instruction(&connection, &case_id, &budget.id).unwrap());
    }
}
