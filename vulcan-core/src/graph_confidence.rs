//! Stored whole-graph link confidence, so `status` does not refold every link.
//!
//! The breakdown counts note-to-note links, where each source/target pair's
//! links all take the pair's confidence folded in byte order
//! ([`crate::graph::prefer_edge_confidence`]). Folding every link costs about
//! a second at 100K notes, so each source note's contribution is stored:
//!
//! - `graph_confidence_sources` holds one row per source note with links:
//!   the folded counts of its outgoing note-to-note links.
//! - Triggers only mark sources whose contribution may have changed in
//!   `graph_confidence_dirty`: link rows written or removed, and notes whose
//!   extension changes (their own links and the links pointing at them).
//! - Scans recompute dirty sources inside their write transaction, and a full
//!   rebuild recomputes every source.
//! - A read sums stored rows of clean sources and folds dirty sources live in
//!   one snapshot, so it is exact whenever a recompute has not run yet.
//!
//! Every count comes from [`fold_sources`], the one fold shared by all paths.

use std::collections::HashMap;

use rusqlite::{Connection, Transaction};

use crate::graph::{prefer_edge_confidence, GraphConfidenceBreakdown, LinkConfidence};

const FOLD_SQL: &str = "
    SELECT links.source_document_id, links.resolved_target_id,
           links.confidence, links.confidence_score
    FROM links
    JOIN documents AS source ON source.id = links.source_document_id
    JOIN documents AS target ON target.id = links.resolved_target_id
    WHERE source.extension = 'md' AND target.extension = 'md'";
const FOLD_ORDER: &str =
    " ORDER BY links.source_document_id, links.resolved_target_id, links.byte_offset";

/// Create the stored totals and their dirty marking, then fill them.
pub(crate) fn apply_schema(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "CREATE TABLE graph_confidence_sources (
             document_id TEXT PRIMARY KEY,
             extracted INTEGER NOT NULL,
             inferred INTEGER NOT NULL,
             ambiguous INTEGER NOT NULL
         ) WITHOUT ROWID;
         CREATE TABLE graph_confidence_dirty (document_id TEXT PRIMARY KEY) WITHOUT ROWID;
         CREATE TRIGGER graph_confidence_link_insert AFTER INSERT ON links BEGIN
             INSERT INTO graph_confidence_dirty SELECT new.source_document_id
             WHERE NOT EXISTS (SELECT 1 FROM graph_confidence_dirty
                               WHERE document_id = new.source_document_id);
         END;
         CREATE TRIGGER graph_confidence_link_delete AFTER DELETE ON links BEGIN
             INSERT INTO graph_confidence_dirty SELECT old.source_document_id
             WHERE NOT EXISTS (SELECT 1 FROM graph_confidence_dirty
                               WHERE document_id = old.source_document_id);
         END;
         CREATE TRIGGER graph_confidence_link_update AFTER UPDATE ON links BEGIN
             INSERT INTO graph_confidence_dirty SELECT old.source_document_id
             WHERE NOT EXISTS (SELECT 1 FROM graph_confidence_dirty
                               WHERE document_id = old.source_document_id);
             INSERT INTO graph_confidence_dirty SELECT new.source_document_id
             WHERE NOT EXISTS (SELECT 1 FROM graph_confidence_dirty
                               WHERE document_id = new.source_document_id);
         END;
         CREATE TRIGGER graph_confidence_extension_update AFTER UPDATE OF extension ON documents
         WHEN old.extension IS NOT new.extension BEGIN
             INSERT INTO graph_confidence_dirty SELECT new.id
             WHERE NOT EXISTS (SELECT 1 FROM graph_confidence_dirty WHERE document_id = new.id);
             INSERT INTO graph_confidence_dirty
             SELECT DISTINCT links.source_document_id FROM links
             WHERE links.resolved_target_id = new.id
               AND NOT EXISTS (SELECT 1 FROM graph_confidence_dirty
                               WHERE document_id = links.source_document_id);
         END;",
    )?;
    recompute_all(transaction)
}

