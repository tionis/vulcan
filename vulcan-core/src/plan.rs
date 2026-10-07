//! The note-store planner (QRY.5, `docs/specs/query-architecture.md` §4.5).
//!
//! Every note frontend (DQL, the note filter language behind `QueryAst`,
//! `ls`, saved reports, Bases, and search, the Tasks DSL, and `DataviewJS`
//! page selection) compiles its question into a [`NotePlan`]: a source that
//! selects candidates in SQL, shared predicate atoms that decide candidates
//! from stored fields, and the hydration its residual program and output
//! need. [`execute_note_plan`] runs those stages over an
//! [`IndexedNoteLookup`] and reports what it did; the frontend then runs its
//! residual (DQL commands, Bases views, Tasks filters) on the rows.

use crate::note_lookup::{IndexedNoteLookup, NoteLookup};
use crate::paths::VaultPaths;
use crate::permissions::PermissionFilter;
use crate::predicate::{Decision, Dialect, Predicate, RecordValues};
use crate::properties::PropertyError;
use crate::source::{SourceColumns, SourceExpr};
use crate::CacheDatabase;
use rusqlite::types::Value as SqlValue;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Instant;

/// What a frontend asks of the note store.
pub(crate) struct NotePlan<'a> {
    /// Names the frontend in explain output.
    pub frontend: &'static str,
    /// Candidate selection; `None` is every readable document.
    pub source: Option<SourceExpr>,
    /// Restrict candidates to Markdown notes.
    pub markdown_only: bool,
    /// Decides candidates from stored fields; [`Predicate::Unknown`] decides
    /// nothing.
    pub predicate: Predicate,
    /// Which notes' file objects the residual and output read.
    pub hydration: Hydration<'a>,
    /// Further notes to hydrate, such as the note containing a query.
    pub also_hydrate: Vec<String>,
}

/// Which notes a plan hydrates.
pub(crate) enum Hydration<'a> {
    /// Every row that remains after decisions.
    Rows,
    /// Every remaining row, with stored fields only: the frontend reads no
    /// row's tags, links, tasks, or lists.
    Stored,
    /// Only rows the predicate leaves undecided: the residual reads them,
    /// and the output needs paths only.
    Undecided,
    /// These paths, whatever the rows (task-bearing notes for the Tasks DSL).
    Paths(&'a HashSet<String>),
}

/// What a plan did, for `--explain` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPlanExplain {
    pub frontend: String,
    /// How candidates were selected.
    pub candidate_path: String,
    pub candidates: usize,
    /// Candidates the predicate decided as matches, so the residual need
    /// not decide them.
    pub decided_matches: usize,
    /// Candidates the predicate decided as non-matches; never hydrated.
    pub decided_out: usize,
    /// Candidates left for the frontend's residual.
    pub residual: usize,
    /// Rows loaded with stored fields only, never hydrated.
    #[serde(default)]
    pub stored: usize,
    /// Notes whose file objects were hydrated.
    pub hydrated: usize,
    pub stages: Vec<QueryPlanStage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPlanStage {
    pub name: String,
    pub micros: u64,
}

/// A plan's rows in path order.
#[derive(Debug)]
pub(crate) struct PlannedRows {
    pub rows: Vec<String>,
    /// Rows the predicate left undecided.
    pub undecided: HashSet<String>,
    /// Whether rows were hydrated rather than loaded with stored fields.
    pub rows_hydrated: bool,
    pub explain: QueryPlanExplain,
}

