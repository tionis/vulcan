use crate::graph::{resolve_note_reference, GraphQueryError};
use crate::parser::parse_document;
use crate::refactor::{RefactorChange, RefactorFileReport, RefactorReport};
use crate::scan::{scan_vault_unlocked, ScanError, ScanMode};
use crate::write_lock::acquire_write_lock;
use crate::{
    load_vault_config, query_notes, CacheError, LinkConfidence, LinkResolutionMode,
    LinkStylePreference, NoteQuery, VaultPaths,
};
use aho_corasick::AhoCorasick;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::ops::Range;
use std::path::{Component, Path};
use ulid::Ulid;

#[derive(Debug)]
pub enum SuggestionError {
    Cache(CacheError),
    CacheMissing,
    Graph(GraphQueryError),
    InvalidRewrite(String),
    Io(std::io::Error),
    Scan(ScanError),
    Sqlite(rusqlite::Error),
}

impl Display for SuggestionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::CacheMissing => {
                formatter.write_str("cache is missing; run `vulcan scan` before using suggestions")
            }
            Self::Graph(error) => write!(formatter, "{error}"),
            Self::InvalidRewrite(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Scan(error) => write!(formatter, "{error}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for SuggestionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Graph(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Scan(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::CacheMissing | Self::InvalidRewrite(_) => None,
        }
    }
}

impl From<CacheError> for SuggestionError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<GraphQueryError> for SuggestionError {
    fn from(error: GraphQueryError) -> Self {
        Self::Graph(error)
    }
}

impl From<std::io::Error> for SuggestionError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ScanError> for SuggestionError {
    fn from(error: ScanError) -> Self {
        Self::Scan(error)
    }
}

