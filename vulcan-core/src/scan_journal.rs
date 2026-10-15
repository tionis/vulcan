//! A bounded journal of what each scan changed, for processes that follow
//! the vault's index owner ([`crate::index_owner`]).
//!
//! A follower does not scan, yet some consumers act on the paths each scan
//! changed (change feeds, editor notifications, post-scan events). Every scan
//! that changes the index appends one batch of `(path, status)` rows inside
//! its own transaction, so a batch is visible exactly when its changes are.
//! A full rebuild, or a batch too large to list, records a single
//! reconciliation marker instead. Only the newest batches are kept; the
//! lowest batch that may have been dropped is the floor, and a follower
//! whose position is below it has missed changes and must reconcile.

use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// Batches kept for followers; older ones are dropped.
const KEPT_BATCHES: i64 = 64;
/// Larger batches record a reconciliation marker instead of their paths.
const MAX_BATCH_PATHS: usize = 10_000;
const FLOOR_KEY: &str = "scan_change_floor";
const RECONCILE: &str = "reconcile";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanChangeStatus {
    Added,
    Updated,
    Deleted,
}

impl ScanChangeStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Updated => "updated",
            Self::Deleted => "deleted",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "added" => Some(Self::Added),
            "updated" => Some(Self::Updated),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }
}

/// What changed after a follower's position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanChanges {
    /// The newest batch read; the follower's next position.
    pub position: i64,
    /// Changes may be missing (dropped batches, a rebuild, or an oversized
    /// batch): reconcile everything instead of trusting `changes`.
    pub reconcile: bool,
    /// Each changed path with its last status, in batch then path order.
    pub changes: Vec<(String, ScanChangeStatus)>,
}

pub(crate) fn apply_schema(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "CREATE TABLE scan_change_log (
             batch INTEGER NOT NULL,
             path TEXT NOT NULL,
             status TEXT NOT NULL,
             PRIMARY KEY (batch, path)
         ) WITHOUT ROWID;",
    )
}

/// Record one scan's changes. Does nothing when nothing changed.
pub(crate) fn record(
    transaction: &Transaction<'_>,
    changes: &[(String, ScanChangeStatus)],
) -> Result<(), rusqlite::Error> {
    if changes.is_empty() {
        return Ok(());
    }
    if changes.len() > MAX_BATCH_PATHS {
        return record_reconcile(transaction);
    }
    let batch = next_batch(transaction)?;
    let mut insert = transaction.prepare(
        "INSERT OR REPLACE INTO scan_change_log (batch, path, status) VALUES (?1, ?2, ?3)",
    )?;
    for (path, status) in changes {
        insert.execute(params![batch, path, status.as_str()])?;
    }
    prune(transaction, batch)
}

/// Record that followers must reconcile everything, as after a rebuild.
pub(crate) fn record_reconcile(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    let batch = next_batch(transaction)?;
    transaction.execute(
        "INSERT INTO scan_change_log (batch, path, status) VALUES (?1, '', ?2)",
        params![batch, RECONCILE],
    )?;
    prune(transaction, batch)
}

fn next_batch(connection: &Connection) -> Result<i64, rusqlite::Error> {
    Ok(latest(connection)? + 1)
}

fn prune(transaction: &Transaction<'_>, newest: i64) -> Result<(), rusqlite::Error> {
    let oldest_kept = newest - KEPT_BATCHES + 1;
    if oldest_kept <= floor(transaction)? + 1 {
        return Ok(());
    }
    transaction.execute(
        "DELETE FROM scan_change_log WHERE batch < ?1",
        [oldest_kept],
    )?;
    transaction.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![FLOOR_KEY, (oldest_kept - 1).to_string()],
    )?;
    Ok(())
}

/// Batches at or below the floor may have been dropped.
fn floor(connection: &Connection) -> Result<i64, rusqlite::Error> {
    Ok(connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [FLOOR_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0))
}

/// The newest batch, a follower's starting position.
pub fn latest(connection: &Connection) -> Result<i64, rusqlite::Error> {
    Ok(connection
        .query_row("SELECT MAX(batch) FROM scan_change_log", [], |row| {
            row.get::<_, Option<i64>>(0)
        })?
        .unwrap_or(0)
        .max(floor(connection)?))
}

