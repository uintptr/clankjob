//! The `cases` table.

use chrono::{DateTime, Utc};
use clankjob_core::case::{ApprovalPolicy, Case, CaseState, NewCase, Usage};
use clankjob_core::ids::CaseId;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;

use crate::{Result, from_millis, parse_enum, to_millis};

const COLUMNS: &str = "id, title, goal, owner, profile, llm, model, state, budgets, usage, result, outcome, \
                       created_at, updated_at, human_channels, approvals";

/// Filter for [`list_cases`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaseFilter {
    /// Only cases in this state.
    pub state: Option<CaseState>,
    /// Only cases created before this one (pagination cursor).
    pub before: Option<CaseId>,
    /// Maximum number of cases returned.
    pub limit: u32,
}

fn case_from_row(row: &Row<'_>) -> Result<Case> {
    let state: String = row.get(7)?;
    let budgets: String = row.get(8)?;
    let usage: String = row.get(9)?;
    let result: Option<String> = row.get(10)?;
    let human_channels: String = row.get(14)?;
    let approvals: String = row.get(15)?;
    Ok(Case {
        id: CaseId::from_string(row.get::<_, String>(0)?),
        title: row.get(1)?,
        goal: row.get(2)?,
        owner: row.get(3)?,
        profile: row.get(4)?,
        llm: row.get(5)?,
        model: row.get(6)?,
        state: parse_enum(&state)?,
        budgets: serde_json::from_str(&budgets)?,
        usage: serde_json::from_str(&usage)?,
        result: result.as_deref().map(serde_json::from_str).transpose()?,
        outcome: row.get(11)?,
        human_channels: serde_json::from_str(&human_channels)?,
        approvals: parse_enum(&approvals)?,
        created_at: from_millis(row.get(12)?)?,
        updated_at: from_millis(row.get(13)?)?,
    })
}

/// Insert a new case in state `pending`.
///
/// # Arguments
///
/// * `connection` - Connection or transaction
/// * `id` - Id for the new case
/// * `new_case` - Case fields
/// * `now` - Creation time
///
/// # Returns
///
/// The stored case
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the insert fails.
pub fn insert_case(connection: &Connection, id: &CaseId, new_case: &NewCase, now: DateTime<Utc>) -> Result<Case> {
    let case = Case {
        id: id.clone(),
        title: new_case.title.clone(),
        goal: new_case.goal.clone(),
        owner: new_case.owner.clone(),
        profile: new_case.profile.clone(),
        llm: new_case.llm.clone(),
        model: new_case.model.clone(),
        state: CaseState::Pending,
        budgets: new_case.budgets,
        usage: Usage::default(),
        result: None,
        outcome: None,
        human_channels: new_case.human_channels.clone().unwrap_or_default(),
        approvals: new_case.approvals,
        created_at: now,
        updated_at: now,
    };
    connection.execute(
        &format!(
            "INSERT INTO cases ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, ?11, ?11, ?12, ?13)"
        ),
        params![
            case.id.as_str(),
            case.title,
            case.goal,
            case.owner,
            case.profile,
            case.llm,
            case.model,
            case.state.as_str(),
            serde_json::to_string(&case.budgets)?,
            serde_json::to_string(&case.usage)?,
            to_millis(now),
            serde_json::to_string(&case.human_channels)?,
            case.approvals.as_str(),
        ],
    )?;
    Ok(case)
}

/// Fetch one case.
///
/// # Returns
///
/// The case, or `None` if no case has this id
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or the row is corrupt.
pub fn get_case(connection: &Connection, id: &CaseId) -> Result<Option<Case>> {
    let mut statement = connection.prepare_cached(&format!("SELECT {COLUMNS} FROM cases WHERE id = ?1"))?;
    // `query_row` maps rusqlite errors only, so the row is fetched raw and converted after.
    statement
        .query_row([id.as_str()], |row| Ok(case_from_row(row)))
        .optional()?
        .transpose()
}

/// List cases, newest first.
///
/// Ordering uses SQLite's `rowid`, which grows with every insert. ULIDs created within
/// the same millisecond do not sort by creation order, so they can't be used for this.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the query fails or a row is corrupt.
pub fn list_cases(connection: &Connection, filter: &CaseFilter) -> Result<Vec<Case>> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM cases WHERE (?1 IS NULL OR state = ?1) \
         AND (?2 IS NULL OR rowid < (SELECT rowid FROM cases WHERE id = ?2)) ORDER BY rowid DESC LIMIT ?3"
    ))?;
    let rows = statement.query_map(
        params![
            filter.state.map(CaseState::as_str),
            filter.before.as_ref().map(CaseId::as_str),
            filter.limit
        ],
        |row| Ok(case_from_row(row)),
    )?;
    // Each item is a rusqlite result wrapping our own result; `?` then collect flattens both.
    rows.map(|row| row?).collect()
}

/// Change a case's state.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn update_state(connection: &Connection, id: &CaseId, state: CaseState, now: DateTime<Utc>) -> Result<()> {
    connection.execute(
        "UPDATE cases SET state = ?2, updated_at = ?3 WHERE id = ?1",
        params![id.as_str(), state.as_str(), to_millis(now)],
    )?;
    Ok(())
}