impl From<rusqlite::Error> for SuggestionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MentionSuggestion {
    pub source_path: String,
    pub matched_text: String,
    pub target_path: Option<String>,
    pub candidate_paths: Vec<String>,
    pub line: usize,
    pub column: usize,
    pub context: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MentionSuggestionsReport {
    pub suggestions: Vec<MentionSuggestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DuplicateGroup {
    pub value: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MergeCandidate {
    pub left_path: String,
    pub right_path: String,
    pub score: f64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DuplicateSuggestionsReport {
    pub duplicate_titles: Vec<DuplicateGroup>,
    pub alias_collisions: Vec<DuplicateGroup>,
    pub merge_candidates: Vec<MergeCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MentionCandidate {
    name: String,
    paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NoteIdentity {
    id: String,
    path: String,
    filename: String,
    aliases: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkSuggestionStatus {
    Pending,
    Accepted,
    Rejected,
}

impl LinkSuggestionStatus {
    fn from_db(value: &str) -> Self {
        match value {
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            _ => Self::Pending,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct LinkSuggestionSignals {
    pub embedding_score: f64,
    pub graph_score: f64,
    pub mention_score: f64,
    pub tag_score: f64,
    pub cross_community_bonus: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkSuggestion {
    pub id: String,
    pub source_path: String,
    pub target_path: String,
    pub score: f64,
    pub display_score: f64,
    pub signals: LinkSuggestionSignals,
    pub status: LinkSuggestionStatus,
    pub created_at: String,
    pub accepted_at: Option<String>,
    pub rejected_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkSuggestionsReport {
    pub suggestions: Vec<LinkSuggestion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextEdit {
    start: usize,
    end: usize,
    replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FilePlan {
    path: String,
    updated_contents: String,
    changes: Vec<RefactorChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrontmatterBlock {
    full_start: usize,
    full_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BkTreeNode {
    term: String,
    children: HashMap<usize, BkTreeNode>,
}

impl BkTreeNode {
    fn new(term: String) -> Self {
        Self {
            term,
            children: HashMap::new(),
        }
    }

    fn insert(&mut self, term: String) {
        let distance = levenshtein(&self.term, &term);
        if distance == 0 {
            return;
        }
        if let Some(child) = self.children.get_mut(&distance) {
            child.insert(term);
            return;
        }
        self.children.insert(distance, Self::new(term));
    }

    fn search_within<'a>(
        &'a self,
        term: &str,
        max_distance: usize,
        matches: &mut Vec<(&'a str, usize)>,
    ) {
        let distance = levenshtein(&self.term, term);
        if distance <= max_distance {
            matches.push((self.term.as_str(), distance));
        }

        let min_distance = distance.saturating_sub(max_distance);
        let max_child_distance = distance.saturating_add(max_distance);
        for (child_distance, child) in &self.children {
            if (*child_distance >= min_distance) && (*child_distance <= max_child_distance) {
                child.search_within(term, max_distance, matches);
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BkTree {
    root: Option<BkTreeNode>,
}

impl BkTree {
    fn from_terms<'a>(terms: impl IntoIterator<Item = &'a str>) -> Self {
        let mut tree = Self::default();
        for term in terms {
            tree.insert(term.to_string());
        }
        tree
    }

    fn insert(&mut self, term: String) {
        if let Some(root) = &mut self.root {
            root.insert(term);
            return;
        }
        self.root = Some(BkTreeNode::new(term));
    }

    fn search_within(&self, term: &str, max_distance: usize) -> Vec<(&str, usize)> {
        let mut matches = Vec::new();
        if let Some(root) = &self.root {
            root.search_within(term, max_distance, &mut matches);
        }
        matches
    }
}

pub fn suggest_mentions(
    paths: &VaultPaths,
    note_identifier: Option<&str>,
) -> Result<MentionSuggestionsReport, SuggestionError> {
    let config = load_vault_config(paths).config;
    let connection = open_existing_cache(paths)?;
    let notes = load_note_identities(&connection)?;
    let candidates = build_mention_candidates(&notes);
    let selected_paths = selected_note_paths(paths, &notes, note_identifier)?;
    let automaton = AhoCorasick::builder()
        .build(candidates.iter().map(|c| c.name.as_bytes()))
        .expect("aho-corasick should build from note names");
    let mut suggestions = Vec::new();

    for path in selected_paths {
        let source = fs::read_to_string(paths.vault_root().join(&path))?;
        let parsed = parse_document(&source, &config);
        let blocked = blocked_ranges(&source, &parsed);
        suggestions.extend(find_note_mentions(
            &path,
            &source,
            &candidates,
            &blocked,
            &automaton,
        ));
    }

    suggestions.sort_by(|left, right| {
        left.source_path
            .cmp(&right.source_path)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
            .then(left.matched_text.cmp(&right.matched_text))
    });
    Ok(MentionSuggestionsReport { suggestions })
}

pub fn suggest_links(
    paths: &VaultPaths,
    note_identifier: Option<&str>,
    limit: Option<usize>,
    min_score: f64,
    status: Option<LinkSuggestionStatus>,
) -> Result<LinkSuggestionsReport, SuggestionError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_note_identities(&connection)?;
    compute_and_persist_link_suggestions(paths, &connection, note_identifier, min_score)?;
    load_link_suggestions(
        &connection,
        &notes,
        note_identifier,
        limit,
        min_score,
        status,
    )
}

pub fn accept_link_suggestion(
    paths: &VaultPaths,
    id: &str,
) -> Result<LinkSuggestion, SuggestionError> {
    update_link_suggestion_status(paths, id, LinkSuggestionStatus::Accepted)
}

pub fn reject_link_suggestion(
    paths: &VaultPaths,
    id: &str,
) -> Result<LinkSuggestion, SuggestionError> {
    update_link_suggestion_status(paths, id, LinkSuggestionStatus::Rejected)
}

fn compute_and_persist_link_suggestions(
    paths: &VaultPaths,
    connection: &Connection,
    note_identifier: Option<&str>,
    min_score: f64,
) -> Result<(), SuggestionError> {
    let notes = load_note_identities(connection)?;
    let note_by_path = notes
        .iter()
        .map(|note| (note.path.clone(), note))
        .collect::<HashMap<_, _>>();
    let selected_paths = selected_note_paths(paths, &notes, note_identifier)?;
    let mention_pairs = suggest_mentions(paths, note_identifier)?
        .suggestions
        .into_iter()
        .filter_map(|suggestion| {
            suggestion
                .target_path
                .map(|target| ((suggestion.source_path, target), 0.2))
        })
        .collect::<HashMap<_, _>>();
    let tag_map = load_tag_map(connection)?;
    let existing_edges = load_existing_link_pairs(connection)?;
    let graph_distances = graph_proximity_candidates(connection)?;
    let communities = load_graph_community_map(connection)?;

    for source_path in selected_paths {
        let Some(source) = note_by_path.get(&source_path) else {
            continue;
        };
        for target in &notes {
            if source.id == target.id
                || existing_edges.contains(&(source.id.clone(), target.id.clone()))
            {
                continue;
            }
            let mention_score = mention_pairs
                .get(&(source.path.clone(), target.path.clone()))
                .copied()
                .unwrap_or(0.0);
            let graph_score = graph_distances
                .get(&(source.id.clone(), target.id.clone()))
                .map_or(0.0, |distance| 0.3 / f64::from(*distance));
            let empty_tags = BTreeSet::new();
            let tag_score = jaccard_tags(
                tag_map.get(&source.id).unwrap_or(&empty_tags),
                tag_map.get(&target.id).unwrap_or(&empty_tags),
            ) * 0.1;
            let cross_community_bonus =
                match (communities.get(&source.id), communities.get(&target.id)) {
                    (Some(left), Some(right)) if left != right => 1.15,
                    _ => 1.0,
                };
            let signals = LinkSuggestionSignals {
                embedding_score: 0.0,
                graph_score,
                mention_score,
                tag_score,
                cross_community_bonus,
            };
            let base_score = signals.embedding_score
                + signals.graph_score
                + signals.mention_score
                + signals.tag_score;
            let mut score = base_score * cross_community_bonus;
            if let Some(existing_status) =
                existing_suggestion_status(connection, &source.id, &target.id)?
            {
                if existing_status == LinkSuggestionStatus::Rejected {
                    score *= 0.5;
                }
            }
            if score < min_score || score <= 0.0 {
                continue;
            }
            let signals_json = serde_json::to_string(&signals)
                .map_err(|error| SuggestionError::InvalidRewrite(error.to_string()))?;
            connection.execute(
                "
                INSERT INTO link_suggestions (
                    id, source_document_id, target_document_id, score, signals, status, created_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, 'pending', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                ON CONFLICT(source_document_id, target_document_id) DO UPDATE SET
                    score = excluded.score,
                    signals = excluded.signals
                WHERE link_suggestions.status = 'pending'
                ",
                params![
                    Ulid::new().to_string(),
                    &source.id,
                    &target.id,
                    score,
                    signals_json,
                ],
            )?;
        }
    }
    Ok(())
}

fn update_link_suggestion_status(
    paths: &VaultPaths,
    id: &str,
    status: LinkSuggestionStatus,
) -> Result<LinkSuggestion, SuggestionError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_note_identities(&connection)?;
    let (source_id, target_id, score) = connection.query_row(
        "SELECT source_document_id, target_document_id, score FROM link_suggestions WHERE id = ?1",
        params![id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        },
    )?;
    match status {
        LinkSuggestionStatus::Accepted => {
            connection.execute(
                "
                UPDATE link_suggestions
                SET status = 'accepted', accepted_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                WHERE id = ?1
                ",
                params![id],
            )?;
            insert_inferred_link(&connection, &source_id, &target_id, score)?;
        }
        LinkSuggestionStatus::Rejected => {
            connection.execute(
                "
                UPDATE link_suggestions
                SET status = 'rejected', rejected_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                WHERE id = ?1
                ",
                params![id],
            )?;
        }
        LinkSuggestionStatus::Pending => {}
    }
    load_link_suggestions(&connection, &notes, None, None, 0.0, None)?
        .suggestions
        .into_iter()
        .find(|suggestion| suggestion.id == id)
        .ok_or_else(|| SuggestionError::InvalidRewrite(format!("suggestion not found: {id}")))
}

fn insert_inferred_link(
    connection: &Connection,
    source_id: &str,
    target_id: &str,
    score: f64,
) -> Result<(), SuggestionError> {
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

fn load_link_suggestions(
    connection: &Connection,
    notes: &[NoteIdentity],
    note_identifier: Option<&str>,
    limit: Option<usize>,
    min_score: f64,
    status: Option<LinkSuggestionStatus>,
) -> Result<LinkSuggestionsReport, SuggestionError> {
    let id_to_path = notes
        .iter()
        .map(|note| (note.id.clone(), note.path.clone()))
        .collect::<HashMap<_, _>>();
    let selected_ids = if let Some(identifier) = note_identifier {
        selected_note_paths_from_loaded(notes, identifier)?
            .into_iter()
            .filter_map(|path| {
                notes
                    .iter()
                    .find(|note| note.path == path)
                    .map(|note| note.id.clone())
            })
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let mut statement = connection.prepare(
        "
        SELECT id, source_document_id, target_document_id, score, signals, status, created_at, accepted_at, rejected_at
        FROM link_suggestions
        ORDER BY score DESC, created_at ASC, id ASC
        ",
    )?;
    let rows = statement.query_map([], |row| {
        let signals_json: String = row.get(4)?;
        let signals = serde_json::from_str::<LinkSuggestionSignals>(&signals_json).unwrap_or(
            LinkSuggestionSignals {
                embedding_score: 0.0,
                graph_score: 0.0,
                mention_score: 0.0,
                tag_score: 0.0,
                cross_community_bonus: 1.0,
            },
        );
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, f64>(3)?,
            signals,
            LinkSuggestionStatus::from_db(&row.get::<_, String>(5)?),
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    })?;
    let mut suggestions = Vec::new();
    for row in rows {
        let (
            id,
            source_id,
            target_id,
            score,
            signals,
            row_status,
            created_at,
            accepted_at,
            rejected_at,
        ) = row?;
        if score < min_score {
            continue;
        }
        if status.as_ref().is_some_and(|status| status != &row_status) {
            continue;
        }
        if !selected_ids.is_empty() && !selected_ids.contains(&source_id) {
            continue;
        }
        let Some(source_path) = id_to_path.get(&source_id).cloned() else {
            continue;
        };
        let Some(target_path) = id_to_path.get(&target_id).cloned() else {
            continue;
        };
        suggestions.push(LinkSuggestion {
            id,
            source_path,
            target_path,
            score,
            display_score: score.min(1.0),
            signals,
            status: row_status,
            created_at,
            accepted_at,
            rejected_at,
        });
        if limit.is_some_and(|limit| suggestions.len() >= limit) {
            break;
        }
    }
    Ok(LinkSuggestionsReport { suggestions })
}

pub fn link_mentions(
    paths: &VaultPaths,
    note_identifier: Option<&str>,
    dry_run: bool,
) -> Result<RefactorReport, SuggestionError> {
    let _lock = acquire_write_lock(paths)?;
    let config = load_vault_config(paths).config;
    let connection = open_existing_cache(paths)?;
    let notes = load_note_identities(&connection)?;
    let all_note_paths = notes
        .iter()
        .map(|note| note.path.clone())
        .collect::<Vec<_>>();
    let suggestions = suggest_mentions(paths, note_identifier)?.suggestions;
    let mut suggestions_by_file = BTreeMap::<String, Vec<MentionSuggestion>>::new();
    for suggestion in suggestions
        .into_iter()
        .filter(|suggestion| suggestion.target_path.is_some())
    {
        suggestions_by_file
            .entry(suggestion.source_path.clone())
            .or_default()
            .push(suggestion);
    }

    let mut plans = Vec::new();
    for (path, file_suggestions) in suggestions_by_file {
        let source = fs::read_to_string(paths.vault_root().join(&path))?;
        let mut occupied = Vec::<Range<usize>>::new();
        let mut edits = Vec::new();
        let mut changes = Vec::new();

        for suggestion in file_suggestions {
            let Some(target_path) = suggestion.target_path.as_deref() else {
                continue;
            };
            if let Some((start, end)) = locate_unique_occurrence(
                &source,
                &suggestion.matched_text,
                suggestion.line,
                suggestion.column,
            ) {
                if ranges_intersect(&occupied, start, end) {
                    continue;
                }
                let replacement = render_link_for_mention(
                    &path,
                    target_path,
                    &suggestion.matched_text,
                    &all_note_paths,
                    config.link_resolution,
                    config.link_style,
                );
                edits.push(TextEdit {
                    start,
                    end,
                    replacement: replacement.clone(),
                });
                changes.push(RefactorChange {
                    before: suggestion.matched_text,
                    after: replacement,
                });
                occupied.push(start..end);
            }
        }

        if let Some(plan) = build_file_plan(&path, &source, &edits, changes) {
            plans.push(plan);
        }
    }

    finalize_refactor(paths, dry_run, "link_mentions", plans)
}

pub fn suggest_duplicates(
    paths: &VaultPaths,
) -> Result<DuplicateSuggestionsReport, SuggestionError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_note_identities(&connection)?;
    let duplicate_titles = duplicate_title_groups(&notes);
    let alias_collisions = alias_collision_groups(&notes);
    let merge_candidates = merge_candidates(&notes, &duplicate_titles, &alias_collisions);

    Ok(DuplicateSuggestionsReport {
        duplicate_titles,
        alias_collisions,
        merge_candidates,
    })
}

pub fn bulk_replace(
    paths: &VaultPaths,
    filters: &[String],
    find: &str,
    replace: &str,
    dry_run: bool,
) -> Result<RefactorReport, SuggestionError> {
    if find.is_empty() {
        return Err(SuggestionError::InvalidRewrite(
            "`rewrite --find` must not be empty".to_string(),
        ));
    }

    let _lock = acquire_write_lock(paths)?;
    let notes = query_notes(
        paths,
        &NoteQuery {
            filters: filters.to_vec(),
            sort_by: None,
            sort_descending: false,
        },
    )
    .map_err(|error| SuggestionError::InvalidRewrite(error.to_string()))?;
    let note_paths = notes
        .notes
        .into_iter()
        .map(|note| note.document_path)
        .collect::<Vec<_>>();
    let plans = bulk_replace_plans(paths, &note_paths, find, replace)?;

    finalize_refactor(paths, dry_run, "bulk_replace", plans)
}

pub fn bulk_replace_on_paths(
    paths: &VaultPaths,
    note_paths: &[String],
    find: &str,
    replace: &str,
    dry_run: bool,
) -> Result<RefactorReport, SuggestionError> {
    if find.is_empty() {
        return Err(SuggestionError::InvalidRewrite(
            "`rewrite --find` must not be empty".to_string(),
        ));
    }

    let _lock = acquire_write_lock(paths)?;
    let plans = bulk_replace_plans(paths, note_paths, find, replace)?;
    finalize_refactor(paths, dry_run, "bulk_replace", plans)
}

fn bulk_replace_plans(
    paths: &VaultPaths,
    note_paths: &[String],
    find: &str,
    replace: &str,
) -> Result<Vec<FilePlan>, SuggestionError> {
    let mut plans = Vec::new();

    for path in note_paths {
        let source = fs::read_to_string(paths.vault_root().join(path))?;
        let mut edits = Vec::new();
        let mut changes = Vec::new();

        for (start, matched) in source.match_indices(find) {
            let end = start + matched.len();
            edits.push(TextEdit {
                start,
                end,
                replacement: replace.to_string(),
            });
            changes.push(RefactorChange {
                before: matched.to_string(),
                after: replace.to_string(),
            });
        }

        if let Some(plan) = build_file_plan(path, &source, &edits, changes) {
            plans.push(plan);
        }
    }

    Ok(plans)
}

fn open_existing_cache(paths: &VaultPaths) -> Result<Connection, SuggestionError> {
    if !paths.cache_db().exists() {
        return Err(SuggestionError::CacheMissing);
    }
    Ok(Connection::open(paths.cache_db())?)
}

fn load_note_identities(connection: &Connection) -> Result<Vec<NoteIdentity>, SuggestionError> {
    let mut aliases_by_document = HashMap::<String, Vec<String>>::new();
    let mut alias_statement =
        connection.prepare("SELECT document_id, alias_text FROM aliases ORDER BY alias_text")?;
    let alias_rows = alias_statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in alias_rows {
        let (document_id, alias_text) = row?;
        aliases_by_document
            .entry(document_id)
            .or_default()
            .push(alias_text);
    }

    let mut statement = connection.prepare(
        "
        SELECT id, path, filename
        FROM documents
        WHERE extension = 'md'
        ORDER BY path
        ",
    )?;
    let rows = statement.query_map([], |row| {
        let document_id = row.get::<_, String>(0)?;
        Ok(NoteIdentity {
            id: document_id.clone(),
            path: row.get(1)?,
            filename: row.get(2)?,
            aliases: aliases_by_document.remove(&document_id).unwrap_or_default(),
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SuggestionError::from)
}

fn selected_note_paths_from_loaded(
    notes: &[NoteIdentity],
    note_identifier: &str,
) -> Result<Vec<String>, SuggestionError> {
    let lower = note_identifier.to_ascii_lowercase();
    let matches = notes
        .iter()
        .filter(|note| {
            note.path.eq_ignore_ascii_case(&lower)
                || note.filename.eq_ignore_ascii_case(&lower)
                || note
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(&lower))
        })
        .map(|note| note.path.clone())
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(GraphQueryError::NoteNotFound {
            identifier: note_identifier.to_string(),
        }
        .into());
    }
    Ok(matches)
}

fn load_tag_map(
    connection: &Connection,
) -> Result<HashMap<String, BTreeSet<String>>, SuggestionError> {
    let mut map = HashMap::<String, BTreeSet<String>>::new();
    let mut statement = connection
        .prepare("SELECT document_id, tag_text FROM tags ORDER BY document_id, tag_text")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (document_id, tag) = row?;
        map.entry(document_id).or_default().insert(tag);
    }
    Ok(map)
}

fn load_existing_link_pairs(
    connection: &Connection,
) -> Result<BTreeSet<(String, String)>, SuggestionError> {
    let mut statement = connection.prepare(
        "SELECT source_document_id, resolved_target_id FROM links WHERE resolved_target_id IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<BTreeSet<_>, _>>()
        .map_err(SuggestionError::from)
}

fn graph_proximity_candidates(
    connection: &Connection,
) -> Result<HashMap<(String, String), u32>, SuggestionError> {
    let existing = load_existing_link_pairs(connection)?;
    let mut undirected = HashMap::<String, BTreeSet<String>>::new();
    for (source_id, target_id) in &existing {
        undirected
            .entry(source_id.clone())
            .or_default()
            .insert(target_id.clone());
        undirected
            .entry(target_id.clone())
            .or_default()
            .insert(source_id.clone());
    }
    let mut candidates = HashMap::new();
    for source in undirected.keys() {
        let mut queue = std::collections::VecDeque::from([(source.clone(), 0u32)]);
        let mut seen = BTreeSet::from([source.clone()]);
        while let Some((current, distance)) = queue.pop_front() {
            if distance >= 3 {
                continue;
            }
            for neighbor in undirected.get(&current).into_iter().flatten() {
                if !seen.insert(neighbor.clone()) {
                    continue;
                }
                let next_distance = distance + 1;
                if next_distance > 1
                    && !existing.contains(&(source.clone(), neighbor.clone()))
                    && source != neighbor
                {
                    candidates.insert((source.clone(), neighbor.clone()), next_distance);
                }
                queue.push_back((neighbor.clone(), next_distance));
            }
        }
    }
    Ok(candidates)
}

fn load_graph_community_map(
    connection: &Connection,
) -> Result<HashMap<String, i64>, SuggestionError> {
    let mut map = HashMap::new();
    let mut statement =
        connection.prepare("SELECT document_id, community_id FROM graph_clusters")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (document_id, community_id) = row?;
        map.insert(document_id, community_id);
    }
    Ok(map)
}

fn existing_suggestion_status(
    connection: &Connection,
    source_id: &str,
    target_id: &str,
) -> Result<Option<LinkSuggestionStatus>, SuggestionError> {
    let mut statement = connection.prepare(
        "SELECT status FROM link_suggestions WHERE source_document_id = ?1 AND target_document_id = ?2",
    )?;
    let mut rows = statement.query(params![source_id, target_id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(LinkSuggestionStatus::from_db(
            &row.get::<_, String>(0)?,
        )))
    } else {
        Ok(None)
    }
}

fn jaccard_tags(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count();
    let union = left.union(right).count();
    if union == 0 {
        0.0
    } else {
        f64::from(u32::try_from(intersection).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(union).unwrap_or(u32::MAX))
    }
}

fn build_mention_candidates(notes: &[NoteIdentity]) -> Vec<MentionCandidate> {
    let mut by_name = BTreeMap::<String, BTreeSet<String>>::new();
    for note in notes {
        by_name
            .entry(note.filename.clone())
            .or_default()
            .insert(note.path.clone());
        for alias in &note.aliases {
            if alias.chars().count() >= 3 {
                by_name
                    .entry(alias.clone())
                    .or_default()
                    .insert(note.path.clone());
            }
        }
    }

    let mut candidates = by_name
        .into_iter()
        .filter(|(name, _)| name.chars().count() >= 3)
        .map(|(name, paths)| MentionCandidate {
            name,
            paths: paths.into_iter().collect(),
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .name
            .chars()
            .count()
            .cmp(&left.name.chars().count())
            .then(left.name.cmp(&right.name))
    });
    candidates
}

fn selected_note_paths(
    paths: &VaultPaths,
    notes: &[NoteIdentity],
    note_identifier: Option<&str>,
) -> Result<Vec<String>, SuggestionError> {
    if let Some(identifier) = note_identifier {
        return Ok(vec![resolve_note_reference(paths, identifier)?.path]);
    }

    Ok(notes.iter().map(|note| note.path.clone()).collect())
}

fn blocked_ranges(source: &str, parsed: &crate::ParsedDocument) -> Vec<Range<usize>> {
    let mut blocked = parsed
        .links
        .iter()
        .map(|link| link.byte_offset..(link.byte_offset + link.raw_text.len()))
        .collect::<Vec<_>>();
    if let Some(frontmatter) = find_frontmatter_block(source) {
        blocked.push(frontmatter.full_start..frontmatter.full_end);
    }
    blocked
}

fn find_note_mentions(
    source_path: &str,
    source: &str,
    candidates: &[MentionCandidate],
    blocked: &[Range<usize>],
    automaton: &AhoCorasick,
) -> Vec<MentionSuggestion> {
    let mut suggestions = Vec::new();
    let mut occupied = Vec::<Range<usize>>::new();

    for mat in automaton.find_overlapping_iter(source) {
        let start = mat.start();
        let end = mat.end();
        let candidate = &candidates[mat.pattern().as_usize()];
        if ranges_intersect(blocked, start, end)
            || ranges_intersect(&occupied, start, end)
            || !is_word_boundary(source, start, end)
        {
            continue;
        }
        let candidate_paths = candidate
            .paths
            .iter()
            .filter(|path| path.as_str() != source_path)
            .cloned()
            .collect::<Vec<_>>();
        if candidate_paths.is_empty() {
            continue;
        }
        let matched_text = &source[start..end];
        let (line, column, context) = line_column_context(source, start);
        suggestions.push(MentionSuggestion {
            source_path: source_path.to_string(),
            matched_text: matched_text.to_string(),
            target_path: (candidate_paths.len() == 1).then(|| candidate_paths[0].clone()),
            candidate_paths,
            line,
            column,
            context,
        });
        occupied.push(start..end);
    }

    suggestions
}

fn duplicate_title_groups(notes: &[NoteIdentity]) -> Vec<DuplicateGroup> {
    let mut groups = BTreeMap::<String, BTreeSet<String>>::new();
    for note in notes {
        groups
            .entry(note.filename.clone())
            .or_default()
            .insert(note.path.clone());
    }
    groups
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(value, paths)| DuplicateGroup {
            value,
            paths: paths.into_iter().collect(),
        })
        .collect()
}

fn alias_collision_groups(notes: &[NoteIdentity]) -> Vec<DuplicateGroup> {
    let mut groups = BTreeMap::<String, BTreeSet<String>>::new();
    for note in notes {
        for alias in &note.aliases {
            groups
                .entry(alias.clone())
                .or_default()
                .insert(note.path.clone());
        }
    }
    groups
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(value, paths)| DuplicateGroup {
            value,
            paths: paths.into_iter().collect(),
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn merge_candidates(
    notes: &[NoteIdentity],
    duplicate_titles: &[DuplicateGroup],
    alias_collisions: &[DuplicateGroup],
) -> Vec<MergeCandidate> {
    let mut candidates = BTreeMap::<(String, String), MergeCandidate>::new();

    for group in duplicate_titles {
        for pair in pair_paths(&group.paths) {
            candidates
                .entry(pair.clone())
                .and_modify(|candidate| {
                    candidate.score = candidate.score.max(1.0);
                    candidate
                        .reasons
                        .push(format!("same title `{}`", group.value));
                })
                .or_insert_with(|| MergeCandidate {
                    left_path: pair.0.clone(),
                    right_path: pair.1.clone(),
                    score: 1.0,
                    reasons: vec![format!("same title `{}`", group.value)],
                });
        }
    }

    for group in alias_collisions {
        for pair in pair_paths(&group.paths) {
            candidates
                .entry(pair.clone())
                .and_modify(|candidate| {
                    candidate.score = candidate.score.max(0.95);
                    candidate
                        .reasons
                        .push(format!("shared alias `{}`", group.value));
                })
                .or_insert_with(|| MergeCandidate {
                    left_path: pair.0.clone(),
                    right_path: pair.1.clone(),
                    score: 0.95,
                    reasons: vec![format!("shared alias `{}`", group.value)],
                });
        }
    }

    // Pre-compute lowercased filenames to avoid re-allocating in the inner loop
    let lowercased: Vec<String> = notes
        .iter()
        .map(|n| n.filename.to_ascii_lowercase())
        .collect();
    let mut name_to_indices = BTreeMap::<String, Vec<usize>>::new();
    for (index, filename) in lowercased.iter().enumerate() {
        name_to_indices
            .entry(filename.clone())
            .or_default()
            .push(index);
    }
    let mut length_buckets = BTreeMap::<usize, Vec<String>>::new();
    for name in name_to_indices.keys() {
        length_buckets
            .entry(name.len())
            .or_default()
            .push(name.clone());
    }
    let bucket_trees = length_buckets
        .iter()
        .map(|(length, names)| {
            (
                *length,
                BkTree::from_terms(names.iter().map(std::string::String::as_str)),
            )
        })
        .collect::<BTreeMap<_, _>>();

    for (left_name, left_indices) in &name_to_indices {
        for bucket_length in [
            left_name.len().saturating_sub(1),
            left_name.len(),
            left_name.len().saturating_add(1),
        ] {
            let Some(tree) = bucket_trees.get(&bucket_length) else {
                continue;
            };
            for (right_name, distance) in tree.search_within(left_name, 1) {
                if (distance > 1) || (right_name <= left_name.as_str()) {
                    continue;
                }
                let Some(right_indices) = name_to_indices.get(right_name) else {
                    continue;
                };
                for &left_index in left_indices {
                    let left = &notes[left_index];
                    for &right_index in right_indices {
                        let right = &notes[right_index];
                        if left.filename.eq_ignore_ascii_case(&right.filename) {
                            continue;
                        }
                        let pair = ordered_pair(&left.path, &right.path);
                        candidates
                            .entry(pair.clone())
                            .and_modify(|candidate| {
                                candidate.score = candidate.score.max(0.8);
                                candidate.reasons.push("similar title".to_string());
                            })
                            .or_insert_with(|| MergeCandidate {
                                left_path: pair.0.clone(),
                                right_path: pair.1.clone(),
                                score: 0.8,
                                reasons: vec!["similar title".to_string()],
                            });
                    }
                }
            }
        }
    }

    let mut merged = candidates.into_values().collect::<Vec<_>>();
    merged.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.left_path.cmp(&right.left_path))
            .then(left.right_path.cmp(&right.right_path))
    });
    merged
}

fn pair_paths(paths: &[String]) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (left_index, left) in paths.iter().enumerate() {
        for right in paths.iter().skip(left_index + 1) {
            pairs.push(ordered_pair(left, right));
        }
    }
    pairs
}

fn ordered_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_string(), right.to_string())
    } else {
        (right.to_string(), left.to_string())
    }
}

fn find_frontmatter_block(source: &str) -> Option<FrontmatterBlock> {
    let mut lines = source.split_inclusive('\n');
    let first_line = lines.next()?;
    if trim_line(first_line) != "---" {
        return None;
    }

    let mut offset = first_line.len();
    for line in lines {
        if trim_line(line) == "---" {
            return Some(FrontmatterBlock {
                full_start: 0,
                full_end: offset + line.len(),
            });
        }
        offset += line.len();
    }

    None
}

fn trim_line(line: &str) -> &str {
    line.trim_end_matches('\n').trim_end_matches('\r')
}

fn ranges_intersect(ranges: &[Range<usize>], start: usize, end: usize) -> bool {
    ranges
        .iter()
        .any(|range| start < range.end && end > range.start)
}

fn is_word_boundary(source: &str, start: usize, end: usize) -> bool {
    let previous = source[..start].chars().next_back();
    let next = source[end..].chars().next();
    !previous.is_some_and(is_word_character) && !next.is_some_and(is_word_character)
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-')
}

fn line_column_context(source: &str, byte_offset: usize) -> (usize, usize, String) {
    let line_start = source[..byte_offset]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line_end = source[byte_offset..]
        .find('\n')
        .map_or(source.len(), |index| byte_offset + index);
    let line = source[..byte_offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let column = source[line_start..byte_offset].chars().count() + 1;
    (
        line,
        column,
        source[line_start..line_end]
            .trim_end_matches('\r')
            .to_string(),
    )
}

fn locate_unique_occurrence(
    source: &str,
    matched_text: &str,
    line: usize,
    column: usize,
) -> Option<(usize, usize)> {
    source
        .match_indices(matched_text)
        .find_map(|(start, matched)| {
            let (candidate_line, candidate_column, _) = line_column_context(source, start);
            (candidate_line == line && candidate_column == column)
                .then_some((start, start + matched.len()))
        })
}

fn render_link_for_mention(
    source_path: &str,
    target_path: &str,
    matched_text: &str,
    document_paths: &[String],
    resolution_mode: LinkResolutionMode,
    preferred_style: LinkStylePreference,
) -> String {
    let rendered_target = match resolution_mode {
        LinkResolutionMode::Absolute => format_target_path(target_path, preferred_style),
        LinkResolutionMode::Relative => format_target_path(
            &relative_path_from_source(source_path, target_path),
            preferred_style,
        ),
        LinkResolutionMode::Shortest => {
            shortest_unique_path(target_path, document_paths, preferred_style)
        }
    };

    match preferred_style {
        LinkStylePreference::Wikilink => format!("[[{rendered_target}]]"),
        LinkStylePreference::Markdown => format!("[{matched_text}]({rendered_target})"),
    }
}

fn format_target_path(path: &str, style: LinkStylePreference) -> String {
    match style {
        LinkStylePreference::Markdown => {
            if Path::new(path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                path.to_string()
            } else {
                format!("{path}.md")
            }
        }
        LinkStylePreference::Wikilink => path.strip_suffix(".md").unwrap_or(path).to_string(),
    }
}

fn relative_path_from_source(source_path: &str, destination_path: &str) -> String {
    let source_dir = Path::new(source_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let source_parts = source_dir
        .components()
        .filter_map(component_to_string)
        .collect::<Vec<_>>();
    let destination_parts = Path::new(destination_path)
        .components()
        .filter_map(component_to_string)
        .collect::<Vec<_>>();
    let shared = source_parts
        .iter()
        .zip(destination_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = Vec::new();
    for _ in 0..(source_parts.len() - shared) {
        parts.push("..".to_string());
    }
    parts.extend(destination_parts.into_iter().skip(shared));
    parts.join("/")
}

fn component_to_string(component: Component<'_>) -> Option<String> {
    match component {
        Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
        Component::CurDir | Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
            None
        }
    }
}

fn shortest_unique_path(
    destination_path: &str,
    document_paths: &[String],
    style: LinkStylePreference,
) -> String {
    let destination = path_identity(destination_path, style);
    let destination_parts = destination.split('/').collect::<Vec<_>>();
    for suffix_len in 1..=destination_parts.len() {
        let candidate_parts = &destination_parts[destination_parts.len() - suffix_len..];
        let matches = document_paths
            .iter()
            .filter(|path| path_suffix_matches(path, candidate_parts, style))
            .count();
        if matches == 1 {
            return format_target_path(&candidate_parts.join("/"), style);
        }
    }

    format_target_path(destination_path, style)
}

fn path_identity(path: &str, style: LinkStylePreference) -> &str {
    match style {
        LinkStylePreference::Markdown => path,
        LinkStylePreference::Wikilink => path.strip_suffix(".md").unwrap_or(path),
    }
}

fn path_suffix_matches(path: &str, candidate_parts: &[&str], style: LinkStylePreference) -> bool {
    let identity = path_identity(path, style);
    let parts = identity.split('/').collect::<Vec<_>>();
    parts.ends_with(candidate_parts)
}

fn build_file_plan(
    path: &str,
    source: &str,
    edits: &[TextEdit],
    changes: Vec<RefactorChange>,
) -> Option<FilePlan> {
    if edits.is_empty() || changes.is_empty() {
        return None;
    }
    let updated_contents = apply_edits(source, edits);
    if updated_contents == source {
        return None;
    }
    Some(FilePlan {
        path: path.to_string(),
        updated_contents,
        changes,
    })
}

fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
    let mut updated = source.to_string();
    let mut sorted = edits.to_vec();
    sorted.sort_by_key(|edit| std::cmp::Reverse(edit.start));
    for edit in sorted {
        updated.replace_range(edit.start..edit.end, &edit.replacement);
    }
    updated
}

fn finalize_refactor(
    paths: &VaultPaths,
    dry_run: bool,
    action: &str,
    plans: Vec<FilePlan>,
) -> Result<RefactorReport, SuggestionError> {
    if !dry_run {
        for plan in &plans {
            fs::write(paths.vault_root().join(&plan.path), &plan.updated_contents)?;
        }
        if !plans.is_empty() {
            scan_vault_unlocked(paths, ScanMode::Incremental)?;
        }
    }

    Ok(RefactorReport {
        dry_run,
        action: action.to_string(),
        files: plans
            .into_iter()
            .map(|plan| RefactorFileReport {
                path: plan.path,
                changes: plan.changes,
            })
            .collect(),
    })
}

fn levenshtein(left: &str, right: &str) -> usize {
    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut current = vec![0_usize; right_chars.len() + 1];

    for (left_index, left_char) in left_chars.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let substitution_cost = usize::from(left_char != right_char);
            current[right_index + 1] = (current[right_index] + 1)
                .min(previous[right_index + 1] + 1)
                .min(previous[right_index] + substitution_cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan_vault, ScanMode};
    use std::path::Path;
    use std::time::Instant;
    use tempfile::TempDir;

    #[test]
    fn suggest_mentions_reports_unambiguous_and_ambiguous_candidates() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("suggestions", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = suggest_mentions(&paths, Some("Home")).expect("suggestions should succeed");

        assert!(report
            .suggestions
            .iter()
            .any(|suggestion| suggestion.matched_text == "Bob"
                && suggestion.target_path.as_deref() == Some("People/Bob.md")));
        assert!(report
            .suggestions
            .iter()
            .any(|suggestion| suggestion.matched_text == "Alpha"
                && suggestion.target_path.is_none()
                && suggestion.candidate_paths.len() == 2));
    }

    #[test]
    fn link_mentions_dry_run_preserves_files_and_apply_links_unambiguous_mentions() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("suggestions", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let dry_run = link_mentions(&paths, Some("Home"), true).expect("dry run should succeed");
        assert_eq!(dry_run.files.len(), 1);
        assert_eq!(
            fs::read_to_string(vault_root.join("Home.md")).expect("home note should read"),
            "# Home\n\nBob should be linked.\nAlpha should stay as a suggestion because there are two Alpha notes.\nGuide is also mentioned.\n"
        );

        let applied = link_mentions(&paths, Some("Home"), false).expect("apply should succeed");
        assert_eq!(applied.files.len(), 1);
        let updated =
            fs::read_to_string(vault_root.join("Home.md")).expect("home note should read");
        assert!(updated.contains("[[Bob]]"));
        assert!(updated.contains("Alpha should stay as a suggestion"));
    }

    #[test]
    fn suggest_links_accepts_and_rejects_persisted_suggestions() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::write(vault_root.join("A.md"), "# A\n\n[[B]]\n").expect("write A");
        fs::write(vault_root.join("B.md"), "# B\n\n[[Charlie]]\n").expect("write B");
        fs::write(vault_root.join("Charlie.md"), "# Charlie\n").expect("write Charlie");
        fs::write(
            vault_root.join("D.md"),
            "# D\n\nCharlie is mentioned here.\n",
        )
        .expect("write D");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report =
            suggest_links(&paths, None, None, 0.0, None).expect("link suggestions should compute");

        let graph_suggestion = report
            .suggestions
            .iter()
            .find(|suggestion| {
                suggestion.source_path == "A.md" && suggestion.target_path == "Charlie.md"
            })
            .expect("A -> Charlie should be suggested through graph proximity");
        assert!(graph_suggestion.signals.graph_score > 0.0);
        let mention_suggestion = report
            .suggestions
            .iter()
            .find(|suggestion| {
                suggestion.source_path == "D.md" && suggestion.target_path == "Charlie.md"
            })
            .expect("D -> Charlie should be suggested through mention text");
        assert!(mention_suggestion.signals.mention_score > 0.0);

        let accepted =
            accept_link_suggestion(&paths, &graph_suggestion.id).expect("accept should succeed");
        assert_eq!(accepted.status, LinkSuggestionStatus::Accepted);
        let rejected =
            reject_link_suggestion(&paths, &mention_suggestion.id).expect("reject should succeed");
        assert_eq!(rejected.status, LinkSuggestionStatus::Rejected);

        let connection = Connection::open(paths.cache_db()).expect("cache should open");
        let inferred: (String, f64) = connection
            .query_row(
                "
                SELECT confidence, confidence_score
                FROM links
                JOIN documents AS source ON source.id = links.source_document_id
                JOIN documents AS target ON target.id = links.resolved_target_id
                WHERE source.path = 'A.md' AND target.path = 'Charlie.md'
                ",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("inferred link should exist");
        assert_eq!(inferred.0, "INFERRED");
        assert!(inferred.1 > 0.0);

        let after_accept =
            suggest_links(&paths, None, None, 0.0, Some(LinkSuggestionStatus::Pending))
                .expect("suggestions should recompute");
        assert!(!after_accept.suggestions.iter().any(|suggestion| {
            suggestion.source_path == "A.md" && suggestion.target_path == "Charlie.md"
        }));
    }

    #[test]
    fn suggest_links_scores_cross_community_and_is_idempotent() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::write(
            vault_root.join("Alpha.md"),
            "# Alpha\n\nBeta is relevant.\n",
        )
        .expect("write alpha");
        fs::write(vault_root.join("Beta.md"), "# Beta\n").expect("write beta");
        fs::write(vault_root.join("Direct.md"), "# Direct\n\n[[Beta]]\n").expect("write direct");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let connection = Connection::open(paths.cache_db()).expect("cache should open");
        let alpha_id: String = connection
            .query_row(
                "SELECT id FROM documents WHERE path = 'Alpha.md'",
                [],
                |row| row.get(0),
            )
            .expect("alpha id");
        let beta_id: String = connection
            .query_row(
                "SELECT id FROM documents WHERE path = 'Beta.md'",
                [],
                |row| row.get(0),
            )
            .expect("beta id");
        connection
            .execute(
                "INSERT INTO graph_clusters (document_id, community_id, label, cohesion, computed_at)
                 VALUES (?1, 1, 'alpha', 1.0, 'now'), (?2, 2, 'beta', 1.0, 'now')",
                params![alpha_id, beta_id],
            )
            .expect("clusters should insert");

        let first =
            suggest_links(&paths, None, None, 0.0, None).expect("suggestions should compute");
        let second =
            suggest_links(&paths, None, None, 0.0, None).expect("suggestions should recompute");
        let first_summary = first
            .suggestions
            .iter()
            .map(|suggestion| {
                (
                    suggestion.source_path.clone(),
                    suggestion.target_path.clone(),
                    format!("{:.6}", suggestion.score),
                    suggestion.status,
                )
            })
            .collect::<Vec<_>>();
        let second_summary = second
            .suggestions
            .iter()
            .map(|suggestion| {
                (
                    suggestion.source_path.clone(),
                    suggestion.target_path.clone(),
                    format!("{:.6}", suggestion.score),
                    suggestion.status,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(first_summary, second_summary);

        let alpha_beta = first
            .suggestions
            .iter()
            .find(|suggestion| {
                suggestion.source_path == "Alpha.md" && suggestion.target_path == "Beta.md"
            })
            .expect("cross-community mention should be suggested");
        assert!((alpha_beta.signals.cross_community_bonus - 1.15).abs() < f64::EPSILON);
        assert!(alpha_beta.score > alpha_beta.signals.mention_score);
        assert!(!first.suggestions.iter().any(|suggestion| {
            suggestion.source_path == "Direct.md" && suggestion.target_path == "Beta.md"
        }));
    }

    #[test]
    fn suggest_duplicates_reports_titles_aliases_and_merge_candidates() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("suggestions", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = suggest_duplicates(&paths).expect("duplicate report should succeed");

        assert!(report
            .duplicate_titles
            .iter()
            .any(|group| group.value == "Alpha" && group.paths.len() == 2));
        assert!(report
            .alias_collisions
            .iter()
            .any(|group| group.value == "Guide" && group.paths.len() == 2));
        assert!(report.merge_candidates.iter().any(|candidate| candidate
            .reasons
            .iter()
            .any(|reason| reason.contains("same title"))));
    }

    #[test]
    fn merge_candidates_compares_adjacent_filename_length_buckets() {
        let notes = vec![
            NoteIdentity {
                id: "1".to_string(),
                path: "Docs/Guide.md".to_string(),
                filename: "Guide".to_string(),
                aliases: Vec::new(),
            },
            NoteIdentity {
                id: "2".to_string(),
                path: "Docs/Guides.md".to_string(),
                filename: "Guides".to_string(),
                aliases: Vec::new(),
            },
            NoteIdentity {
                id: "3".to_string(),
                path: "Docs/Guidebook.md".to_string(),
                filename: "Guidebook".to_string(),
                aliases: Vec::new(),
            },
        ];

        let candidates = merge_candidates(&notes, &[], &[]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].left_path, "Docs/Guide.md");
        assert_eq!(candidates[0].right_path, "Docs/Guides.md");
        assert!((candidates[0].score - 0.8).abs() < f64::EPSILON);
        assert_eq!(candidates[0].reasons, vec!["similar title"]);
    }

    #[test]
    fn merge_candidates_bk_tree_finds_single_edit_prefix_changes() {
        let notes = vec![
            NoteIdentity {
                id: "1".to_string(),
                path: "Docs/Guide.md".to_string(),
                filename: "Guide".to_string(),
                aliases: Vec::new(),
            },
            NoteIdentity {
                id: "2".to_string(),
                path: "Docs/Quide.md".to_string(),
                filename: "Quide".to_string(),
                aliases: Vec::new(),
            },
            NoteIdentity {
                id: "3".to_string(),
                path: "Docs/AGuide.md".to_string(),
                filename: "AGuide".to_string(),
                aliases: Vec::new(),
            },
            NoteIdentity {
                id: "4".to_string(),
                path: "Docs/Guidebook.md".to_string(),
                filename: "Guidebook".to_string(),
                aliases: Vec::new(),
            },
        ];

        let candidates = merge_candidates(&notes, &[], &[]);
        let pairs = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.left_path.as_str(),
                    candidate.right_path.as_str(),
                    candidate.score,
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            pairs,
            vec![
                ("Docs/AGuide.md", "Docs/Guide.md", 0.8),
                ("Docs/Guide.md", "Docs/Quide.md", 0.8),
            ]
        );
    }

    #[test]
    #[ignore = "benchmark-style regression test; run manually with --ignored --nocapture"]
    fn suggest_mentions_benchmark_with_large_candidate_set() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::create_dir_all(&vault_root).expect("vault directory should be created");
        let candidate_count = 1_200_usize;
        let matched_indexes = [7_usize, 456, 1_199];

        for index in 0..candidate_count {
            let title = format!("Topic{index:04}");
            write_note(
                &vault_root,
                &format!("{title}.md"),
                &format!("# {title}\n\nSynthetic benchmark note.\n"),
            );
        }

        let home_contents = format!(
            "# Home\n\n{} should all resolve as suggestions.\n",
            matched_indexes
                .iter()
                .map(|index| format!("Topic{index:04}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        write_note(&vault_root, "Home.md", &home_contents);

        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let started = Instant::now();
        let report = suggest_mentions(&paths, Some("Home")).expect("suggestions should succeed");
        let elapsed = started.elapsed();

        assert_eq!(report.suggestions.len(), matched_indexes.len());
        for index in matched_indexes {
            let matched_text = format!("Topic{index:04}");
            assert!(report.suggestions.iter().any(|suggestion| {
                suggestion.matched_text == matched_text
                    && suggestion.target_path.as_deref() == Some(&format!("{matched_text}.md"))
            }));
        }

        eprintln!("suggest_mentions benchmark: {candidate_count} candidates in {elapsed:?}");
    }

    #[test]
    fn bulk_replace_filters_selected_notes_and_reindexes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("mixed-properties", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = bulk_replace(
            &paths,
            &["reviewed = true".to_string()],
            "release",
            "launch",
            false,
        )
        .expect("rewrite should succeed");

        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].path, "Done.md");
        assert!(fs::read_to_string(vault_root.join("Done.md"))
            .expect("done note should read")
            .contains("launch"));
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_recursive(&source, destination);
        fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
    }

    fn write_note(vault_root: &Path, relative_path: &str, contents: &str) {
        let path = vault_root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory should be created");
        }
        fs::write(path, contents).expect("note should be written");
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).expect("parent directory should exist");
                }
                fs::copy(entry.path(), target).expect("file should be copied");
            }
        }
    }
}
