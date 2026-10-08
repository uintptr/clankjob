//! The `skills`, `skill_versions` and `skill_uses` tables: skills cases saved, every
//! version of each, and which cases used them.

use chrono::{DateTime, Utc};
use clankjob_core::ids::{CaseId, HumanRequestId};
use clankjob_core::skill::{SkillAuthor, SkillDraft, SkillSummary, SkillUse, SkillVersion};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Result, from_millis, from_optional_millis, parse_enum, to_millis};

const VERSION_COLUMNS: &str = "skill, version, description, content, files, saved_by, case_id, approval_id, created_at";

fn version_from_row(row: &Row<'_>) -> Result<SkillVersion> {
    let files: String = row.get(4)?;
    let saved_by: String = row.get(5)?;
    Ok(SkillVersion {
        skill: row.get(0)?,
        version: row.get(1)?,
        draft: SkillDraft {
            description: row.get(2)?,
            content: row.get(3)?,
            files: serde_json::from_str(&files)?,
        },
        saved_by: parse_enum(&saved_by)?,
        case_id: row.get::<_, Option<String>>(6)?.map(CaseId::from_string),
        approval_id: row.get::<_, Option<String>>(7)?.map(HumanRequestId::from_string),
        created_at: from_millis(row.get(8)?)?,
    })
}

/// Who saved a new version, and with whose approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Saver<'a> {
    /// Owner or agent.
    pub author: SkillAuthor,
    /// The case that saved it.
    pub case_id: Option<&'a CaseId>,
    /// The approval that let it in.
    pub approval_id: Option<&'a HumanRequestId>,
}

/// Save a new version of a skill, creating the skill if it is new. The new version
/// becomes current; a disabled skill stays disabled.
///
/// # Returns
///
/// The new version's number
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if a write fails.
pub fn save_version(
    connection: &Connection,
    name: &str,
    draft: &SkillDraft,
    saver: Saver<'_>,
    now: DateTime<Utc>,
) -> Result<u32> {
    let version: u32 = connection.query_row(
        "INSERT INTO skills (name, version, created_at, updated_at) VALUES (?1, 1, ?2, ?2) \
         ON CONFLICT (name) DO UPDATE SET version = version + 1, updated_at = ?2 RETURNING version",
        params![name, to_millis(now)],
        |row| row.get(0),
    )?;
    connection.execute(
        &format!("INSERT INTO skill_versions ({VERSION_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
        params![
            name,
            version,
            draft.description,
            draft.content,
            serde_json::to_string(&draft.files)?,
            saver.author.as_str(),
            saver.case_id.map(CaseId::as_str),
            saver.approval_id.map(HumanRequestId::as_str),
            to_millis(now)
        ],
    )?;
    Ok(version)
}

/// One version of a skill; `None` for the current one.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or the row is corrupt.
pub fn get_version(connection: &Connection, name: &str, version: Option<u32>) -> Result<Option<SkillVersion>> {
    let row = connection
        .query_row(
            &format!(
                "SELECT {VERSION_COLUMNS} FROM skill_versions WHERE skill = ?1 \
                 AND version = COALESCE(?2, (SELECT version FROM skills WHERE name = ?1))"
            ),
            params![name, version],
            |row| Ok(version_from_row(row)),
        )
        .optional()?;
    row.transpose()
}

/// The current version of an enabled skill: what cases may read and run.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or the row is corrupt.
pub fn get_enabled(connection: &Connection, name: &str) -> Result<Option<SkillVersion>> {
    let enabled: Option<bool> = connection
        .query_row("SELECT enabled FROM skills WHERE name = ?1", [name], |row| row.get(0))
        .optional()?;
    if enabled == Some(true) {
        get_version(connection, name, None)
    } else {
        Ok(None)
    }
}

/// Every version of a skill, newest first.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn list_versions(connection: &Connection, name: &str) -> Result<Vec<SkillVersion>> {
    let mut statement = connection.prepare(&format!(
        "SELECT {VERSION_COLUMNS} FROM skill_versions WHERE skill = ?1 ORDER BY version DESC"
    ))?;
    let rows = statement.query_map([name], |row| Ok(version_from_row(row)))?;
    rows.map(|row| row?).collect()
}

/// A skill's current description and file names, its first author, and its use.
const SUMMARY_QUERY: &str = "SELECT s.name, current.description, s.enabled, s.version, \
         (SELECT json_group_array(json_extract(file.value, '$.name')) FROM json_each(current.files) AS file), \
         first.saved_by, first.case_id, s.created_at, s.updated_at, \
         (SELECT COUNT(*) FROM skill_uses WHERE skill = s.name), \
         (SELECT MAX(last_used_at) FROM skill_uses WHERE skill = s.name) \
     FROM skills AS s \
     JOIN skill_versions AS current ON current.skill = s.name AND current.version = s.version \
     JOIN skill_versions AS first ON first.skill = s.name AND first.version = 1";

