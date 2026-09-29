//! SQLite persistence for clankjob.
//!
//! Every repository function takes a `&Connection`. A `rusqlite::Transaction` dereferences
//! to a `Connection`, so the engine can compose several of these functions into one atomic
//! transaction (see [`begin_write`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::TransactionBehavior;
// Re-exported so callers can hold connections without depending on rusqlite themselves.
pub use rusqlite::{Connection, Transaction};

pub mod activations;
pub mod cases;
pub mod events;
pub mod files;
pub mod human;
pub mod instructions;
pub mod notes;
pub mod queue;
pub mod waits;

/// Schema migrations, applied in order. The index + 1 is stored in `PRAGMA user_version`.
const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_init.sql")];

/// How long a connection waits for a lock held by another connection before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Errors from the storage layer.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// SQLite reported an error.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A JSON column could not be encoded or decoded.
    #[error("invalid JSON in database: {0}")]
    Json(#[from] serde_json::Error),
    /// A stored value is not valid for its column (unknown enum, bad timestamp, …).
    #[error("corrupt database value: {0}")]
    Corrupt(String),
}

/// Shorthand for results of storage functions.
pub type Result<T> = std::result::Result<T, StorageError>;

/// Location of the SQLite database. Cheap to clone; each thread opens its own connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Db {
    path: PathBuf,
}

impl Db {
    /// Point at a database file. Nothing is opened until [`Db::connect`].
    ///
    /// # Arguments
    ///
    /// * `path` - Path of the SQLite file; created on first connect if missing
    pub fn new<P>(path: P) -> Self
    where
        P: AsRef<Path>,
    {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Open a new connection with the pragmas every connection needs.
    ///
    /// WAL mode lets readers proceed while one writer commits, and `foreign_keys` is off by
    /// default in SQLite so it must be enabled per connection.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] if the file cannot be opened or configured.
    pub fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        Ok(connection)
    }

    /// Apply every migration newer than the database's `user_version`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] if a migration fails; that migration is rolled back.
    pub fn migrate(&self) -> Result<()> {
        let mut connection = self.connect()?;
        let current: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        // `u32` always fits in `usize` on the 32- and 64-bit targets this runs on.
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let transaction = connection.transaction()?;
            transaction.execute_batch(migration)?;
            // PRAGMA does not accept bound parameters, hence the format!.
            transaction.execute_batch(&format!("PRAGMA user_version = {}", index.saturating_add(1)))?;
            transaction.commit()?;
        }
        Ok(())
    }
}

/// Start a transaction that takes the write lock immediately.
///
/// A default (deferred) transaction that first reads and then writes can fail with
/// `SQLITE_BUSY` when another writer got in between; taking the lock up front avoids that.
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] if the lock cannot be acquired within the busy timeout.
pub fn begin_write(connection: &mut Connection) -> Result<Transaction<'_>> {
    Ok(connection.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

/// Commit a transaction started with [`begin_write`].
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] if the commit fails; the transaction is rolled back.
pub fn commit(transaction: Transaction<'_>) -> Result<()> {
    Ok(transaction.commit()?)
}

/// Convert a timestamp to the stored representation (Unix milliseconds).
pub(crate) fn to_millis(time: DateTime<Utc>) -> i64 {
    time.timestamp_millis()
}

/// Convert a stored timestamp back.
pub(crate) fn from_millis(millis: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_millis(millis).ok_or_else(|| StorageError::Corrupt(format!("timestamp {millis}")))
}

/// Convert an optional stored timestamp back.
pub(crate) fn from_optional_millis(millis: Option<i64>) -> Result<Option<DateTime<Utc>>> {
    millis.map(from_millis).transpose()
}

/// Parse a stored enum string, mapping failures to [`StorageError::Corrupt`].
pub(crate) fn parse_enum<T>(value: &str) -> Result<T>
where
    T: std::str::FromStr<Err = clankjob_core::ParseEnumError>,
{
    value
        .parse()
        .map_err(|error: clankjob_core::ParseEnumError| StorageError::Corrupt(error.to_string()))
}

#[cfg(test)]
pub(crate) mod test_support {
    use chrono::{DateTime, TimeZone, Utc};
    use clankjob_core::case::{Budgets, NewCase};
    use clankjob_core::ids::CaseId;
    use rusqlite::Connection;
    use tempfile::TempDir;

    use crate::Db;

    /// A migrated database in a temporary directory, deleted when dropped.
    pub struct TestDb {
        pub db: Db,
        // Kept alive so the directory is not deleted while the test runs.
        _dir: TempDir,
    }

    impl TestDb {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = Db::new(dir.path().join("test.db"));
            db.migrate().unwrap();
            Self { db, _dir: dir }
        }

        pub fn connect(&self) -> Connection {
            self.db.connect().unwrap()
        }
    }

    /// A fixed point in time so tests are deterministic.
    pub fn time(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds.saturating_add(1_790_000_000), 0).unwrap()
    }

    /// Insert a case with default values and return its id.
    pub fn insert_case(connection: &Connection) -> CaseId {
        let id = CaseId::generate();
        let new_case = NewCase {
            title: "Quote".to_owned(),
            goal: "Get a quote".to_owned(),
            owner: Some("joe".to_owned()),
            profile: None,
            llm: "default".to_owned(),
            model: None,
            budgets: Budgets::default(),
            instructions: Vec::new(),
        };
        crate::cases::insert_case(connection, &id, &new_case, time(0)).unwrap();
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_idempotent() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let db = Db::new(dir.path().join("test.db"));

        // Act
        db.migrate().unwrap();
        db.migrate().unwrap();

        // Assert
        let version: u32 = db
            .connect()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
    }

    #[test]
    fn millis_round_trip() {
        let time = test_support::time(42);

        assert_eq!(from_millis(to_millis(time)).unwrap(), time);
    }
}
