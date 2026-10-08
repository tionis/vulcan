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

use crate::note_lookup::{
    IndexedNoteLookup, MatchIndex, NoteLookup, OrderIndex, RecordMatcher, RecordOrdering,
};
use crate::paths::VaultPaths;
use crate::permissions::PermissionFilter;
use crate::predicate::{Decision, Dialect, Predicate, RecordValues};
use crate::properties::NoteRecord;
use crate::properties::PropertyError;
use crate::source::{SourceColumns, SourceExpr};
use crate::CacheDatabase;
use rusqlite::types::Value as SqlValue;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
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
    /// The frontend keeps only the first matches in an order it states; the
    /// plan may then return just those rows.
    pub top: Option<PlanTop<'a>>,
}

/// A frontend that sorts its matches (ties by path) and keeps the first
/// `take`. When a lookup holds every stored record up front and the
/// predicate decides every record, the plan walks the records in that order
/// and stops after `take` matches: the rows are then exactly the frontend's
/// first `take`, so sorting and truncating them gives its unchanged answer.
pub(crate) struct PlanTop<'a> {
    pub take: usize,
    /// `None` keeps path order.
    pub order: Option<PlanOrder<'a>>,
}

/// An order of every stored record, cached with the records under `key`.
pub(crate) struct PlanOrder<'a> {
    /// Names the frontend's comparison, sort key, and direction.
    pub key: String,
    pub ordering: &'a dyn RecordOrdering,
}

/// A frontend's sort as a key per record and a total order on keys, ties by
/// path. `key` returns the record's sort key and its class, or `None` to
/// decline; keys of distinct nonzero classes are incomparable, so an order
/// exists only while every key has class `0` or one shared class.
pub(crate) struct KeyedOrdering<K, F, C> {
    key: F,
    compare: C,
    keys: std::marker::PhantomData<fn() -> K>,
}

impl<K, F, C> KeyedOrdering<K, F, C>
where
    F: Fn(&Arc<NoteRecord>) -> Option<(K, u8)>,
    C: Fn(&K, &K) -> std::cmp::Ordering,
{
    pub(crate) fn new(key: F, compare: C) -> Self {
        Self {
            key,
            compare,
            keys: std::marker::PhantomData,
        }
    }
}

/// The shared class of two keys' classes; see [`KeyedOrdering`].
fn merge_class(left: u8, right: u8) -> Option<u8> {
    match (left, right) {
        (0, class) | (class, 0) => Some(class),
        (left, right) => (left == right).then_some(left),
    }
}

fn positions_u32(order: Vec<usize>) -> Option<Arc<[u32]>> {
    order
        .into_iter()
        .map(|position| u32::try_from(position).ok())
        .collect::<Option<Vec<_>>>()
        .map(Arc::from)
}

