//! Durable link-suggestion decisions kept outside the rebuildable cache.
//!
//! Accepting or rejecting a suggestion is user curation, so it must survive `reindex`, breaking
//! cache migrations, and edits that rebuild a note's derived link rows. Decisions are stored by
//! vault-relative path pair in device-local operational state and projected back into the cache
//! (`link_suggestions` rows plus `INFERRED` links) after every scan.

use crate::graph::LinkConfidence;
use crate::VaultPaths;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use ulid::Ulid;

const FEEDBACK_FILE_NAME: &str = "link-suggestion-feedback.json";
const FEEDBACK_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct LinkFeedbackDecision {
    pub source_path: String,
    pub target_path: String,
    /// `accepted` or `rejected`.
    pub status: String,
    pub score: f64,
    pub decided_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LinkFeedbackFile {
    version: u32,
    decisions: Vec<LinkFeedbackDecision>,
}

fn feedback_path(paths: &VaultPaths) -> std::io::Result<PathBuf> {
    Ok(paths.operational_state_dir()?.join(FEEDBACK_FILE_NAME))
}

fn load(paths: &VaultPaths) -> std::io::Result<Option<LinkFeedbackFile>> {
    let path = feedback_path(paths)?;
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let file: LinkFeedbackFile = serde_json::from_slice(&contents).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "invalid link-suggestion feedback in {}: {error}",
                path.display()
            ),
        )
    })?;
    if file.version != FEEDBACK_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "unsupported link-suggestion feedback version {} in {}",
                file.version,
                path.display()
            ),
        ));
    }
    Ok(Some(file))
}

fn store(paths: &VaultPaths, file: &LinkFeedbackFile) -> std::io::Result<()> {
    let path = feedback_path(paths)?;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("feedback path has no parent directory"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, file).map_err(std::io::Error::other)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

/// Records (or replaces) the decision for one source/target pair.
pub(crate) fn record(paths: &VaultPaths, decision: LinkFeedbackDecision) -> std::io::Result<()> {
    let mut file = load(paths)?.unwrap_or(LinkFeedbackFile {
        version: FEEDBACK_VERSION,
        decisions: Vec::new(),
    });
    file.decisions.retain(|existing| {
        existing.source_path != decision.source_path || existing.target_path != decision.target_path
    });
    file.decisions.push(decision);
    store(paths, &file)
}

/// Follows renames that kept document identity so decisions stay attached to their notes.
pub(crate) fn rename_paths(
    paths: &VaultPaths,
    renames: &[(String, String)],
) -> std::io::Result<()> {
    if renames.is_empty() {
        return Ok(());
    }
    let Some(mut file) = load(paths)? else {
        return Ok(());
    };
    let mut changed = false;
    for decision in &mut file.decisions {
        for (old_path, new_path) in renames {
            if &decision.source_path == old_path {
                decision.source_path.clone_from(new_path);
                changed = true;
            }
            if &decision.target_path == old_path {
                decision.target_path.clone_from(new_path);
                changed = true;
            }
        }
    }
    if changed {
        store(paths, &file)?;
    }
    Ok(())
}

/// Projects durable decisions back into the cache. Pairs whose notes are absent stay recorded
/// but are skipped, so a later restore of the note restores the decision too.
pub(crate) fn restore(paths: &VaultPaths, connection: &Connection) -> std::io::Result<()> {
    let Some(file) = load(paths)? else {
        return Ok(());
    };
    restore_decisions(connection, &file.decisions).map_err(std::io::Error::other)
}

fn restore_decisions(
    connection: &Connection,
    decisions: &[LinkFeedbackDecision],
) -> rusqlite::Result<()> {
    let document_id = |path: &str| -> rusqlite::Result<Option<String>> {
        connection
            .query_row(
                "SELECT id FROM documents WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()
    };
    for decision in decisions {
        let accepted = match decision.status.as_str() {
            "accepted" => true,
            "rejected" => false,
            _ => continue,
        };
        let (Some(source_id), Some(target_id)) = (
            document_id(&decision.source_path)?,
            document_id(&decision.target_path)?,
        ) else {
            continue;
        };
        connection.execute(
            "
            INSERT INTO link_suggestions (
                id, source_document_id, target_document_id, score, signals, status, created_at,
                accepted_at, rejected_at
            )
            VALUES (?1, ?2, ?3, ?4, '{}', ?5, ?6, ?7, ?8)
            ON CONFLICT(source_document_id, target_document_id) DO UPDATE SET
                status = excluded.status,
                accepted_at = excluded.accepted_at,
                rejected_at = excluded.rejected_at
            ",
            params![
                Ulid::new().to_string(),
                source_id,
                target_id,
                decision.score,
                decision.status,
                decision.decided_at,
                accepted.then_some(&decision.decided_at),
                (!accepted).then_some(&decision.decided_at),
            ],
        )?;
        if accepted {
            insert_inferred_link(connection, &source_id, &target_id, decision.score)?;
        }
    }
    Ok(())
}

/// Inserts the cache-local `INFERRED` edge for an accepted suggestion unless the pair is
/// already linked.
pub(crate) fn insert_inferred_link(
    connection: &Connection,
    source_id: &str,
    target_id: &str,
    score: f64,
) -> rusqlite::Result<()> {
    let target_path: String = connection.query_row(
        "SELECT path FROM documents WHERE id = ?1",
        params![target_id],
        |row| row.get(0),
    )?;
    connection.execute(
        "
        INSERT INTO links (
            id, source_document_id, raw_text, link_kind, display_text, target_path_candidate,
            target_heading, target_block, resolved_target_id, origin_context, byte_offset,
            confidence, confidence_score
        )
        SELECT ?1, ?2, ?3, 'inferred', NULL, ?3, NULL, NULL, ?4, 'inferred', 0, ?5, ?6
        WHERE NOT EXISTS (
            SELECT 1 FROM links
            WHERE source_document_id = ?2 AND resolved_target_id = ?4
        )
        ",
        params![
            Ulid::new().to_string(),
            source_id,
            target_path,
            target_id,
            LinkConfidence::Inferred.as_str(),
            score.clamp(0.0, 1.0),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn decision(source: &str, target: &str, status: &str) -> LinkFeedbackDecision {
        LinkFeedbackDecision {
            source_path: source.to_string(),
            target_path: target.to_string(),
            status: status.to_string(),
            score: 0.5,
            decided_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn record_replaces_pair_and_rename_follows_paths() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::create_dir_all(temp_dir.path().join(".vulcan")).expect("vulcan dir");
        let paths = VaultPaths::new(temp_dir.path());

        record(&paths, decision("a.md", "b.md", "rejected")).expect("record");
        record(&paths, decision("a.md", "b.md", "accepted")).expect("record");
        record(&paths, decision("c.md", "a.md", "rejected")).expect("record");
        rename_paths(&paths, &[("a.md".to_string(), "x/a.md".to_string())]).expect("rename");

        let file = load(&paths).expect("load").expect("file exists");
        assert_eq!(
            file.decisions,
            vec![
                decision("x/a.md", "b.md", "accepted"),
                decision("c.md", "x/a.md", "rejected"),
            ]
        );
    }

    #[test]
    fn corrupt_feedback_fails_loudly() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::create_dir_all(temp_dir.path().join(".vulcan")).expect("vulcan dir");
        let paths = VaultPaths::new(temp_dir.path());
        fs::write(feedback_path(&paths).expect("path"), "not json").expect("write");

        let error = load(&paths).expect_err("corrupt file should fail");
        assert!(error.to_string().contains(FEEDBACK_FILE_NAME));
    }
}
