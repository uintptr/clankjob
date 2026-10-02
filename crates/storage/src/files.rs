//! The `files` table: files uploaded for a case. The bytes themselves live on disk.

use clankjob_core::file::CaseFile;
use clankjob_core::ids::{CaseId, FileId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Result, StorageError, from_millis, parse_enum, to_millis};

const COLUMNS: &str = "id, case_id, name, media_type, kind, size, sha256, text, pages, created_at";

fn file_from_row(row: &Row<'_>) -> Result<CaseFile> {
    let kind: String = row.get(4)?;
    let size: i64 = row.get(5)?;
    let text: Option<String> = row.get(7)?;
    Ok(CaseFile {
        id: FileId::from_string(row.get::<_, String>(0)?),
        case_id: CaseId::from_string(row.get::<_, String>(1)?),
        name: row.get(2)?,
        media_type: row.get(3)?,
        kind: parse_enum(&kind)?,
        size: u64::try_from(size).map_err(|_| StorageError::Corrupt(format!("file size {size}")))?,
        sha256: row.get(6)?,
        text_chars: text.as_ref().map(|text| text.chars().count() as u64),
        text,
        pages: row.get(8)?,
        created_at: from_millis(row.get(9)?)?,
    })
}

/// Store a file's metadata and extracted text.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the insert fails.
pub fn insert_file(connection: &Connection, file: &CaseFile) -> Result<()> {
    connection.execute(
        &format!("INSERT INTO files ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
        params![
            file.id.as_str(),
            file.case_id.as_str(),
            file.name,
            file.media_type,
            file.kind.as_str(),
            i64::try_from(file.size).map_err(|_| StorageError::Corrupt(format!("file size {}", file.size)))?,
            file.sha256,
            file.text,
            file.pages,
            to_millis(file.created_at),
        ],
    )?;
    Ok(())
}

/// One file of a case.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or the row is corrupt.
pub fn get_file(connection: &Connection, case_id: &CaseId, id: &FileId) -> Result<Option<CaseFile>> {
    let mut statement =
        connection.prepare_cached(&format!("SELECT {COLUMNS} FROM files WHERE case_id = ?1 AND id = ?2"))?;
    statement
        .query_row(params![case_id.as_str(), id.as_str()], |row| Ok(file_from_row(row)))
        .optional()?
        .transpose()
}

/// Every file of a case, in upload order.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn list_files(connection: &Connection, case_id: &CaseId) -> Result<Vec<CaseFile>> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM files WHERE case_id = ?1 ORDER BY rowid"
    ))?;
    let rows = statement.query_map([case_id.as_str()], |row| Ok(file_from_row(row)))?;
    rows.map(|row| row?).collect()
}

#[cfg(test)]
mod tests {
    use clankjob_core::file::FileKind;

    use super::*;
    use crate::test_support::{TestDb, insert_case, time};

    fn file(case_id: &CaseId, name: &str, text: Option<&str>) -> CaseFile {
        CaseFile {
            id: FileId::generate(),
            case_id: case_id.clone(),
            name: name.to_owned(),
            media_type: "text/markdown".to_owned(),
            kind: FileKind::Text,
            size: 12,
            sha256: "ab".repeat(32),
            text: text.map(str::to_owned),
            text_chars: None,
            pages: None,
            created_at: time(1),
        }
    }

    #[test]
    fn files_are_listed_and_fetched_by_case() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let other_case = insert_case(&connection);
        let tone = file(&case_id, "tone.md", Some("Be polite, é"));
        let photo = CaseFile {
            kind: FileKind::Image,
            media_type: "image/png".to_owned(),
            ..file(&case_id, "panel.png", None)
        };

        // Act
        insert_file(&connection, &tone).unwrap();
        insert_file(&connection, &photo).unwrap();

        // Assert
        let listed = list_files(&connection, &case_id).unwrap();
        assert_eq!(
            listed.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["tone.md", "panel.png"]
        );
        assert_eq!(listed[0].text_chars, Some(12));
        assert_eq!(listed[1].kind, FileKind::Image);
        assert_eq!(
            get_file(&connection, &case_id, &photo.id).unwrap().unwrap().name,
            "panel.png"
        );
        assert!(get_file(&connection, &other_case, &photo.id).unwrap().is_none());
        assert_eq!(list_files(&connection, &other_case).unwrap(), [] as [CaseFile; 0]);
    }
}