fn summary_from_row(row: &Row<'_>) -> Result<SkillSummary> {
    let files: String = row.get(4)?;
    let created_by: String = row.get(5)?;
    Ok(SkillSummary {
        name: row.get(0)?,
        description: row.get(1)?,
        enabled: row.get(2)?,
        version: row.get(3)?,
        files: serde_json::from_str(&files)?,
        created_by: parse_enum(&created_by)?,
        created_by_case: row.get::<_, Option<String>>(6)?.map(CaseId::from_string),
        created_at: from_millis(row.get(7)?)?,
        updated_at: from_millis(row.get(8)?)?,
        cases: row.get(9)?,
        last_used_at: from_optional_millis(row.get(10)?)?,
    })
}

/// Skills by name, with their current description and file names and how much they
/// were used.
///
/// # Arguments
///
/// * `enabled_only` - Leave out disabled skills, as cases see the list
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn list_skills(connection: &Connection, enabled_only: bool) -> Result<Vec<SkillSummary>> {
    let mut statement =
        connection.prepare_cached(&format!("{SUMMARY_QUERY} WHERE s.enabled OR NOT ?1 ORDER BY s.name"))?;
    let rows = statement.query_map([enabled_only], |row| Ok(summary_from_row(row)))?;
    rows.map(|row| row?).collect()
}

/// One skill as listed, enabled or not.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or the row is corrupt.
pub fn get_skill(connection: &Connection, name: &str) -> Result<Option<SkillSummary>> {
    let row = connection
        .query_row(&format!("{SUMMARY_QUERY} WHERE s.name = ?1"), [name], |row| {
            Ok(summary_from_row(row))
        })
        .optional()?;
    row.transpose()
}

/// Enable or disable a skill.
///
/// # Returns
///
/// Whether the skill exists
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn set_enabled(connection: &Connection, name: &str, enabled: bool, now: DateTime<Utc>) -> Result<bool> {
    Ok(connection.execute(
        "UPDATE skills SET enabled = ?2, updated_at = ?3 WHERE name = ?1",
        params![name, enabled, to_millis(now)],
    )? > 0)
}

/// Delete a skill with its versions and uses.
///
/// # Returns
///
/// Whether the skill existed
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the delete fails.
pub fn delete_skill(connection: &Connection, name: &str) -> Result<bool> {
    Ok(connection.execute("DELETE FROM skills WHERE name = ?1", [name])? > 0)
}

/// Count a use of a skill by a case; nothing happens if no skill has that name.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the write fails.
pub fn record_use(connection: &Connection, name: &str, case_id: &CaseId, now: DateTime<Utc>) -> Result<()> {
    connection.execute(
        "INSERT INTO skill_uses (skill, case_id, uses, first_used_at, last_used_at) \
         SELECT ?1, ?2, 1, ?3, ?3 WHERE EXISTS (SELECT 1 FROM skills WHERE name = ?1) \
         ON CONFLICT (skill, case_id) DO UPDATE SET uses = uses + 1, last_used_at = ?3",
        params![name, case_id.as_str(), to_millis(now)],
    )?;
    Ok(())
}