impl<K, F, C> RecordOrdering for KeyedOrdering<K, F, C>
where
    F: Fn(&Arc<NoteRecord>) -> Option<(K, u8)>,
    C: Fn(&K, &K) -> std::cmp::Ordering,
{
    fn build(&self, records: &[Arc<NoteRecord>]) -> Option<OrderIndex> {
        let mut class = 0;
        let mut keys = Vec::with_capacity(records.len());
        for record in records {
            let (key, key_class) = (self.key)(record)?;
            class = merge_class(class, key_class)?;
            keys.push(key);
        }
        let mut order = (0..records.len()).collect::<Vec<_>>();
        order.sort_by(|&left, &right| {
            (self.compare)(&keys[left], &keys[right]).then_with(|| {
                records[left]
                    .document_path
                    .cmp(&records[right].document_path)
            })
        });
        Some(OrderIndex {
            order: positions_u32(order)?,
            class,
        })
    }

    fn update(
        &self,
        previous: &OrderIndex,
        records: &[Arc<NoteRecord>],
        changed: &[usize],
    ) -> Option<OrderIndex> {
        let changed_set = changed.iter().copied().collect::<HashSet<_>>();
        let mut order = previous
            .order
            .iter()
            .map(|&position| position as usize)
            .filter(|position| !changed_set.contains(position))
            .collect::<Vec<_>>();
        let mut class = previous.class;
        for &position in changed {
            let record = records.get(position)?;
            let (key, key_class) = (self.key)(record)?;
            class = merge_class(class, key_class)?;
            // Unchanged records keep the keys they were ordered by.
            let mut declined = false;
            let at = order.partition_point(|&other| {
                let Some((other_key, _)) = records.get(other).and_then(&self.key) else {
                    declined = true;
                    return false;
                };
                (self.compare)(&other_key, &key)
                    .then_with(|| records[other].document_path.cmp(&record.document_path))
                    == std::cmp::Ordering::Less
            });
            if declined {
                return None;
            }
            order.insert(at, position);
        }
        Some(OrderIndex {
            order: positions_u32(order)?,
            class,
        })
    }
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
    let walked = walk_top(paths, lookup, plan, &mut stage);
    let ordered_walk = walked.is_some();
    let (candidate_count, decided) = if let Some(walked) = walked {
        stage("candidates", started);
        walked
    } else if let (true, Some(records)) = (in_memory, lookup.all_stored()) {
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
                // The lookup's universe is the read scope; membership below
                // filters candidates without scanning permitted ids.
                None,
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
                _ if ordered_walk => "ordered walk over retained notes".to_string(),
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

/// What a plan's source and predicate match among retained records.
struct PlanMatcher<'p, 'l> {
    paths: &'p VaultPaths,
    lookup: &'p IndexedNoteLookup<'l>,
    plan: &'p NotePlan<'p>,
}

fn bit(bits: &[u64], position: usize) -> bool {
    bits.get(position / 64)
        .is_some_and(|word| word & (1 << (position % 64)) != 0)
}

fn set_bit(bits: &mut [u64], position: usize, value: bool) {
    let mask = 1 << (position % 64);
    if value {
        bits[position / 64] |= mask;
    } else {
        bits[position / 64] &= !mask;
    }
}

/// Whether a record's membership in a source depends only on that record
/// (its path, tags, and outgoing links), so only changed records can join or
/// leave it. `LinkedFrom` depends on another document's links; a link's
/// target depends on other documents' identities, so after identities
/// changed, `LinksTo` does too.
fn per_document(source: &SourceExpr, identities_changed: bool) -> bool {
    match source {
        SourceExpr::Folder(_) | SourceExpr::Path(_) | SourceExpr::Tag(_) => true,
        SourceExpr::LinksTo(_) => !identities_changed,
        SourceExpr::LinkedFrom(_) => false,
        SourceExpr::And(children) | SourceExpr::Or(children) => children
            .iter()
            .all(|child| per_document(child, identities_changed)),
        SourceExpr::Not(inner) => per_document(inner, identities_changed),
    }
}

impl PlanMatcher<'_, '_> {
    /// The source's members as a bitset, selected in SQL as the plan
    /// otherwise would.
    fn source_bits(&self, source: &SourceExpr, len: usize) -> Option<Vec<u64>> {
        let mut bits = vec![0_u64; len.div_ceil(64)];
        self.mark_members(source, &mut bits)?;
        Some(bits)
    }

    /// Set the bits of `source`'s members, selected in SQL.
    fn mark_members(&self, source: &SourceExpr, bits: &mut [u64]) -> Option<()> {
        let index = self.lookup.identity_index();
        for candidate in sql_candidates(
            self.lookup.database(),
            self.paths,
            &CandidateQuery {
                source: Some(source),
                predicate: None,
                markdown_only: self.plan.markdown_only,
                with_properties: false,
            },
            None,
        )
        .ok()?
        {
            if let Some(position) = index
                .position(&candidate.path)
                .filter(|&at| at < bits.len() * 64)
            {
                set_bit(bits, position, true);
            }
        }
        Some(())
    }

    /// `previous` members with each changed record's membership probed
    /// again.
    fn changed_members(
        &self,
        source: &SourceExpr,
        previous: &[u64],
        records: &[Arc<NoteRecord>],
        changed: &[usize],
    ) -> Option<Vec<u64>> {
        let connection = self.lookup.database()?.connection();
        let mut bits = previous.to_vec();
        for &position in changed {
            let record = records.get(position)?;
            // The candidate query's Markdown restriction: the indexed
            // extension, which the record carries.
            let member = (!self.plan.markdown_only || record.file_ext == "md")
                && source
                    .contains_document(connection, &record.document_id, &record.document_path)
                    .ok()?;
            set_bit(&mut bits, position, member);
        }
        Some(bits)
    }

    /// Decide the record at `position`: `Some(matched)`, or `None` when it
    /// is undecided. Non-Markdown records never match a Markdown-only plan.
    fn decide(&self, records: &[Arc<NoteRecord>], position: usize) -> Option<bool> {
        let record = records.get(position)?;
        if self.plan.markdown_only && !is_markdown(&record.document_path) {
            return Some(false);
        }
        match self.plan.predicate.decide(
            Dialect::Dataview,
            &RecordValues {
                properties: &record.properties,
                path: &record.document_path,
                name: &record.file_name,
                ext: &record.file_ext,
            },
        ) {
            Decision::Match => Some(true),
            Decision::NoMatch => Some(false),
            // Properties that are not an object.
            Decision::Undecided => None,
        }
    }

    /// Decide every undecided source member, then intersect.
    fn finish(
        &self,
        records: &[Arc<NoteRecord>],
        source: Option<Vec<u64>>,
        mut decided: Vec<u64>,
        mut predicate: Vec<u64>,
        candidates: &mut dyn Iterator<Item = usize>,
    ) -> Option<MatchIndex> {
        for position in candidates {
            if bit(&decided, position) || source.as_ref().is_some_and(|bits| !bit(bits, position)) {
                continue;
            }
            set_bit(&mut decided, position, true);
            set_bit(&mut predicate, position, self.decide(records, position)?);
        }
        let matched = match &source {
            Some(source) => source
                .iter()
                .zip(&predicate)
                .map(|(source, predicate)| source & predicate)
                .collect(),
            None => predicate.clone(),
        };
        Some(MatchIndex {
            source: source.map(Arc::from),
            decided: Arc::from(decided),
            predicate: Arc::from(predicate),
            matched: Arc::from(matched),
            identities_changed: false,
        })
    }
}

impl RecordMatcher for PlanMatcher<'_, '_> {
    fn build(&self, records: &[Arc<NoteRecord>]) -> Option<MatchIndex> {
        let words = records.len().div_ceil(64);
        let source = match &self.plan.source {
            Some(source) => Some(self.source_bits(source, records.len())?),
            None => None,
        };
        self.finish(
            records,
            source,
            vec![0; words],
            vec![0; words],
            &mut (0..records.len()),
        )
    }

    fn update(
        &self,
        previous: &MatchIndex,
        records: &[Arc<NoteRecord>],
        changed: &[usize],
    ) -> Option<MatchIndex> {
        let mut decided = previous.decided.to_vec();
        let mut predicate = previous.predicate.to_vec();
        if decided.len() != records.len().div_ceil(64) {
            return None;
        }
        for &position in changed {
            set_bit(&mut decided, position, false);
            set_bit(&mut predicate, position, false);
        }
        match (&self.plan.source, &previous.source) {
            (None, _) => self.finish(
                records,
                None,
                decided,
                predicate,
                &mut changed.iter().copied(),
            ),
            // Only changed records can join or leave; they decide again.
            (Some(source), Some(members)) if per_document(source, previous.identities_changed) => {
                let members = self.changed_members(source, members, records, changed)?;
                self.finish(
                    records,
                    Some(members),
                    decided,
                    predicate,
                    &mut changed.iter().copied(),
                )
            }
            // Any record may join: decide every member not yet decided.
            (Some(source), _) => {
                let source = self.source_bits(source, records.len())?;
                self.finish(
                    records,
                    Some(source),
                    decided,
                    predicate,
                    &mut (0..records.len()),
                )
            }
        }
    }
}

/// The first `take` matches of a [`PlanTop`] in its order, then in path
/// order, with the records walked; `None` when the plan must decide every
/// candidate instead. The match set and order are cached with the records,
/// so walking an order correlated with the predicate reads no record it
/// skips. Index stages name how each index was obtained.
fn walk_top(
    paths: &VaultPaths,
    lookup: &IndexedNoteLookup<'_>,
    plan: &NotePlan<'_>,
    stage: &mut dyn FnMut(&str, Instant),
) -> Option<(usize, DecidedCandidates)> {
    let top = plan.top.as_ref()?;
    let records = lookup.all_stored()?;
    if !plan.predicate.is_total() {
        return None;
    }
    let key = format!(
        "{}:{:?}:{}",
        plan.markdown_only,
        plan.source,
        serde_json::to_string(&plan.predicate).ok()?
    );
    let started = Instant::now();
    let (matches, origin) = lookup.record_matches(
        &key,
        &PlanMatcher {
            paths,
            lookup,
            plan,
        },
    )?;
    stage(&format!("match index {}", origin.name()), started);
    let started = Instant::now();
    let order = match &top.order {
        Some(order) => {
            let (index, origin) = lookup.record_order(&order.key, order.ordering)?;
            stage(&format!("order index {}", origin.name()), started);
            Some(index.order)
        }
        None => None,
    };
    let positions: Box<dyn Iterator<Item = usize>> = match &order {
        Some(order) => Box::new(order.iter().map(|&position| position as usize)),
        None => Box::new(0..records.len()),
    };
    let mut decided = DecidedCandidates {
        rows: Vec::new(),
        undecided: HashSet::new(),
        matches: 0,
        excluded: 0,
    };
    let mut walked = 0;
    for position in positions {
        if decided.rows.len() >= top.take {
            break;
        }
        walked += 1;
        if bit(&matches.matched, position) {
            decided.matches += 1;
            decided
                .rows
                .push(records.get(position)?.document_path.clone());
        }
    }
    decided.excluded = walked - decided.matches;
    decided.rows.sort();
    Some((walked, decided))
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
        filter.note_query_scope_sql("_permission_documents", "note_query.document_id")
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
    fn link_sources_stay_per_document_only_while_identities_hold() {
        let links = SourceExpr::And(vec![
            SourceExpr::Tag("t".into()),
            SourceExpr::LinksTo("id".into()),
        ]);
        assert!(per_document(&links, false));
        assert!(!per_document(&links, true));
        assert!(per_document(&SourceExpr::Folder("f".into()), true));
        assert!(!per_document(
            &SourceExpr::Not(Box::new(SourceExpr::LinkedFrom("id".into()))),
            false
        ));
    }

    #[test]
    fn plans_select_decide_and_hydrate_what_they_name() {
        let (_temp, paths) = vault();
        let run = |plan: &NotePlan<'_>| {
            let lookup =
                load_indexed_note_lookup(&paths, NoteIndexReadScope::Filter(None)).unwrap();
            let planned = execute_note_plan(&paths, &lookup, plan).unwrap();
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
            top: None,
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
            top: None,
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
            top: None,
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
            top: None,
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
            top: None,
        });
        assert_eq!(planned.rows, ["B.md"]);
    }
}