/// The changes after `position`, read in one snapshot. `None` when nothing
/// changed.
pub fn changes_since(
    connection: &Connection,
    position: i64,
) -> Result<Option<ScanChanges>, rusqlite::Error> {
    let snapshot = connection.unchecked_transaction()?;
    let newest = latest(&snapshot)?;
    if newest <= position {
        return Ok(None);
    }
    let mut reconcile = position < floor(&snapshot)?;
    let mut by_path = std::collections::BTreeMap::new();
    let mut statement = snapshot.prepare(
        "SELECT path, status FROM scan_change_log WHERE batch > ?1 ORDER BY batch, path",
    )?;
    let mut rows = statement.query([position])?;
    while let Some(row) = rows.next()? {
        let (path, status): (String, String) = (row.get(0)?, row.get(1)?);
        match ScanChangeStatus::parse(&status) {
            Some(status) => {
                // An earlier addition stays an addition until it is deleted.
                let entry = by_path.entry(path).or_insert(status);
                if !(*entry == ScanChangeStatus::Added && status == ScanChangeStatus::Updated) {
                    *entry = status;
                }
            }
            None => reconcile = true,
        }
    }
    drop(rows);
    drop(statement);
    snapshot.finish()?;
    Ok(Some(ScanChanges {
        position: newest,
        reconcile,
        changes: by_path.into_iter().collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CacheDatabase, VaultPaths};

    fn database() -> (tempfile::TempDir, CacheDatabase) {
        let temp = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temp.path());
        std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
        let database = CacheDatabase::open(&paths).unwrap();
        (temp, database)
    }

    fn write(database: &CacheDatabase, changes: &[(&str, ScanChangeStatus)]) {
        let transaction = database.connection().unchecked_transaction().unwrap();
        let changes = changes
            .iter()
            .map(|(path, status)| ((*path).to_string(), *status))
            .collect::<Vec<_>>();
        record(&transaction, &changes).unwrap();
        transaction.commit().unwrap();
    }

    #[test]
    fn followers_read_each_path_once_with_its_net_status() {
        use ScanChangeStatus::{Added, Deleted, Updated};
        let (_temp, database) = database();
        let connection = database.connection();
        assert_eq!(latest(connection).unwrap(), 0);
        assert_eq!(changes_since(connection, 0).unwrap(), None);
        write(&database, &[]);
        assert_eq!(latest(connection).unwrap(), 0, "no change, no batch");

        write(&database, &[("a.md", Added), ("b.md", Updated)]);
        write(
            &database,
            &[("a.md", Updated), ("b.md", Deleted), ("c.md", Added)],
        );
        let read = changes_since(connection, 0).unwrap().unwrap();
        assert_eq!(read.position, 2);
        assert!(!read.reconcile);
        assert_eq!(
            read.changes,
            [
                ("a.md".to_string(), Added),
                ("b.md".to_string(), Deleted),
                ("c.md".to_string(), Added),
            ]
        );
        let later = changes_since(connection, 1).unwrap().unwrap();
        assert_eq!(later.changes[0], ("a.md".to_string(), Updated));
        assert_eq!(changes_since(connection, 2).unwrap(), None);
    }

    #[test]
    fn scans_journal_what_they_change() {
        use crate::{scan_vault, ScanMode};
        use ScanChangeStatus::{Added, Deleted, Updated};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let paths = VaultPaths::new(root);
        std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
        std::fs::write(root.join("A.md"), "a").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let database = CacheDatabase::open(&paths).unwrap();
        let connection = database.connection();
        let rebuilt = changes_since(connection, 0).unwrap().unwrap();
        assert!(
            rebuilt.reconcile,
            "a full rebuild asks followers to reconcile"
        );
        let mut position = rebuilt.position;
        let mut step = |change: &dyn Fn()| {
            change();
            scan_vault(&paths, ScanMode::Incremental).unwrap();
            let read = changes_since(connection, position).unwrap();
            if let Some(read) = &read {
                position = read.position;
                assert!(!read.reconcile);
            }
            read.map(|read| read.changes).unwrap_or_default()
        };
        let entry = |path: &str, status| (path.to_string(), status);
        assert_eq!(
            step(&|| std::fs::write(root.join("B.md"), "b").unwrap()),
            [entry("B.md", Added)]
        );
        assert_eq!(
            step(&|| std::fs::write(root.join("B.md"), "b2").unwrap()),
            [entry("B.md", Updated)]
        );
        assert_eq!(
            step(&|| std::fs::rename(root.join("B.md"), root.join("C.md")).unwrap()),
            [entry("B.md", Deleted), entry("C.md", Updated)]
        );
        assert_eq!(
            step(&|| std::fs::remove_file(root.join("A.md")).unwrap()),
            [entry("A.md", Deleted)]
        );
        assert_eq!(step(&|| {}), [], "an unchanged vault journals nothing");
    }

    #[test]
    fn rebuilds_oversized_batches_and_dropped_batches_require_reconciliation() {
        let (_temp, database) = database();
        let connection = database.connection();
        {
            let transaction = connection.unchecked_transaction().unwrap();
            record_reconcile(&transaction).unwrap();
            transaction.commit().unwrap();
        }
        assert!(changes_since(connection, 0).unwrap().unwrap().reconcile);

        let huge = (0..=MAX_BATCH_PATHS)
            .map(|index| (format!("{index}.md"), ScanChangeStatus::Added))
            .collect::<Vec<_>>();
        {
            let transaction = connection.unchecked_transaction().unwrap();
            record(&transaction, &huge).unwrap();
            transaction.commit().unwrap();
        }
        let read = changes_since(connection, 1).unwrap().unwrap();
        assert!(read.reconcile && read.changes.is_empty());

        for _ in 0..KEPT_BATCHES {
            write(&database, &[("x.md", ScanChangeStatus::Updated)]);
        }
        let newest = latest(connection).unwrap();
        let kept: i64 = connection
            .query_row(
                "SELECT COUNT(DISTINCT batch) FROM scan_change_log",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, KEPT_BATCHES);
        // A follower behind the dropped batches must reconcile; a current one need not.
        assert!(changes_since(connection, 1).unwrap().unwrap().reconcile);
        assert!(
            !changes_since(connection, newest - 1)
                .unwrap()
                .unwrap()
                .reconcile
        );
    }
}