/// Run a plan's shared stages: candidates, decisions, hydration.
#[allow(clippy::too_many_lines)]
pub(crate) fn execute_note_plan(
    paths: &VaultPaths,
    lookup: &IndexedNoteLookup<'_>,
    plan: &NotePlan<'_>,
    filter: Option<&PermissionFilter>,
) -> Result<PlannedRows, PropertyError> {
    let mut stages = Vec::new();
    let mut stage = |name: &str, started: Instant| {
        stages.push(QueryPlanStage {
            name: name.to_string(),
            micros: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
        });
    };

    let started = Instant::now();
    // SQL narrows by source and by what the predicate cannot exclude, and
    // reads the facts the predicate decides on, so deciding loads no note.
    // A lookup over retained records decides on those instead (QRY.6).
    let narrowing = plan.predicate.is_useful().then_some(&plan.predicate);
    let from_records = narrowing.is_some() && lookup.retains_records();
    // Without a source, retained records decide faster in memory than SQL
    // scans every row; the identities are already the read scope.
    let in_memory = plan.source.is_none() && (narrowing.is_none() || from_records);
    let (candidate_count, decided) = if let (true, Some(records)) = (in_memory, lookup.all_stored())
    {
        // Every record is at hand in path order: decide them where they are.
        let candidate_count = records
            .iter()
            .filter(|record| !plan.markdown_only || is_markdown(&record.document_path))
            .count();
        stage("candidates", started);
        let started = Instant::now();
        let decided = decide_records(
            &plan.predicate,
            records,
            plan.markdown_only,
            !lookup.retains_records(),
        );
        stage("decide", started);
        (candidate_count, decided)
    } else {
        let mut candidates = if in_memory {
            lookup
                .paths()
                .filter(|path| !plan.markdown_only || is_markdown(path))
                .map(|path| CandidateFacts {
                    path: path.to_string(),
                    ..CandidateFacts::default()
                })
                .collect()
        } else {
            sql_candidates(
                lookup.database(),
                paths,
                &CandidateQuery {
                    source: plan.source.as_ref(),
                    predicate: narrowing,
                    markdown_only: plan.markdown_only,
                    with_properties: !from_records,
                },
                filter,
            )?
            .into_iter()
            .filter(|candidate| lookup.contains(&candidate.path))
            .collect::<Vec<_>>()
        };
        candidates.sort_by(|left, right| left.path.cmp(&right.path));
        if from_records {
            lookup.prefetch_stored(candidates.iter().map(|candidate| candidate.path.as_str()));
            for candidate in &mut candidates {
                candidate.record = lookup.note_arc_at(&candidate.path);
            }
            if let Some(error) = lookup.take_error() {
                return Err(error);
            }
        }
        let candidate_count = candidates.len();
        stage("candidates", started);
        let started = Instant::now();
        let decided = decide_candidates(&plan.predicate, candidates, !lookup.retains_records());
        stage("decide", started);
        (candidate_count, decided)
    };
    let (candidates, undecided) = (decided.rows, decided.undecided);
    let (decided_matches, decided_out) = (decided.matches, decided.excluded);

    let started = Instant::now();
    let stored = if matches!(plan.hydration, Hydration::Stored) {
        lookup.prefetch_stored(candidates.iter().map(String::as_str));
        candidates.len()
    } else {
        0
    };
    let hydrate = match &plan.hydration {
        Hydration::Rows => candidates.clone(),
        Hydration::Stored => Vec::new(),
        Hydration::Undecided => candidates
            .iter()
            .filter(|path| undecided.contains(*path))
            .cloned()
            .collect(),
        Hydration::Paths(paths) => paths.iter().cloned().collect(),
    };
    lookup.prefetch_hydrated(hydrate.iter().chain(&plan.also_hydrate).map(String::as_str));
    stage("hydrate", started);
    if let Some(error) = lookup.take_error() {
        return Err(error);
    }

    Ok(PlannedRows {
        explain: QueryPlanExplain {
            frontend: plan.frontend.to_string(),
            candidate_path: match (&plan.source, plan.predicate.is_useful()) {
                (Some(_), true) => "sql source and predicate atoms".to_string(),
                (Some(_), false) => "sql source".to_string(),
                (None, true) if in_memory => "predicate atoms over retained notes".to_string(),
                (None, true) => "sql predicate atoms".to_string(),
                (None, false) if plan.markdown_only => "every readable note".to_string(),
                (None, false) => "every readable document".to_string(),
            },
            candidates: candidate_count,
            decided_matches,
            decided_out,
            residual: undecided.len(),
            stored,
            hydrated: hydrate.len(),
            stages,
        },
        rows: candidates,
        undecided,
        rows_hydrated: !matches!(plan.hydration, Hydration::Stored),
    })
}