/// Tables holding a case's rows, children first: `channel_deliveries` refers to
/// `human_requests`, and every table refers to `cases`, which goes last.
const CASE_TABLES: &[&str] = &[
    "channel_deliveries",
    "events",
    "activations",
    "work_queue",
    "wait_conditions",
    "case_notes",
    "human_requests",
    "instructions",
    "files",
];

/// Delete a case and every row about it. The bytes of its files are the caller's to
/// remove, once the transaction commits.
///
/// # Returns
///
/// Whether the case existed
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if a delete fails.
pub fn delete_case(connection: &Connection, id: &CaseId) -> Result<bool> {
    for table in CASE_TABLES {
        connection.execute(&format!("DELETE FROM {table} WHERE case_id = ?1"), params![id.as_str()])?;
    }
    Ok(connection.execute("DELETE FROM cases WHERE id = ?1", params![id.as_str()])? > 0)
}

/// Change when a case's approval-gated calls wait for the owner.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn update_approvals(
    connection: &Connection,
    id: &CaseId,
    approvals: ApprovalPolicy,
    now: DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "UPDATE cases SET approvals = ?2, updated_at = ?3 WHERE id = ?1",
        params![id.as_str(), approvals.as_str(), to_millis(now)],
    )?;
    Ok(())
}

/// Change the LLM a case runs on and its model override (`None`: the LLM's default).
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn update_model(
    connection: &Connection,
    id: &CaseId,
    llm: &str,
    model: Option<&str>,
    now: DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "UPDATE cases SET llm = ?2, model = ?3, updated_at = ?4 WHERE id = ?1",
        params![id.as_str(), llm, model, to_millis(now)],
    )?;
    Ok(())
}

/// Change a case's title.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn update_title(connection: &Connection, id: &CaseId, title: &str, now: DateTime<Utc>) -> Result<()> {
    connection.execute(
        "UPDATE cases SET title = ?2, updated_at = ?3 WHERE id = ?1",
        params![id.as_str(), title, to_millis(now)],
    )?;
    Ok(())
}

/// Replace a case's usage counters.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn update_usage(connection: &Connection, id: &CaseId, usage: &Usage, now: DateTime<Utc>) -> Result<()> {
    connection.execute(
        "UPDATE cases SET usage = ?2, updated_at = ?3 WHERE id = ?1",
        params![id.as_str(), serde_json::to_string(usage)?, to_millis(now)],
    )?;
    Ok(())
}

/// Record how a case ended: the `complete` summary and result, or the failure reason.
///
/// # Errors
///
/// Returns a [`crate::StorageError`] if the update fails.
pub fn update_outcome(
    connection: &Connection,
    id: &CaseId,
    outcome: &str,
    result: Option<&Value>,
    now: DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "UPDATE cases SET outcome = ?2, result = ?3, updated_at = ?4 WHERE id = ?1",
        params![
            id.as_str(),
            outcome,
            result.map(serde_json::to_string).transpose()?,
            to_millis(now)
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDb, insert_case as insert_test_case, time};

    #[test]
    fn inserted_case_can_be_read_back() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();

        // Act
        let id = insert_test_case(&connection);
        let case = get_case(&connection, &id).unwrap().unwrap();

        // Assert
        assert_eq!(case.id, id);
        assert_eq!(case.state, CaseState::Pending);
        assert_eq!(case.created_at, time(0));
    }

    #[test]
    fn missing_case_is_none() {
        let test_db = TestDb::new();

        assert!(get_case(&test_db.connect(), &CaseId::generate()).unwrap().is_none());
    }

    #[test]
    fn list_filters_by_state_and_paginates_newest_first() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let first = insert_test_case(&connection);
        let second = insert_test_case(&connection);
        let third = insert_test_case(&connection);
        update_state(&connection, &second, CaseState::Sleeping, time(1)).unwrap();

        // Act
        let all = list_cases(
            &connection,
            &CaseFilter {
                limit: 10,
                ..CaseFilter::default()
            },
        )
        .unwrap();
        let sleeping = list_cases(
            &connection,
            &CaseFilter {
                state: Some(CaseState::Sleeping),
                limit: 10,
                before: None,
            },
        )
        .unwrap();
        let older = list_cases(
            &connection,
            &CaseFilter {
                before: Some(third.clone()),
                limit: 1,
                state: None,
            },
        )
        .unwrap();

        // Assert
        let ids: Vec<CaseId> = all.into_iter().map(|case| case.id).collect();
        assert_eq!(ids, vec![third, second.clone(), first]);
        assert_eq!(sleeping.len(), 1);
        assert_eq!(older.first().unwrap().id, second);
    }

    #[test]
    fn usage_and_outcome_are_updated() {
        // Arrange
        let test_db = TestDb::new();
        let connection = test_db.connect();
        let id = insert_test_case(&connection);
        let usage = Usage {
            activations: 2,
            input_tokens: 100,
            output_tokens: 20,
        };

        // Act
        update_usage(&connection, &id, &usage, time(5)).unwrap();
        update_outcome(
            &connection,
            &id,
            "done",
            Some(&serde_json::json!({"price": 10})),
            time(6),
        )
        .unwrap();

        // Assert
        let case = get_case(&connection, &id).unwrap().unwrap();
        assert_eq!(case.usage, usage);
        assert_eq!(case.outcome.as_deref(), Some("done"));
        assert_eq!(case.result, Some(serde_json::json!({"price": 10})));
        assert_eq!(case.updated_at, time(6));
    }
}