/// The cases that used a skill, most recent first.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn list_uses(connection: &Connection, name: &str) -> Result<Vec<SkillUse>> {
    let mut statement = connection.prepare(
        "SELECT u.case_id, c.title, u.uses, u.first_used_at, u.last_used_at \
         FROM skill_uses AS u JOIN cases AS c ON c.id = u.case_id \
         WHERE u.skill = ?1 ORDER BY u.last_used_at DESC",
    )?;
    let rows = statement.query_map([name], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    rows.map(|row| {
        let (case_id, title, uses, first, last) = row?;
        Ok(SkillUse {
            case_id: CaseId::from_string(case_id),
            title,
            uses,
            first_used_at: from_millis(first)?,
            last_used_at: from_millis(last)?,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use clankjob_core::skill::SkillFile;

    use super::*;
    use crate::test_support::{TestDb, insert_case, time};

    fn draft(description: &str, files: &[&str]) -> SkillDraft {
        SkillDraft {
            description: description.to_owned(),
            content: format!("How to: {description}"),
            files: files
                .iter()
                .map(|name| SkillFile {
                    name: (*name).to_owned(),
                    content: "print(1)".to_owned(),
                })
                .collect(),
        }
    }

    const OWNER: Saver<'static> = Saver {
        author: SkillAuthor::Owner,
        case_id: None,
        approval_id: None,
    };

    #[test]
    fn every_save_is_a_new_current_version_and_the_first_names_the_author() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let approval = HumanRequestId::generate();
        let agent = Saver {
            author: SkillAuthor::Agent,
            case_id: Some(&case_id),
            approval_id: Some(&approval),
        };

        // Act
        let first = save_version(&connection, "forecast", &draft("Weather", &["f.py"]), agent, time(1)).unwrap();
        let second = save_version(&connection, "forecast", &draft("Weather, hourly", &[]), OWNER, time(2)).unwrap();
        save_version(&connection, "alpha", &draft("First", &[]), OWNER, time(3)).unwrap();

        // Assert
        assert_eq!((first, second), (1, 2));
        let current = get_version(&connection, "forecast", None).unwrap().unwrap();
        assert_eq!(
            (current.version, current.draft.description.as_str()),
            (2, "Weather, hourly")
        );
        let old = get_version(&connection, "forecast", Some(1)).unwrap().unwrap();
        assert_eq!(old.draft.files[0].name, "f.py");
        assert_eq!(
            (old.case_id.as_ref(), old.approval_id.as_ref()),
            (Some(&case_id), Some(&approval))
        );
        let versions: Vec<u32> = list_versions(&connection, "forecast")
            .unwrap()
            .iter()
            .map(|version| version.version)
            .collect();
        assert_eq!(versions, [2, 1]);
        let listed = list_skills(&connection, false).unwrap();
        assert_eq!(
            listed.iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "forecast"]
        );
        assert_eq!(listed[1].created_by, SkillAuthor::Agent);
        assert_eq!(listed[1].created_by_case.as_ref(), Some(&case_id));
        assert_eq!((listed[1].created_at, listed[1].updated_at), (time(1), time(2)));
        assert_eq!(listed[1].files, [] as [String; 0], "the current version has no files");
        assert!(get_version(&connection, "nope", None).unwrap().is_none());
    }

    #[test]
    fn disabled_skills_are_hidden_from_cases_and_deleting_removes_everything() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        save_version(&connection, "forecast", &draft("Weather", &["f.py"]), OWNER, time(1)).unwrap();

        // Act
        record_use(&connection, "forecast", &case_id, time(2)).unwrap();
        record_use(&connection, "forecast", &case_id, time(3)).unwrap();
        record_use(&connection, "unknown", &case_id, time(3)).unwrap();
        let listed = list_skills(&connection, true).unwrap();
        let uses = list_uses(&connection, "forecast").unwrap();
        set_enabled(&connection, "forecast", false, time(4)).unwrap();
        let hidden = (
            get_enabled(&connection, "forecast").unwrap(),
            list_skills(&connection, true).unwrap(),
        );
        let kept = get_skill(&connection, "forecast").unwrap();
        let deleted = delete_skill(&connection, "forecast").unwrap();

        // Assert
        assert_eq!((listed[0].cases, listed[0].last_used_at), (1, Some(time(3))));
        assert_eq!(listed[0].files, ["f.py"]);
        assert_eq!((uses[0].uses, uses[0].title.as_str()), (2, "Quote"));
        assert_eq!((uses[0].first_used_at, uses[0].last_used_at), (time(2), time(3)));
        assert!(hidden.0.is_none() && hidden.1.is_empty());
        assert_eq!(kept.map(|skill| skill.enabled), Some(false), "a disabled skill is kept");
        assert!(deleted);
        assert_eq!(list_versions(&connection, "forecast").unwrap().len(), 0);
        let left: u32 = connection
            .query_row("SELECT COUNT(*) FROM skill_uses", [], |row| row.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn deleting_a_case_keeps_the_skills_it_saved() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let case_id = insert_case(&connection);
        let agent = Saver {
            author: SkillAuthor::Agent,
            case_id: Some(&case_id),
            approval_id: None,
        };
        save_version(&connection, "forecast", &draft("Weather", &[]), agent, time(1)).unwrap();
        record_use(&connection, "forecast", &case_id, time(2)).unwrap();

        // Act
        crate::cases::delete_case(&connection, &case_id).unwrap();

        // Assert
        let skill = &list_skills(&connection, false).unwrap()[0];
        assert_eq!(
            (skill.created_by, skill.created_by_case.as_ref()),
            (SkillAuthor::Agent, None)
        );
        assert_eq!(skill.cases, 0);
    }
}
