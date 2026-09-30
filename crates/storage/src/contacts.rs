//! The `contacts` table: the owner's address book.

use chrono::{DateTime, Utc};
use clankjob_core::contact::{Contact, ContactSource, NewContact};
use clankjob_core::ids::ContactId;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Result, from_millis, parse_enum, to_millis};

const COLUMNS: &str = "id, name, email, phone, note, trusted, added_by, created_at, updated_at";

fn contact_from_row(row: &Row<'_>) -> Result<Contact> {
    let added_by: String = row.get(6)?;
    Ok(Contact {
        id: ContactId::from_string(row.get::<_, String>(0)?),
        name: row.get(1)?,
        email: row.get(2)?,
        phone: row.get(3)?,
        note: row.get(4)?,
        trusted: row.get(5)?,
        added_by: parse_enum(&added_by)?,
        created_at: from_millis(row.get(7)?)?,
        updated_at: from_millis(row.get(8)?)?,
    })
}

/// Add a contact.
///
/// # Returns
///
/// The stored contact
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the insert fails.
pub fn insert_contact(
    connection: &Connection,
    contact: &NewContact,
    added_by: ContactSource,
    now: DateTime<Utc>,
) -> Result<Contact> {
    let id = ContactId::generate();
    connection.execute(
        &format!("INSERT INTO contacts ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)"),
        params![
            id.as_str(),
            contact.name,
            contact.email,
            contact.phone,
            contact.note,
            contact.trusted,
            added_by.as_str(),
            to_millis(now)
        ],
    )?;
    Ok(Contact {
        id,
        name: contact.name.clone(),
        email: contact.email.clone(),
        phone: contact.phone.clone(),
        note: contact.note.clone(),
        trusted: contact.trusted,
        added_by,
        created_at: now,
        updated_at: now,
    })
}

/// Fetch one contact.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails.
pub fn get_contact(connection: &Connection, id: &ContactId) -> Result<Option<Contact>> {
    let row = connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM contacts WHERE id = ?1"),
            params![id.as_str()],
            |row| Ok(contact_from_row(row)),
        )
        .optional()?;
    row.transpose()
}

/// Every contact, by name.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails.
pub fn list_contacts(connection: &Connection) -> Result<Vec<Contact>> {
    let mut statement = connection.prepare(&format!(
        "SELECT {COLUMNS} FROM contacts ORDER BY name COLLATE NOCASE, id"
    ))?;
    let rows = statement.query_map([], |row| Ok(contact_from_row(row)))?;
    rows.map(|row| row?).collect()
}

/// Replace a contact's fields.
///
/// # Returns
///
/// Whether the contact existed
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn update_contact(
    connection: &Connection,
    id: &ContactId,
    contact: &NewContact,
    now: DateTime<Utc>,
) -> Result<bool> {
    let changed = connection.execute(
        "UPDATE contacts SET name = ?2, email = ?3, phone = ?4, note = ?5, trusted = ?6, updated_at = ?7 WHERE id = ?1",
        params![
            id.as_str(),
            contact.name,
            contact.email,
            contact.phone,
            contact.note,
            contact.trusted,
            to_millis(now)
        ],
    )?;
    Ok(changed > 0)
}

/// Delete a contact.
///
/// # Returns
///
/// Whether the contact existed
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the delete fails.
pub fn delete_contact(connection: &Connection, id: &ContactId) -> Result<bool> {
    Ok(connection.execute("DELETE FROM contacts WHERE id = ?1", params![id.as_str()])? > 0)
}

/// The email addresses of trusted contacts, lowercased: emails to these alone need no
/// approval.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails.
pub fn trusted_emails(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT lower(trim(email)) FROM contacts WHERE trusted = 1 AND trim(coalesce(email, '')) <> ''",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    fn db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::new(dir.path().join("test.db"));
        db.migrate().unwrap();
        let connection = db.connect().unwrap();
        (dir, connection)
    }

    #[test]
    fn contacts_are_added_listed_by_name_edited_and_deleted() {
        let (_dir, connection) = db();
        let now = Utc::now();
        let robin = NewContact {
            name: "Robin Tremblay".to_owned(),
            email: Some("robin@sparky.ca".to_owned()),
            ..NewContact::default()
        };
        let stored = insert_contact(&connection, &robin, ContactSource::Agent, now).unwrap();
        insert_contact(
            &connection,
            &NewContact {
                name: "alex".to_owned(),
                ..NewContact::default()
            },
            ContactSource::Owner,
            now,
        )
        .unwrap();

        let names: Vec<String> = list_contacts(&connection)
            .unwrap()
            .into_iter()
            .map(|contact| contact.name)
            .collect();
        assert_eq!(names, ["alex", "Robin Tremblay"]);
        assert!(trusted_emails(&connection).unwrap().is_empty());

        let trusted = NewContact {
            trusted: true,
            email: Some(" Robin@Sparky.ca ".to_owned()),
            ..robin
        };
        assert!(update_contact(&connection, &stored.id, &trusted, now).unwrap());
        assert_eq!(trusted_emails(&connection).unwrap(), ["robin@sparky.ca"]);
        assert_eq!(
            get_contact(&connection, &stored.id).unwrap().unwrap().added_by,
            ContactSource::Agent
        );

        assert!(delete_contact(&connection, &stored.id).unwrap());
        assert!(!delete_contact(&connection, &stored.id).unwrap());
        assert!(get_contact(&connection, &stored.id).unwrap().is_none());
    }
}
