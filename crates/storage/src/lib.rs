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
pub mod channels;
pub mod contacts;
pub mod events;
pub mod files;
pub mod human;
pub mod instructions;
pub mod notes;
pub mod queue;
pub mod skills;
pub mod waits;

/// Schema migrations, applied in order. The index + 1 is stored in `PRAGMA user_version`.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_channels.sql"),
    include_str!("../migrations/0003_approvals_and_checks.sql"),
    include_str!("../migrations/0004_contacts.sql"),
    include_str!("../migrations/0005_daily_activation_budget.sql"),
    include_str!("../migrations/0006_skills.sql"),
];

/// Backups taken before migrating that are kept, newest first; older ones are deleted.
const KEPT_BACKUPS: usize = 3;
/// File name prefix of those backups, in `backups/` next to the database.
const BACKUP_PREFIX: &str = "before-schema-";

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
    /// The database was migrated by a newer server; this one would misread it.
    #[error(
        "the database has schema {found}, newer than this server's {known}: run the newer \
         version again, or restore the backup taken before it migrated (backups/ next to the database)"
    )]
    TooNew {
        /// The database's schema version.
        found: u32,
        /// The newest schema this server knows.
        known: usize,
    },
    /// The backup before migrating could not be written; nothing was migrated.
    #[error("cannot back up the database before migrating it: {0}")]
    Backup(#[from] std::io::Error),
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

    /// Copy the database into `backups/` next to it (a consistent snapshot, taken with
    /// `VACUUM INTO`) and delete all but the newest [`KEPT_BACKUPS`].
    fn backup(&self, connection: &Connection, schema: u32) -> Result<PathBuf> {
        let dir = self.path.parent().unwrap_or_else(|| Path::new(".")).join("backups");
        std::fs::create_dir_all(&dir)?;
        // Down to the nanosecond: VACUUM INTO refuses a file that already exists.
        let stamp = Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
        let backup = dir.join(format!("{BACKUP_PREFIX}{schema}-{stamp}.db"));
        // VACUUM INTO takes a file name, not a bound parameter; quotes are doubled.
        let name = backup.to_string_lossy().replace('\'', "''");
        connection.execute_batch(&format!("VACUUM INTO '{name}'"))?;
        let mut backups: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(BACKUP_PREFIX))
            })
            .collect();
        // Sorted by modification time, newest first; the stamp in the name breaks ties.
        backups.sort_by_cached_key(|path| {
            let modified = std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok();
            std::cmp::Reverse((modified, path.clone()))
        });
        for old in backups.iter().skip(KEPT_BACKUPS) {
            std::fs::remove_file(old)?;
        }
        Ok(backup)
    }

    /// Apply every migration newer than the database's `user_version`. A database that
    /// already has a schema is first backed up into `backups/` next to it, so going back to
    /// the previous version means restoring that file.
    ///
    /// # Returns
    ///
    /// The backup taken, if anything was migrated on an existing database
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::TooNew`] if a newer server migrated the database,
    /// [`StorageError::Backup`] if the backup cannot be written (nothing is migrated then),
    /// and [`StorageError::Sqlite`] if a migration fails; that migration is rolled back.
    pub fn migrate(&self) -> Result<Option<PathBuf>> {
        let mut connection = self.connect()?;
        let current: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        // `u32` always fits in `usize` on the 32- and 64-bit targets this runs on.
        let applied = current as usize;
        if applied > MIGRATIONS.len() {
            return Err(StorageError::TooNew {
                found: current,
                known: MIGRATIONS.len(),
            });
        }
        let backup = if 0 < applied && applied < MIGRATIONS.len() {
            Some(self.backup(&connection, current)?)
        } else {
            None
        };
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(applied) {
            let transaction = connection.transaction()?;
            transaction.execute_batch(migration)?;
            // PRAGMA does not accept bound parameters, hence the format!.
            transaction.execute_batch(&format!("PRAGMA user_version = {}", index.saturating_add(1)))?;
            transaction.commit()?;
        }
        Ok(backup)
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
            human_channels: Some(vec!["discord_joe".to_owned()]),
            approvals: clankjob_core::case::ApprovalPolicy::default(),
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
    fn an_existing_database_is_backed_up_before_migrating_and_old_backups_are_pruned() {
        // Arrange: a database one migration behind.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::new(dir.path().join("clankjob.db"));
        let behind = MIGRATIONS.len().saturating_sub(1);
        let connection = db.connect().unwrap();
        for migration in &MIGRATIONS[..behind] {
            connection.execute_batch(migration).unwrap();
        }
        connection.execute_batch(&format!("PRAGMA user_version = {behind}")).unwrap();

        // Act
        let backup = db.migrate().unwrap().unwrap();
        let schema: u32 = Connection::open(&backup)
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let again = db.migrate().unwrap();
        for _ in 0..KEPT_BACKUPS {
            let _ = db.backup(&connection, 1).unwrap();
        }

        // Assert
        assert_eq!(schema as usize, behind, "the backup is the database as it was");
        assert!(again.is_none(), "nothing to migrate, nothing backed up");
        assert_eq!(
            std::fs::read_dir(backup.parent().unwrap()).unwrap().count(),
            KEPT_BACKUPS
        );
        assert!(!backup.exists(), "the oldest backup was pruned");
    }

    #[test]
    fn the_lifetime_activation_budget_becomes_a_daily_one() {
        // Arrange: two cases stored with the former budget, one at its default of 20.
        let test_db = test_support::TestDb::new();
        let connection = test_db.connect();
        let default = test_support::insert_case(&connection);
        let custom = test_support::insert_case(&connection);
        for (case, limit) in [(&default, 20), (&custom, 5)] {
            let old =
                format!(r#"{{"max_activations": {limit}, "max_turns_per_activation": 30, "max_total_tokens": 9}}"#);
            connection
                .execute(
                    "UPDATE cases SET budgets = ?1 WHERE id = ?2",
                    rusqlite::params![old, case.as_str()],
                )
                .unwrap();
        }

        // Act
        connection.execute_batch(MIGRATIONS[4]).unwrap();

        // Assert
        let budgets = |case: &clankjob_core::ids::CaseId| cases::get_case(&connection, case).unwrap().unwrap().budgets;
        assert_eq!(budgets(&default).max_activations_per_day, 100);
        assert_eq!(budgets(&custom).max_activations_per_day, 5);
        assert_eq!(budgets(&custom).max_total_tokens, 9, "the other budgets are kept");
        let stored: String = connection
            .query_row("SELECT budgets FROM cases WHERE id = ?1", [custom.as_str()], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!stored.contains("\"max_activations\""));
    }

    #[test]
    fn a_database_from_a_newer_server_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::new(dir.path().join("clankjob.db"));
        db.connect().unwrap().execute_batch("PRAGMA user_version = 999").unwrap();

        assert!(matches!(db.migrate(), Err(StorageError::TooNew { found: 999, .. })));
    }

    #[test]
    fn millis_round_trip() {
        let time = test_support::time(42);

        assert_eq!(from_millis(to_millis(time)).unwrap(), time);
    }
}
