//! The append-only `events` table.

use chrono::{DateTime, Utc};
use clankjob_core::event::{Event, EventBody};
use clankjob_core::ids::{ActivationId, CaseId};
use rusqlite::{Connection, Row, params};
use serde_json::Value;

use crate::{Result, StorageError, from_millis, to_millis};

fn event_from_row(row: &Row<'_>) -> Result<Event> {
    let kind: String = row.get(3)?;
    let payload: String = row.get(4)?;
    // Rebuild the adjacently tagged form `{"kind": ..., "payload": ...}` serde expects.
    // A unit variant such as `nudge` is stored with a `null` payload and has no content.
    let mut tagged = serde_json::Map::with_capacity(2);
    tagged.insert("kind".to_owned(), Value::String(kind));
    let payload: Value = serde_json::from_str(&payload)?;
    if !payload.is_null() {
        tagged.insert("payload".to_owned(), payload);
    }
    Ok(Event {
        seq: row.get(0)?,
        case_id: CaseId::from_string(row.get::<_, String>(1)?),
        activation_id: row.get::<_, Option<String>>(2)?.map(ActivationId::from_string),
        body: serde_json::from_value(Value::Object(tagged))?,
        created_at: from_millis(row.get(5)?)?,
    })
}

/// Append an event to a case's log.
///
/// # Arguments
///
/// * `connection` - Connection or transaction
/// * `case_id` - The case the event belongs to
/// * `activation_id` - The activation that produced it, if any
/// * `body` - What happened
/// * `now` - When it happened
///
/// # Returns
///
/// The event's sequence number
///
/// # Errors
///
/// Returns a [`StorageError`] if the insert fails.
pub fn append_event(
    connection: &Connection,
    case_id: &CaseId,
    activation_id: Option<&ActivationId>,
    body: &EventBody,
    now: DateTime<Utc>,
) -> Result<i64> {
    let payload = match serde_json::to_value(body)? {
        Value::Object(mut tagged) => tagged.remove("payload").unwrap_or(Value::Null),
        other => return Err(StorageError::Corrupt(format!("event body serialized as {other}"))),
    };
    connection.execute(
        "INSERT INTO events (case_id, activation_id, kind, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            case_id.as_str(),
            activation_id.map(ActivationId::as_str),
            body.kind(),
            serde_json::to_string(&payload)?,
            to_millis(now)
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

/// List a case's events in order.
///
/// # Arguments
///
/// * `connection` - Connection or transaction
/// * `case_id` - The case
/// * `after` - Only events with a greater sequence number (0 for all)
/// * `limit` - Maximum number of events, or `None` for all of them
///
/// # Errors
///
/// Returns a [`StorageError`] if the query fails or a row is corrupt.
pub fn list_events(connection: &Connection, case_id: &CaseId, after: i64, limit: Option<u32>) -> Result<Vec<Event>> {
    let mut statement = connection.prepare_cached(
        "SELECT seq, case_id, activation_id, kind, payload, created_at FROM events \
         WHERE case_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
    )?;
    // SQLite treats a negative LIMIT as "no limit".
    let limit = limit.map_or(-1, i64::from);
    let rows = statement.query_map(params![case_id.as_str(), after, limit], |row| Ok(event_from_row(row)))?;
    rows.map(|row| row?).collect()
}

#[cfg(test)]
mod tests {
    use clankjob_core::event::WakeReason;

    use super::*;
    use crate::test_support::{TestDb, insert_case, time};

    #[test]
    fn events_round_trip_in_order() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let activation_id = ActivationId::generate();
        let wake = EventBody::Wake(WakeReason::Created);

        // Act
        let first = append_event(&connection, &case_id, None, &wake, time(1)).unwrap();
        let second = append_event(&connection, &case_id, Some(&activation_id), &EventBody::Nudge, time(2)).unwrap();
        let events = list_events(&connection, &case_id, 0, None).unwrap();

        // Assert
        assert!(second > first);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].body, wake);
        assert_eq!(events[1].body, EventBody::Nudge);
        assert_eq!(events[1].activation_id, Some(activation_id));
    }

    #[test]
    fn after_and_limit_page_through_events() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let seqs: Vec<i64> = (0..3)
            .map(|index| append_event(&connection, &case_id, None, &EventBody::Nudge, time(index)).unwrap())
            .collect();

        // Act
        let page = list_events(&connection, &case_id, seqs[0], Some(1)).unwrap();

        // Assert
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].seq, seqs[1]);
    }
}
