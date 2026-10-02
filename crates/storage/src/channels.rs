//! The `channel_deliveries` outbox and `channel_cursors` (design §10.3).

use chrono::{DateTime, Utc};
use clankjob_core::channel::{ChannelDelivery, DeliveryKind, DeliveryStatus, OpenDelivery};
use clankjob_core::ids::{CaseId, DeliveryId, HumanRequestId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;

use crate::{Result, from_millis, parse_enum, to_millis};

const COLUMNS: &str = "id, case_id, human_request_id, channel, kind, payload, status, external, attempts, \
                       next_attempt_at, last_error, created_at";

fn delivery_from_row(row: &Row<'_>) -> Result<ChannelDelivery> {
    let kind: String = row.get(4)?;
    let payload: String = row.get(5)?;
    let status: String = row.get(6)?;
    let external: Option<String> = row.get(7)?;
    Ok(ChannelDelivery {
        id: DeliveryId::from_string(row.get::<_, String>(0)?),
        case_id: CaseId::from_string(row.get::<_, String>(1)?),
        human_request_id: row.get::<_, Option<String>>(2)?.map(HumanRequestId::from_string),
        channel: row.get(3)?,
        kind: parse_enum(&kind)?,
        payload: serde_json::from_str(&payload)?,
        status: parse_enum(&status)?,
        external: external.as_deref().map(serde_json::from_str).transpose()?,
        attempts: row.get(8)?,
        next_attempt_at: from_millis(row.get(9)?)?,
        last_error: row.get(10)?,
        created_at: from_millis(row.get(11)?)?,
    })
}

/// Queue a message for a channel, to be sent as soon as possible.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the insert fails.
pub fn insert_delivery(
    connection: &Connection,
    case_id: &CaseId,
    human_request_id: Option<&HumanRequestId>,
    channel: &str,
    kind: DeliveryKind,
    payload: &Value,
    now: DateTime<Utc>,
) -> Result<DeliveryId> {
    let id = DeliveryId::generate();
    connection.execute(
        "INSERT INTO channel_deliveries (id, case_id, human_request_id, channel, kind, payload, status, \
         next_attempt_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?7, ?7)",
        params![
            id.as_str(),
            case_id.as_str(),
            human_request_id.map(HumanRequestId::as_str),
            channel,
            kind.as_str(),
            serde_json::to_string(payload)?,
            to_millis(now)
        ],
    )?;
    Ok(id)
}

/// Pending messages due for sending, oldest first.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn due_deliveries(connection: &Connection, now: DateTime<Utc>, limit: u32) -> Result<Vec<ChannelDelivery>> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM channel_deliveries WHERE status = 'pending' AND next_attempt_at <= ?1 \
         ORDER BY rowid LIMIT ?2"
    ))?;
    let rows = statement.query_map(params![to_millis(now), limit], |row| Ok(delivery_from_row(row)))?;
    rows.map(|row| row?).collect()
}

/// Record that a message was sent, with what the channel returned.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn mark_sent(connection: &Connection, id: &DeliveryId, external: Option<&Value>, now: DateTime<Utc>) -> Result<()> {
    connection.execute(
        "UPDATE channel_deliveries SET status = 'sent', external = ?2, last_error = NULL, updated_at = ?3 \
         WHERE id = ?1",
        params![id.as_str(), external.map(serde_json::to_string).transpose()?, to_millis(now)],
    )?;
    Ok(())
}

/// Record a failed attempt and when to try again.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn mark_retry(
    connection: &Connection,
    id: &DeliveryId,
    error: &str,
    next_attempt_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "UPDATE channel_deliveries SET attempts = attempts + 1, last_error = ?2, next_attempt_at = ?3, \
         updated_at = ?4 WHERE id = ?1",
        params![id.as_str(), error, to_millis(next_attempt_at), to_millis(now)],
    )?;
    Ok(())
}

/// Stop trying: the message failed for good, or is no longer needed.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn mark_finished(
    connection: &Connection,
    id: &DeliveryId,
    status: DeliveryStatus,
    error: Option<&str>,
    now: DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "UPDATE channel_deliveries SET status = ?2, last_error = COALESCE(?3, last_error), updated_at = ?4 \
         WHERE id = ?1",
        params![id.as_str(), status.as_str(), error, to_millis(now)],
    )?;
    Ok(())
}

/// Channels a question was queued for.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails.
pub fn request_channels(connection: &Connection, request_id: &HumanRequestId) -> Result<Vec<String>> {
    let mut statement = connection.prepare_cached(
        "SELECT DISTINCT channel FROM channel_deliveries WHERE human_request_id = ?1 AND kind = 'request' \
         ORDER BY channel",
    )?;
    let rows = statement.query_map([request_id.as_str()], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The message that posted a question to a channel, if one was queued.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or the row is corrupt.
pub fn request_delivery(
    connection: &Connection,
    request_id: &HumanRequestId,
    channel: &str,
) -> Result<Option<ChannelDelivery>> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM channel_deliveries WHERE human_request_id = ?1 AND channel = ?2 \
         AND kind = 'request' ORDER BY rowid DESC LIMIT 1"
    ))?;
    statement
        .query_row(params![request_id.as_str(), channel], |row| Ok(delivery_from_row(row)))
        .optional()?
        .transpose()
}