fn is_markdown(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// Candidates after the predicate: the rows it does not exclude, in order.
struct DecidedCandidates {
    rows: Vec<String>,
    undecided: HashSet<String>,
    matches: usize,
    excluded: usize,
}

/// Decide every record (in path order) on its stored fields; only rows the
/// predicate keeps allocate their paths. One-shot queries decide in
/// parallel; a host's retained lookups serve concurrent requests, which
/// already occupy its cores, and per-request fan-out there only queues
/// requests behind each other (QRY.6), so they decide on the request's
/// thread.
fn decide_records(
    predicate: &Predicate,
    records: &[std::sync::Arc<crate::properties::NoteRecord>],
    markdown_only: bool,
    parallel: bool,
) -> DecidedCandidates {
    use rayon::prelude::*;
    let useful = predicate.is_useful();
    let decide = |record: &std::sync::Arc<crate::properties::NoteRecord>| {
        if markdown_only && !is_markdown(&record.document_path) {
            return None;
        }
        Some(if useful {
            predicate.decide(
                Dialect::Dataview,
                &RecordValues {
                    properties: &record.properties,
                    path: &record.document_path,
                    name: &record.file_name,
                    ext: &record.file_ext,
                },
            )
        } else {
            Decision::Undecided
        })
    };
    let decisions = if parallel {
        records.par_iter().map(decide).collect::<Vec<_>>()
    } else {
        records.iter().map(decide).collect::<Vec<_>>()
    };
    let mut decided = DecidedCandidates {
        rows: Vec::new(),
        undecided: HashSet::new(),
        matches: 0,
        excluded: 0,
    };
    for (record, decision) in records.iter().zip(decisions) {
        match decision {
            None => {}
            Some(Decision::NoMatch) => decided.excluded += 1,
            Some(Decision::Match) => {
                decided.matches += usize::from(useful);
                decided.rows.push(record.document_path.clone());
            }
            Some(Decision::Undecided) => {
                decided.undecided.insert(record.document_path.clone());
                decided.rows.push(record.document_path.clone());
            }
        }
    }
    decided
}

/// Decide candidates on their stored facts, in parallel; no note loads.
/// See [`decide_records`] for `parallel`.
fn decide_candidates(
    predicate: &Predicate,
    candidates: Vec<CandidateFacts>,
    parallel: bool,
) -> DecidedCandidates {
    use rayon::prelude::*;
    if !predicate.is_useful() {
        let rows = candidates
            .into_iter()
            .map(|candidate| candidate.path)
            .collect::<Vec<_>>();
        return DecidedCandidates {
            undecided: rows.iter().cloned().collect(),
            rows,
            matches: 0,
            excluded: 0,
        };
    }
    let decide = |candidate: &CandidateFacts| {
        if let Some(record) = &candidate.record {
            return predicate.decide(
                Dialect::Dataview,
                &RecordValues {
                    properties: &record.properties,
                    path: &record.document_path,
                    name: &record.file_name,
                    ext: &record.file_ext,
                },
            );
        }
        let properties = candidate
            .properties
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
        predicate.decide(
            Dialect::Dataview,
            &RecordValues {
                properties: &properties,
                path: &candidate.path,
                name: &candidate.name,
                ext: &candidate.ext,
            },
        )
    };
    let decisions = if parallel {
        candidates.par_iter().map(decide).collect::<Vec<_>>()
    } else {
        candidates.iter().map(decide).collect::<Vec<_>>()
    };
    let mut decided = DecidedCandidates {
        rows: Vec::new(),
        undecided: HashSet::new(),
        matches: 0,
        excluded: 0,
    };
    for (candidate, decision) in candidates.into_iter().zip(decisions) {
        match decision {
            Decision::Match => decided.matches += 1,
            Decision::NoMatch => {
                decided.excluded += 1;
                continue;
            }
            Decision::Undecided => {
                decided.undecided.insert(candidate.path.clone());
            }
        }
        decided.rows.push(candidate.path);
    }
    decided
}

/// What deciding a candidate reads: identity and stored properties.
#[derive(Debug, Default)]
struct CandidateFacts {
    path: String,
    name: String,
    ext: String,
    /// Canonical JSON properties; `None` reads as no properties.
    properties: Option<String>,
    /// The candidate's stored record, decided on instead of `properties`
    /// when the lookup retains records.
    record: Option<std::sync::Arc<crate::properties::NoteRecord>>,
}

/// What [`sql_candidates`] selects.
struct CandidateQuery<'a> {
    source: Option<&'a SourceExpr>,
    predicate: Option<&'a Predicate>,
    markdown_only: bool,
    /// Read the properties the predicate decides on.
    with_properties: bool,
}