/// Fold the links of `sources` (every source when `None`) into per-source
/// counts. Sources without note-to-note links are absent.
fn fold_sources(
    connection: &Connection,
    sources: Option<&[String]>,
) -> Result<HashMap<String, GraphConfidenceBreakdown>, rusqlite::Error> {
    let (sql, ids) = match sources {
        Some(sources) => (
            format!(
                "{FOLD_SQL} AND links.source_document_id IN (SELECT value FROM json_each(?1)){FOLD_ORDER}"
            ),
            Some(serde_json::to_string(sources).expect("ids serialize")),
        ),
        None => (format!("{FOLD_SQL}{FOLD_ORDER}"), None),
    };
    let mut statement = connection.prepare(&sql)?;
    let mut rows = match &ids {
        Some(ids) => statement.query([ids])?,
        None => statement.query([])?,
    };
    let mut totals = HashMap::<String, GraphConfidenceBreakdown>::new();
    // The current pair: source, target, folded confidence, and link count.
    let mut pair: Option<(String, String, (LinkConfidence, f64), usize)> = None;
    let mut finish = |pair: Option<(String, String, (LinkConfidence, f64), usize)>| {
        if let Some((source, _, (confidence, _), links)) = pair {
            let breakdown = totals.entry(source).or_default();
            for _ in 0..links {
                breakdown.add(confidence);
            }
        }
    };
    while let Some(row) = rows.next()? {
        let (source, target): (String, String) = (row.get(0)?, row.get(1)?);
        let next = (
            LinkConfidence::from_db(&row.get::<_, String>(2)?),
            row.get::<_, f64>(3)?,
        );
        match pair.as_mut() {
            Some((current_source, current_target, existing, links))
                if *current_source == source && *current_target == target =>
            {
                prefer_edge_confidence(existing, next);
                *links += 1;
            }
            _ => finish(pair.replace((source, target, next, 1))),
        }
    }
    finish(pair);
    Ok(totals)
}

fn store(
    transaction: &Transaction<'_>,
    totals: &HashMap<String, GraphConfidenceBreakdown>,
) -> Result<(), rusqlite::Error> {
    let mut insert = transaction.prepare(
        "INSERT OR REPLACE INTO graph_confidence_sources
             (document_id, extracted, inferred, ambiguous) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (source, counts) in totals {
        insert.execute(rusqlite::params![
            source,
            counts.extracted,
            counts.inferred,
            counts.ambiguous
        ])?;
    }
    Ok(())
}

/// Recompute every source, as after a full rebuild.
pub(crate) fn recompute_all(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "DELETE FROM graph_confidence_sources; DELETE FROM graph_confidence_dirty;",
    )?;
    store(transaction, &fold_sources(transaction, None)?)
}

/// Recompute the sources marked dirty since the last recompute.
pub(crate) fn recompute_dirty(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    let dirty = dirty_sources(transaction)?;
    if dirty.is_empty() {
        return Ok(());
    }
    transaction.execute(
        "DELETE FROM graph_confidence_sources
         WHERE document_id IN (SELECT document_id FROM graph_confidence_dirty)",
        [],
    )?;
    store(transaction, &fold_sources(transaction, Some(&dirty))?)?;
    transaction.execute("DELETE FROM graph_confidence_dirty", [])?;
    Ok(())
}

fn dirty_sources(connection: &Connection) -> Result<Vec<String>, rusqlite::Error> {
    connection
        .prepare("SELECT document_id FROM graph_confidence_dirty")?
        .query_map([], |row| row.get(0))?
        .collect()
}

