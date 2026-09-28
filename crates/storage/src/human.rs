//! The `human_requests` table.

use chrono::{DateTime, Utc};
use clankjob_core::human::{HumanRequest, HumanRequestStatus};
use clankjob_core::ids::{CaseId, HumanRequestId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Result, from_millis, from_optional_millis, parse_enum, to_millis};

const COLUMNS: &str = "id, case_id, question, status, answer, answered_via, responder, created_at, resolved_at";

/// How a request was answered, for [`answer_request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer<'a> {
    /// The answer text.
    pub text: &'a str,
    /// Channel it came from, e.g. `web`.
    pub via: &'a str,
    /// Who answered, as identified by the channel.
    pub responder: Option<&'a str>,
}

fn request_from_row(row: &Row<'_>) -> Result<HumanRequest> {
    let status: String = row.get(3)?;
    Ok(HumanRequest {
        id: HumanRequestId::from_string(row.get::<_, String>(0)?),
        case_id: CaseId::from_string(row.get::<_, String>(1)?),
        question: row.get(2)?,
        status: parse_enum(&status)?,
        answer: row.get(4)?,
        answered_via: row.get(5)?,
        responder: row.get(6)?,
        created_at: from_millis(row.get(7)?)?,
        resolved_at: from_optional_millis(row.get(8)?)?,
    })
}

/// Store a new open request.
///
/// # Returns
///
/// The stored request
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the insert fails.
pub fn insert_request(
    connection: &Connection,
    id: &HumanRequestId,
    case_id: &CaseId,
    question: &str,
    now: DateTime<Utc>,
) -> Result<HumanRequest> {
    connection.execute(
        &format!("INSERT INTO human_requests ({COLUMNS}) VALUES (?1, ?2, ?3, 'open', NULL, NULL, NULL, ?4, NULL)"),
        params![id.as_str(), case_id.as_str(), question, to_millis(now)],
    )?;
    Ok(HumanRequest {
        id: id.clone(),
        case_id: case_id.clone(),
        question: question.to_owned(),
        status: HumanRequestStatus::Open,
        answer: None,
        answered_via: None,
        responder: None,
        created_at: now,
        resolved_at: None,
    })
}

/// Fetch one request.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or the row is corrupt.
pub fn get_request(connection: &Connection, id: &HumanRequestId) -> Result<Option<HumanRequest>> {
    let mut statement = connection.prepare_cached(&format!("SELECT {COLUMNS} FROM human_requests WHERE id = ?1"))?;
    statement
        .query_row([id.as_str()], |row| Ok(request_from_row(row)))
        .optional()?
        .transpose()
}

/// List requests, oldest first, optionally filtered by status and case.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn list_requests(
    connection: &Connection,
    status: Option<HumanRequestStatus>,
    case_id: Option<&CaseId>,
) -> Result<Vec<HumanRequest>> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM human_requests WHERE (?1 IS NULL OR status = ?1) AND (?2 IS NULL OR case_id = ?2) \
         ORDER BY created_at, rowid"
    ))?;
    let rows = statement.query_map(
        params![status.map(HumanRequestStatus::as_str), case_id.map(CaseId::as_str)],
        |row| Ok(request_from_row(row)),
    )?;
    rows.map(|row| row?).collect()
}

/// Record the answer to a request, only if it is still open (first answer wins).
///
/// # Returns
///
/// `true` if this call answered the request, `false` if it was no longer open
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn answer_request(
    connection: &Connection,
    id: &HumanRequestId,
    answer: Answer<'_>,
    now: DateTime<Utc>,
) -> Result<bool> {
    Ok(connection.execute(
        "UPDATE human_requests SET status = 'answered', answer = ?2, answered_via = ?3, responder = ?4, \
         resolved_at = ?5 WHERE id = ?1 AND status = 'open'",
        params![id.as_str(), answer.text, answer.via, answer.responder, to_millis(now)],
    )? > 0)
}

/// Close every open request of a case without an answer (superseded or cancelled).
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn close_open_requests(
    connection: &Connection,
    case_id: &CaseId,
    status: HumanRequestStatus,
    now: DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "UPDATE human_requests SET status = ?2, resolved_at = ?3 WHERE case_id = ?1 AND status = 'open'",
        params![case_id.as_str(), status.as_str(), to_millis(now)],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDb, insert_case, time};

    const WEB: Answer<'static> = Answer {
        text: "yes",
        via: "web",
        responder: Some("joe"),
    };

    #[test]
    fn first_answer_wins() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let id = HumanRequestId::generate();
        insert_request(&connection, &id, &case_id, "Proceed?", time(1)).unwrap();

        // Act
        let first = answer_request(&connection, &id, WEB, time(2)).unwrap();
        let second = answer_request(
            &connection,
            &id,
            Answer {
                text: "no",
                via: "discord",
                responder: None,
            },
            time(3),
        )
        .unwrap();

        // Assert
        assert!(first);
        assert!(!second);
        let request = get_request(&connection, &id).unwrap().unwrap();
        assert_eq!(request.status, HumanRequestStatus::Answered);
        assert_eq!(request.answer.as_deref(), Some("yes"));
        assert_eq!(request.answered_via.as_deref(), Some("web"));
        assert_eq!(request.resolved_at, Some(time(2)));
    }

    #[test]
    fn list_filters_and_close_supersedes_open_requests() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let other_case = insert_case(&connection);
        insert_request(&connection, &HumanRequestId::generate(), &case_id, "A?", time(1)).unwrap();
        insert_request(&connection, &HumanRequestId::generate(), &other_case, "B?", time(2)).unwrap();

        // Act
        close_open_requests(&connection, &case_id, HumanRequestStatus::Superseded, time(3)).unwrap();

        // Assert
        let open = list_requests(&connection, Some(HumanRequestStatus::Open), None).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].question, "B?");
        let for_case = list_requests(&connection, None, Some(&case_id)).unwrap();
        assert_eq!(for_case[0].status, HumanRequestStatus::Superseded);
    }
}