/// The documents `source` selects within the read scope, in one query.
pub(crate) fn source_candidates(
    paths: &VaultPaths,
    source: &SourceExpr,
    markdown_only: bool,
    filter: Option<&PermissionFilter>,
) -> Result<HashSet<String>, PropertyError> {
    Ok(sql_candidates(
        None,
        paths,
        &CandidateQuery {
            source: Some(source),
            predicate: None,
            markdown_only,
            with_properties: false,
        },
        filter,
    )?
    .into_iter()
    .map(|candidate| candidate.path)
    .collect())
}

/// The documents a source selects (all without one) whose stored fields the
/// predicate does not decide as non-matches, within the read scope, with
/// the facts the predicate decides on.
fn sql_candidates(
    database: Option<&CacheDatabase>,
    paths: &VaultPaths,
    query: &CandidateQuery<'_>,
    filter: Option<&PermissionFilter>,
) -> Result<Vec<CandidateFacts>, PropertyError> {
    let CandidateQuery {
        source,
        predicate,
        markdown_only,
        with_properties,
    } = *query;
    let opened;
    let database = if let Some(database) = database {
        database
    } else {
        opened = CacheDatabase::open(paths)?;
        &opened
    };
    let permission_sql = filter.map(|filter| {
        filter.document_scope_sql_for("_permission_documents", "note_query.document_id")
    });
    let mut params = permission_sql
        .as_ref()
        .map(|sql| {
            sql.params
                .iter()
                .cloned()
                .map(SqlValue::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut sql = permission_sql
        .as_ref()
        .map_or_else(String::new, |sql| sql.cte.clone());
    // The narrow table alone: wide `documents` rows are never read.
    sql.push_str(if with_properties {
        "SELECT note_query.path, note_query.filename, note_query.extension, \
                json(note_query.properties) \
         FROM note_query WHERE 1 = 1"
    } else {
        "SELECT note_query.path, note_query.filename, note_query.extension, NULL \
         FROM note_query WHERE 1 = 1"
    });
    if markdown_only {
        sql.push_str(" AND note_query.extension = 'md'");
    }
    if let Some(source) = source {
        sql.push_str(" AND ");
        sql.push_str(&source.render_sql(&SourceColumns::NOTE_QUERY, &mut params));
    }
    if let Some(predicate) = predicate {
        sql.push_str(" AND ");
        sql.push_str(&predicate.render_possible_match(
            Dialect::Dataview,
            &crate::predicate::SqlColumns {
                properties: "COALESCE(note_query.properties, jsonb('{}'))",
                path: "note_query.path",
                name: "note_query.filename",
                ext: "note_query.extension",
            },
            &mut params,
        ));
    }
    if let Some(permission_sql) = permission_sql.as_ref() {
        sql.push_str(&permission_sql.clause);
    }
    let mut statement = database.connection().prepare_cached(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        Ok(CandidateFacts {
            path: row.get(0)?,
            name: row.get(1)?,
            ext: row.get(2)?,
            properties: row.get(3)?,
            record: None,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::parse::Parser;
    use crate::properties::{load_indexed_note_lookup, NoteIndexReadScope};
    use crate::{scan_vault, ScanMode};
    use tempfile::TempDir;

    fn vault() -> (TempDir, VaultPaths) {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path().join("vault");
        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::create_dir_all(root.join("A")).unwrap();
        for (path, contents) in [
            ("A/Open.md", "---\nstatus: open\n---\n"),
            ("A/Done.md", "---\nstatus: done\n---\n"),
            ("A/Odd.md", "---\nstatus: 2026-01-01\n---\n"),
            ("B.md", "---\nstatus: open\n---\n#t\n"),
            ("image.png", "png"),
        ] {
            std::fs::write(root.join(path), contents).unwrap();
        }
        let paths = VaultPaths::new(&root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        (temp_dir, paths)
    }

    fn predicate(source: &str) -> Predicate {
        Predicate::lower_dataview(&Parser::new(source).unwrap().parse().unwrap())
    }

    #[test]
    fn plans_select_decide_and_hydrate_what_they_name() {
        let (_temp, paths) = vault();
        let run = |plan: &NotePlan<'_>| {
            let lookup =
                load_indexed_note_lookup(&paths, NoteIndexReadScope::Filter(None)).unwrap();
            let planned = execute_note_plan(&paths, &lookup, plan, None).unwrap();
            let hydrated = ["A/Open.md", "A/Done.md", "A/Odd.md", "B.md", "image.png"]
                .into_iter()
                .filter(|path| lookup.is_hydrated(path))
                .collect::<Vec<_>>();
            (planned, hydrated)
        };
        // A folder source and the predicate: a date-like string compared
        // with a number stays undecided for the evaluator.
        let (planned, hydrated) = run(&NotePlan {
            frontend: "test",
            source: Some(SourceExpr::Folder("A".into())),
            markdown_only: true,
            predicate: predicate("status != 3"),
            hydration: Hydration::Rows,
            also_hydrate: vec!["B.md".into()],
        });
        assert_eq!(planned.rows, ["A/Done.md", "A/Odd.md", "A/Open.md"]);
        assert_eq!(planned.undecided, HashSet::from(["A/Odd.md".to_string()]));
        assert_eq!(
            (
                planned.explain.decided_matches,
                planned.explain.residual,
                planned.explain.candidate_path.as_str()
            ),
            (2, 1, "sql source and predicate atoms")
        );
        assert_eq!(hydrated, ["A/Open.md", "A/Done.md", "A/Odd.md", "B.md"]);

        // Decided non-matches never hydrate.
        let (planned, hydrated) = run(&NotePlan {
            frontend: "test",
            source: None,
            markdown_only: true,
            predicate: predicate("status = \"open\""),
            hydration: Hydration::Rows,
            also_hydrate: Vec::new(),
        });
        assert_eq!(planned.rows, ["A/Open.md", "B.md"]);
        assert_eq!(hydrated, ["A/Open.md", "B.md"]);

        // Undecided hydration reads only what the residual evaluates.
        let (planned, hydrated) = run(&NotePlan {
            frontend: "test",
            source: None,
            markdown_only: true,
            predicate: predicate("status != 3"),
            hydration: Hydration::Undecided,
            also_hydrate: Vec::new(),
        });
        assert_eq!(planned.rows, ["A/Done.md", "A/Odd.md", "A/Open.md", "B.md"]);
        assert_eq!(hydrated, ["A/Odd.md"]);

        // Without a source or predicate: every readable document, or note.
        let task_paths = HashSet::from(["B.md".to_string()]);
        let (planned, hydrated) = run(&NotePlan {
            frontend: "test",
            source: None,
            markdown_only: false,
            predicate: Predicate::Unknown,
            hydration: Hydration::Paths(&task_paths),
            also_hydrate: Vec::new(),
        });
        assert_eq!(planned.rows.len(), 5);
        assert_eq!(planned.explain.candidate_path, "every readable document");
        assert_eq!(hydrated, ["B.md"]);
        let (planned, _) = run(&NotePlan {
            frontend: "test",
            source: Some(SourceExpr::Tag("t".into())),
            markdown_only: true,
            predicate: Predicate::Unknown,
            hydration: Hydration::Rows,
            also_hydrate: Vec::new(),
        });
        assert_eq!(planned.rows, ["B.md"]);
    }
}