/// The whole-graph breakdown: stored clean sources plus a live fold of dirty
/// ones, read in one snapshot.
pub(crate) fn read(connection: &Connection) -> Result<GraphConfidenceBreakdown, rusqlite::Error> {
    let snapshot = connection.unchecked_transaction()?;
    let mut breakdown = snapshot.query_row(
        "SELECT COALESCE(SUM(extracted), 0), COALESCE(SUM(inferred), 0),
                COALESCE(SUM(ambiguous), 0)
         FROM graph_confidence_sources
         WHERE document_id NOT IN (SELECT document_id FROM graph_confidence_dirty)",
        [],
        |row| {
            Ok(GraphConfidenceBreakdown {
                extracted: row.get(0)?,
                inferred: row.get(1)?,
                ambiguous: row.get(2)?,
            })
        },
    )?;
    let dirty = dirty_sources(&snapshot)?;
    if !dirty.is_empty() {
        for counts in fold_sources(&snapshot, Some(&dirty))?.values() {
            breakdown.extracted += counts.extracted;
            breakdown.inferred += counts.inferred;
            breakdown.ambiguous += counts.ambiguous;
        }
    }
    snapshot.finish()?;
    Ok(breakdown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan_vault, CacheDatabase, ScanMode, VaultPaths};
    use std::fs;
    use tempfile::TempDir;

    /// The former whole-graph query: every note-to-note link in path order,
    /// each pair folded in byte order. The stored totals must always equal it.
    fn path_ordered_reference(connection: &Connection) -> GraphConfidenceBreakdown {
        let mut statement = connection
            .prepare(
                "SELECT source.id, target.id, links.confidence, links.confidence_score
                 FROM links
                 JOIN documents AS source ON source.id = links.source_document_id
                 JOIN documents AS target ON target.id = links.resolved_target_id
                 WHERE source.extension = 'md' AND target.extension = 'md'
                 ORDER BY source.path, target.path, links.byte_offset",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    LinkConfidence::from_db(&row.get::<_, String>(2)?),
                    row.get::<_, f64>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut breakdown = GraphConfidenceBreakdown::default();
        let mut index = 0;
        while index < rows.len() {
            let (source, target, confidence, score) = &rows[index];
            let mut folded = (*confidence, *score);
            let mut end = index + 1;
            while end < rows.len() && rows[end].0 == *source && rows[end].1 == *target {
                prefer_edge_confidence(&mut folded, (rows[end].2, rows[end].3));
                end += 1;
            }
            for _ in index..end {
                breakdown.add(folded.0);
            }
            index = end;
        }
        breakdown
    }

    fn check(paths: &VaultPaths, step: &str, after_scan: bool) -> GraphConfidenceBreakdown {
        let database = CacheDatabase::open(paths).unwrap();
        let connection = database.connection();
        let stored = read(connection).unwrap();
        assert_eq!(stored, path_ordered_reference(connection), "{step}");
        let dirty = dirty_sources(connection).unwrap();
        if after_scan {
            assert!(dirty.is_empty(), "{step}: a scan leaves no dirty source");
        }
        let orphaned: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM graph_confidence_sources
                 WHERE document_id NOT IN (SELECT id FROM documents WHERE extension = 'md')
                   AND document_id NOT IN (SELECT document_id FROM graph_confidence_dirty)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphaned, 0, "{step}: clean rows belong to existing notes");
        stored
    }

    #[test]
    fn stored_totals_equal_a_full_recount_after_every_kind_of_change() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let paths = VaultPaths::new(root);
        fs::create_dir_all(paths.vulcan_dir()).unwrap();
        fs::write(
            root.join("A.md"),
            "[[B]] [[B]] [[C]] [[Missing]] ![[pic.png]]\n",
        )
        .unwrap();
        fs::write(root.join("B.md"), "[[A]]\n").unwrap();
        fs::write(root.join("C.md"), "[[C]]\n").unwrap();
        fs::write(root.join("pic.png"), [0_u8; 8]).unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let first = check(&paths, "full scan", true);
        // A→B twice, A→C, B→A, and C's self-link; not the missing or image links.
        assert_eq!(first.extracted, 5);

        // Writers outside the scan (accepted suggestions, feedback) change
        // confidence and add inferred edges; reads fold those sources live.
        let database = CacheDatabase::open(&paths).unwrap();
        let connection = database.connection();
        connection
            .execute_batch(
                "UPDATE links SET confidence = 'AMBIGUOUS', confidence_score = 0.4
                 WHERE id = (SELECT links.id FROM links JOIN documents AS t
                             ON t.id = links.resolved_target_id
                             WHERE t.path = 'B.md' ORDER BY byte_offset LIMIT 1);
                 UPDATE links SET confidence = 'INFERRED', confidence_score = 0.9
                 WHERE id = (SELECT links.id FROM links JOIN documents AS t
                             ON t.id = links.resolved_target_id
                             WHERE t.path = 'B.md' ORDER BY byte_offset LIMIT 1 OFFSET 1);
                 INSERT INTO links (id, source_document_id, raw_text, link_kind,
                                    resolved_target_id, origin_context, byte_offset,
                                    confidence, confidence_score)
                 SELECT 'inferred-edge', b.id, 'C', 'wikilink', c.id, 'body', 99,
                        'INFERRED', 0.7
                 FROM documents AS b, documents AS c
                 WHERE b.path = 'B.md' AND c.path = 'C.md';",
            )
            .unwrap();
        drop(database);
        let live = check(&paths, "unscanned writes", false);
        assert_eq!((live.extracted, live.inferred, live.ambiguous), (3, 3, 0));

        // Each scan recomputes what changed: edits, extension changes,
        // deletions, and a full rebuild.
        fs::write(root.join("A.md"), "[[B]] [[C]] [[C]]\n").unwrap();
        scan_vault(&paths, ScanMode::Incremental).unwrap();
        check(&paths, "edited source", true);
        fs::rename(root.join("C.md"), root.join("C.txt")).unwrap();
        scan_vault(&paths, ScanMode::Incremental).unwrap();
        check(&paths, "target no longer a note", true);
        fs::remove_file(root.join("B.md")).unwrap();
        scan_vault(&paths, ScanMode::Incremental).unwrap();
        check(&paths, "deleted source and target", true);
        fs::write(root.join("D.md"), "[[A]] [[A]]\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        check(&paths, "full rebuild", true);
    }

    #[test]
    fn an_extension_change_marks_the_note_and_every_source_pointing_at_it() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let paths = VaultPaths::new(root);
        fs::create_dir_all(paths.vulcan_dir()).unwrap();
        fs::write(root.join("A.md"), "[[T]]\n").unwrap();
        fs::write(root.join("T.md"), "[[A]]\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let database = CacheDatabase::open(&paths).unwrap();
        let connection = database.connection();
        connection
            .execute(
                "UPDATE documents SET extension = 'txt' WHERE path = 'T.md'",
                [],
            )
            .unwrap();
        let mut dirty = dirty_sources(connection).unwrap();
        dirty.sort();
        let mut expected = connection
            .prepare("SELECT id FROM documents ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        expected.sort();
        assert_eq!(dirty, expected);
        assert_eq!(
            read(connection).unwrap(),
            path_ordered_reference(connection)
        );
    }
}