/// Questions posted to a channel that are still open, to poll for answers.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn open_deliveries(connection: &Connection, channel: &str) -> Result<Vec<OpenDelivery>> {
    let mut statement = connection.prepare_cached(
        "SELECT d.human_request_id, d.external FROM channel_deliveries d \
         JOIN human_requests r ON r.id = d.human_request_id \
         WHERE d.channel = ?1 AND d.kind = 'request' AND d.status = 'sent' AND r.status = 'open' \
         ORDER BY d.rowid",
    )?;
    let rows = statement.query_map([channel], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    rows.map(|row| {
        let (request_id, external) = row?;
        Ok(OpenDelivery {
            request_id: HumanRequestId::from_string(request_id),
            delivery: external
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or(Value::Null),
        })
    })
    .collect()
}

/// Every message queued for a case, oldest first.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn list_deliveries(connection: &Connection, case_id: &CaseId) -> Result<Vec<ChannelDelivery>> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM channel_deliveries WHERE case_id = ?1 ORDER BY rowid"
    ))?;
    let rows = statement.query_map([case_id.as_str()], |row| Ok(delivery_from_row(row)))?;
    rows.map(|row| row?).collect()
}

/// Where a channel's poll continues; `null` before the first poll.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or the value is corrupt.
pub fn get_cursor(connection: &Connection, channel: &str) -> Result<Value> {
    let cursor: Option<String> = connection
        .query_row(
            "SELECT cursor FROM channel_cursors WHERE channel = ?1",
            [channel],
            |row| row.get(0),
        )
        .optional()?;
    Ok(cursor.as_deref().map(serde_json::from_str).transpose()?.unwrap_or(Value::Null))
}

/// Store where a channel's poll continues.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the write fails.
pub fn set_cursor(connection: &Connection, channel: &str, cursor: &Value, now: DateTime<Utc>) -> Result<()> {
    connection.execute(
        "INSERT INTO channel_cursors (channel, cursor, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (channel) DO UPDATE SET cursor = excluded.cursor, updated_at = excluded.updated_at",
        params![channel, serde_json::to_string(cursor)?, to_millis(now)],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::human::{Answer, answer_request, insert_request};
    use crate::test_support::{TestDb, insert_case, time};

    #[test]
    fn deliveries_are_sent_retried_and_polled_only_while_the_question_is_open() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let request_id = HumanRequestId::generate();
        insert_request(&connection, &request_id, &case_id, "Photo?", time(1)).unwrap();
        let payload = json!({ "kind": "question", "text": "Photo?" });
        let id = insert_delivery(
            &connection,
            &case_id,
            Some(&request_id),
            "discord_joe",
            DeliveryKind::Request,
            &payload,
            time(1),
        )
        .unwrap();

        // Act
        mark_retry(&connection, &id, "rate limited", time(10), time(2)).unwrap();
        let not_yet = due_deliveries(&connection, time(5), 10).unwrap();
        let due = due_deliveries(&connection, time(10), 10).unwrap();
        mark_sent(&connection, &id, Some(&json!({ "thread_id": "7" })), time(11)).unwrap();
        let open = open_deliveries(&connection, "discord_joe").unwrap();
        let answer = Answer {
            text: "here",
            via: "web",
            responder: None,
        };
        answer_request(&connection, &request_id, answer, time(12)).unwrap();

        // Assert
        assert_eq!(not_yet, [] as [ChannelDelivery; 0]);
        assert_eq!((due.len(), due[0].attempts, due[0].payload.clone()), (1, 1, payload));
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].delivery, json!({ "thread_id": "7" }));
        assert_eq!(
            open_deliveries(&connection, "discord_joe").unwrap(),
            [] as [OpenDelivery; 0]
        );
        assert_eq!(request_channels(&connection, &request_id).unwrap(), ["discord_joe"]);
        let sent = request_delivery(&connection, &request_id, "discord_joe").unwrap().unwrap();
        assert_eq!((sent.status, sent.last_error), (DeliveryStatus::Sent, None));
        assert_eq!(list_deliveries(&connection, &case_id).unwrap().len(), 1);
    }

    #[test]
    fn cursor_round_trips_and_defaults_to_null() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();

        // Act
        let before = get_cursor(&connection, "discord_joe").unwrap();
        set_cursor(&connection, "discord_joe", &json!({ "1": "2" }), time(1)).unwrap();
        set_cursor(&connection, "discord_joe", &json!({ "1": "3" }), time(2)).unwrap();

        // Assert
        assert_eq!(before, Value::Null);
        assert_eq!(get_cursor(&connection, "discord_joe").unwrap(), json!({ "1": "3" }));
    }
}
