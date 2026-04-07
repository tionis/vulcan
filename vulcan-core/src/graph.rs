use crate::VaultPaths;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;

#[derive(Debug)]
pub enum GraphQueryError {
    AmbiguousIdentifier {
        identifier: String,
        matches: Vec<String>,
    },
    CacheMissing,
    Io(std::io::Error),
    NoteNotFound {
        identifier: String,
    },
    Sqlite(rusqlite::Error),
}

impl Display for GraphQueryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AmbiguousIdentifier {
                identifier,
                matches,
            } => write!(
                formatter,
                "note identifier '{identifier}' is ambiguous: {}",
                matches.join(", ")
            ),
            Self::CacheMissing => {
                formatter.write_str("cache is missing; run `vulcan scan` before querying the graph")
            }
            Self::Io(error) => write!(formatter, "{error}"),
            Self::NoteNotFound { identifier } => {
                write!(formatter, "note not found: {identifier}")
            }
            Self::Sqlite(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for GraphQueryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::AmbiguousIdentifier { .. } | Self::CacheMissing | Self::NoteNotFound { .. } => {
                None
            }
        }
    }
}

impl From<std::io::Error> for GraphQueryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for GraphQueryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteMatchKind {
    Path,
    Filename,
    Alias,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    External,
    Resolved,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineContext {
    pub line: usize,
    pub column: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutgoingLinksReport {
    pub note_path: String,
    pub matched_by: NoteMatchKind,
    pub links: Vec<OutgoingLinkRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutgoingLinkRecord {
    pub raw_text: String,
    pub link_kind: String,
    pub display_text: Option<String>,
    pub target_path_candidate: Option<String>,
    pub target_heading: Option<String>,
    pub target_block: Option<String>,
    pub resolved_target_path: Option<String>,
    pub resolution_status: ResolutionStatus,
    pub context: Option<LineContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BacklinksReport {
    pub note_path: String,
    pub matched_by: NoteMatchKind,
    pub backlinks: Vec<BacklinkRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BacklinkRecord {
    pub source_path: String,
    pub raw_text: String,
    pub link_kind: String,
    pub display_text: Option<String>,
    pub context: Option<LineContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedNote {
    id: String,
    path: String,
    filename: String,
    aliases: Vec<String>,
}

struct IndexedNoteSet {
    notes: Vec<IndexedNote>,
    by_path: HashMap<String, usize>,
    by_filename: HashMap<String, Vec<usize>>,
    by_alias: HashMap<String, Vec<usize>>,
}

impl IndexedNoteSet {
    fn build(notes: Vec<IndexedNote>) -> Self {
        let mut by_path = HashMap::with_capacity(notes.len());
        let mut by_filename: HashMap<String, Vec<usize>> = HashMap::with_capacity(notes.len());
        let mut by_alias: HashMap<String, Vec<usize>> = HashMap::new();

        for (index, note) in notes.iter().enumerate() {
            // Index by lowercase full path and by path without .md extension.
            by_path.insert(note.path.to_ascii_lowercase(), index);
            let stripped = strip_markdown_extension(&note.path).to_ascii_lowercase();
            if stripped != note.path.to_ascii_lowercase() {
                by_path.entry(stripped).or_insert(index);
            }

            // Index by lowercase filename and by filename with .md appended.
            let lower_filename = note.filename.to_ascii_lowercase();
            by_filename
                .entry(lower_filename.clone())
                .or_default()
                .push(index);
            by_filename
                .entry(format!("{lower_filename}.md"))
                .or_default()
                .push(index);

            // Index each alias in lowercase.
            for alias in &note.aliases {
                by_alias
                    .entry(alias.to_ascii_lowercase())
                    .or_default()
                    .push(index);
            }
        }

        Self {
            notes,
            by_path,
            by_filename,
            by_alias,
        }
    }

    fn resolve(&self, identifier: &str) -> Result<ResolvedNote, GraphQueryError> {
        let lower = identifier.to_ascii_lowercase();

        // Priority 1: exact path match (O(1)).
        if let Some(&index) = self.by_path.get(&lower) {
            let note = &self.notes[index];
            return Ok(ResolvedNote {
                id: note.id.clone(),
                path: note.path.clone(),
                matched_by: NoteMatchKind::Path,
            });
        }

        // Priority 2: filename match — may be ambiguous.
        if let Some(indices) = self.by_filename.get(&lower) {
            // Deduplicate: a note may be in this list twice (once for "foo", once for "foo.md").
            let unique: Vec<usize> = {
                let mut seen = std::collections::HashSet::new();
                indices
                    .iter()
                    .copied()
                    .filter(|i| seen.insert(*i))
                    .collect()
            };
            match unique.as_slice() {
                [single] => {
                    let note = &self.notes[*single];
                    return Ok(ResolvedNote {
                        id: note.id.clone(),
                        path: note.path.clone(),
                        matched_by: NoteMatchKind::Filename,
                    });
                }
                [] => {}
                _ => {
                    let mut paths = unique
                        .iter()
                        .map(|&i| self.notes[i].path.clone())
                        .collect::<Vec<_>>();
                    paths.sort();
                    return Err(GraphQueryError::AmbiguousIdentifier {
                        identifier: identifier.to_string(),
                        matches: paths,
                    });
                }
            }
        }

        // Priority 3: alias match — may be ambiguous.
        if let Some(indices) = self.by_alias.get(&lower) {
            match indices.as_slice() {
                [single] => {
                    let note = &self.notes[*single];
                    return Ok(ResolvedNote {
                        id: note.id.clone(),
                        path: note.path.clone(),
                        matched_by: NoteMatchKind::Alias,
                    });
                }
                [] => {}
                _ => {
                    let mut paths = indices
                        .iter()
                        .map(|&i| self.notes[i].path.clone())
                        .collect::<Vec<_>>();
                    paths.sort();
                    return Err(GraphQueryError::AmbiguousIdentifier {
                        identifier: identifier.to_string(),
                        matches: paths,
                    });
                }
            }
        }

        Err(GraphQueryError::NoteNotFound {
            identifier: identifier.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedNote {
    id: String,
    path: String,
    matched_by: NoteMatchKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteReference {
    pub id: String,
    pub path: String,
    pub matched_by: NoteMatchKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteIdentity {
    pub path: String,
    pub filename: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphPathReport {
    pub from_path: String,
    pub to_path: String,
    pub path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphNodeScore {
    pub document_path: String,
    pub inbound: usize,
    pub outbound: usize,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphHubsReport {
    pub notes: Vec<GraphNodeScore>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphMocCandidate {
    pub document_path: String,
    pub inbound: usize,
    pub outbound: usize,
    pub score: usize,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphMocReport {
    pub notes: Vec<GraphMocCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphDeadEndsReport {
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphComponent {
    pub size: usize,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphComponentsReport {
    pub components: Vec<GraphComponent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NamedCount {
    pub name: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphAnalyticsReport {
    pub note_count: usize,
    pub attachment_count: usize,
    pub base_count: usize,
    pub resolved_note_links: usize,
    pub average_outbound_links: f64,
    pub orphan_notes: usize,
    pub top_tags: Vec<NamedCount>,
    pub top_properties: Vec<NamedCount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphAdjacency {
    edges: Vec<(String, String)>,
    counts: HashMap<String, (usize, usize)>,
}

impl GraphAdjacency {
    fn load(connection: &Connection) -> Result<Self, GraphQueryError> {
        let mut edges = Vec::new();
        let mut counts = HashMap::<String, (usize, usize)>::new();
        let mut statement = connection.prepare(
            "
            SELECT source.id, target.id
            FROM links
            JOIN documents AS source ON source.id = links.source_document_id
            JOIN documents AS target ON target.id = links.resolved_target_id
            WHERE source.extension = 'md' AND target.extension = 'md'
            ORDER BY source.path, target.path, links.byte_offset
            ",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (source_id, target_id) = row?;
            counts.entry(source_id.clone()).or_insert((0, 0)).1 += 1;
            counts.entry(target_id.clone()).or_insert((0, 0)).0 += 1;
            edges.push((source_id, target_id));
        }

        Ok(Self { edges, counts })
    }

    fn inbound_count(&self, note_id: &str) -> usize {
        self.counts.get(note_id).map_or(0, |(inbound, _)| *inbound)
    }

    fn outbound_count(&self, note_id: &str) -> usize {
        self.counts
            .get(note_id)
            .map_or(0, |(_, outbound)| *outbound)
    }

    fn is_orphan(&self, note_id: &str) -> bool {
        self.counts.get(note_id).copied().unwrap_or((0, 0)) == (0, 0)
    }

    fn total_resolved_links(&self) -> usize {
        self.edges.len()
    }

    fn directed(&self) -> HashMap<String, Vec<String>> {
        let mut adjacency = HashMap::<String, Vec<String>>::new();
        for (source_id, target_id) in &self.edges {
            adjacency
                .entry(source_id.clone())
                .or_default()
                .push(target_id.clone());
        }
        adjacency
    }

    fn undirected(&self) -> HashMap<String, BTreeSet<String>> {
        let mut adjacency = HashMap::<String, BTreeSet<String>>::new();
        for (source_id, target_id) in &self.edges {
            adjacency
                .entry(source_id.clone())
                .or_default()
                .insert(target_id.clone());
            adjacency
                .entry(target_id.clone())
                .or_default()
                .insert(source_id.clone());
        }
        adjacency
    }

    fn hubs(&self, notes: &[IndexedNote], min_degree: usize) -> Vec<GraphNodeScore> {
        notes
            .iter()
            .filter_map(|note| {
                let inbound = self.inbound_count(&note.id);
                let outbound = self.outbound_count(&note.id);
                let total = inbound + outbound;
                (total >= min_degree).then(|| GraphNodeScore {
                    document_path: note.path.clone(),
                    inbound,
                    outbound,
                    total,
                })
            })
            .collect()
    }
}

pub fn resolve_note_reference(
    paths: &VaultPaths,
    identifier: &str,
) -> Result<NoteReference, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let note = notes.resolve(identifier)?;

    Ok(NoteReference {
        id: note.id,
        path: note.path,
        matched_by: note.matched_by,
    })
}

pub fn list_note_identities(paths: &VaultPaths) -> Result<Vec<NoteIdentity>, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;

    Ok(notes
        .notes
        .into_iter()
        .map(|note| NoteIdentity {
            path: note.path,
            filename: note.filename,
            aliases: note.aliases,
        })
        .collect())
}

pub fn list_tags(paths: &VaultPaths) -> Result<Vec<NamedCount>, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    tag_counts(&connection, None)
}

pub fn list_tagged_note_identities(
    paths: &VaultPaths,
    tag: &str,
) -> Result<Vec<NoteIdentity>, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let mut indexed = notes
        .notes
        .into_iter()
        .map(|note| {
            (
                note.path.clone(),
                NoteIdentity {
                    path: note.path,
                    filename: note.filename,
                    aliases: note.aliases,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let mut statement = connection.prepare(
        "
        SELECT DISTINCT documents.path
        FROM tags
        JOIN documents ON documents.id = tags.document_id
        WHERE documents.extension = 'md' AND tags.tag_text = ?1
        ORDER BY documents.path
        ",
    )?;
    let rows = statement.query_map([tag], |row| row.get::<_, String>(0))?;
    let mut tagged = Vec::new();
    for row in rows {
        let path = row?;
        if let Some(identity) = indexed.remove(&path) {
            tagged.push(identity);
        }
    }
    Ok(tagged)
}

pub fn query_links(
    paths: &VaultPaths,
    identifier: &str,
) -> Result<OutgoingLinksReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let note = notes.resolve(identifier)?;
    let mut source_cache = HashMap::new();
    let mut statement = connection.prepare(
        "
        SELECT
            links.raw_text,
            links.link_kind,
            links.display_text,
            links.target_path_candidate,
            links.target_heading,
            links.target_block,
            links.byte_offset,
            target.path
        FROM links
        LEFT JOIN documents AS target ON target.id = links.resolved_target_id
        WHERE links.source_document_id = ?1
        ORDER BY links.byte_offset
        ",
    )?;
    let rows = statement.query_map(params![&note.id], |row| {
        Ok(OutgoingLinkRow {
            raw_text: row.get(0)?,
            link_kind: row.get(1)?,
            display_text: row.get(2)?,
            target_path_candidate: row.get(3)?,
            target_heading: row.get(4)?,
            target_block: row.get(5)?,
            byte_offset: row.get(6)?,
            resolved_target_path: row.get(7)?,
        })
    })?;
    let links = rows
        .map(|row| {
            let row = row?;
            let has_resolved_target = row.resolved_target_path.is_some();
            Ok(OutgoingLinkRecord {
                raw_text: row.raw_text,
                link_kind: row.link_kind.clone(),
                display_text: row.display_text,
                target_path_candidate: row.target_path_candidate,
                target_heading: row.target_heading,
                target_block: row.target_block,
                resolved_target_path: row.resolved_target_path,
                resolution_status: resolution_status(&row.link_kind, has_resolved_target),
                context: load_context(paths, &note.path, row.byte_offset, &mut source_cache),
            })
        })
        .collect::<Result<Vec<_>, GraphQueryError>>()?;

    Ok(OutgoingLinksReport {
        note_path: note.path,
        matched_by: note.matched_by,
        links,
    })
}

pub fn query_backlinks(
    paths: &VaultPaths,
    identifier: &str,
) -> Result<BacklinksReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let note = notes.resolve(identifier)?;
    let mut source_cache = HashMap::new();
    let mut statement = connection.prepare(
        "
        SELECT
            source.path,
            links.raw_text,
            links.link_kind,
            links.display_text,
            links.byte_offset
        FROM links
        JOIN documents AS source ON source.id = links.source_document_id
        WHERE links.resolved_target_id = ?1
        ORDER BY source.path, links.byte_offset
        ",
    )?;
    let rows = statement.query_map(params![&note.id], |row| {
        Ok(BacklinkRow {
            source_path: row.get(0)?,
            raw_text: row.get(1)?,
            link_kind: row.get(2)?,
            display_text: row.get(3)?,
            byte_offset: row.get(4)?,
        })
    })?;
    let backlinks = rows
        .map(|row| {
            let row = row?;
            Ok(BacklinkRecord {
                source_path: row.source_path.clone(),
                raw_text: row.raw_text,
                link_kind: row.link_kind,
                display_text: row.display_text,
                context: load_context(paths, &row.source_path, row.byte_offset, &mut source_cache),
            })
        })
        .collect::<Result<Vec<_>, GraphQueryError>>()?;

    Ok(BacklinksReport {
        note_path: note.path,
        matched_by: note.matched_by,
        backlinks,
    })
}

pub fn query_graph_path(
    paths: &VaultPaths,
    from_identifier: &str,
    to_identifier: &str,
) -> Result<GraphPathReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let adjacency = GraphAdjacency::load(&connection)?;
    build_graph_path_report(&notes, &adjacency, from_identifier, to_identifier)
}

pub fn query_graph_hubs(paths: &VaultPaths) -> Result<GraphHubsReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let adjacency = GraphAdjacency::load(&connection)?;
    Ok(build_graph_hubs_report(&notes, &adjacency))
}

pub fn query_graph_moc_candidates(paths: &VaultPaths) -> Result<GraphMocReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let adjacency = GraphAdjacency::load(&connection)?;
    Ok(build_graph_moc_report(&notes, &adjacency))
}

pub fn query_graph_dead_ends(paths: &VaultPaths) -> Result<GraphDeadEndsReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let adjacency = GraphAdjacency::load(&connection)?;
    Ok(build_graph_dead_ends_report(&notes, &adjacency))
}

pub fn query_graph_components(
    paths: &VaultPaths,
) -> Result<GraphComponentsReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let adjacency = GraphAdjacency::load(&connection)?;
    Ok(build_graph_components_report(&notes, &adjacency))
}

pub fn query_graph_analytics(paths: &VaultPaths) -> Result<GraphAnalyticsReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let adjacency = GraphAdjacency::load(&connection)?;
    build_graph_analytics_report(&connection, &notes, &adjacency)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphExportNode {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphExportEdge {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphExportReport {
    pub nodes: Vec<GraphExportNode>,
    pub edges: Vec<GraphExportEdge>,
}

pub fn export_graph(paths: &VaultPaths) -> Result<GraphExportReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let adjacency = GraphAdjacency::load(&connection)?;
    let id_to_path: HashMap<&str, &str> = notes
        .notes
        .iter()
        .map(|note| (note.id.as_str(), note.path.as_str()))
        .collect();
    let export_nodes = notes
        .notes
        .iter()
        .map(|note| GraphExportNode {
            id: note.path.clone(),
            path: note.path.clone(),
        })
        .collect();
    let edges = adjacency
        .edges
        .iter()
        .filter_map(|(src_id, tgt_id)| {
            let src = id_to_path.get(src_id.as_str())?;
            let tgt = id_to_path.get(tgt_id.as_str())?;
            Some(GraphExportEdge {
                source: (*src).to_string(),
                target: (*tgt).to_string(),
            })
        })
        .collect();
    Ok(GraphExportReport { nodes: export_nodes, edges })
}

fn build_graph_path_report(
    notes: &IndexedNoteSet,
    adjacency: &GraphAdjacency,
    from_identifier: &str,
    to_identifier: &str,
) -> Result<GraphPathReport, GraphQueryError> {
    let from = notes.resolve(from_identifier)?;
    let to = notes.resolve(to_identifier)?;
    let directed = adjacency.directed();
    let path_by_id = notes
        .notes
        .iter()
        .map(|note| (note.id.as_str(), note.path.as_str()))
        .collect::<HashMap<_, _>>();
    let path = shortest_path(&directed, &from.id, &to.id)
        .unwrap_or_default()
        .into_iter()
        .map(|id| {
            path_by_id
                .get(id.as_str())
                .map(|path| (*path).to_string())
                .unwrap_or(id)
        })
        .collect::<Vec<_>>();

    Ok(GraphPathReport {
        from_path: from.path,
        to_path: to.path,
        path,
    })
}

fn build_graph_hubs_report(notes: &IndexedNoteSet, adjacency: &GraphAdjacency) -> GraphHubsReport {
    let mut scored = adjacency.hubs(&notes.notes, 0);
    scored.sort_by(|left, right| {
        right
            .total
            .cmp(&left.total)
            .then(right.inbound.cmp(&left.inbound))
            .then(right.outbound.cmp(&left.outbound))
            .then(left.document_path.cmp(&right.document_path))
    });

    GraphHubsReport { notes: scored }
}

fn build_graph_moc_report(notes: &IndexedNoteSet, adjacency: &GraphAdjacency) -> GraphMocReport {
    let mut candidates = notes
        .notes
        .iter()
        .filter_map(|note| {
            let inbound = adjacency.inbound_count(&note.id);
            let outbound = adjacency.outbound_count(&note.id);
            let mut reasons = Vec::new();
            let lower_path = note.path.to_ascii_lowercase();
            if outbound >= 3 {
                reasons.push(format!("{outbound} outbound links"));
            }
            if inbound >= 2 {
                reasons.push(format!("{inbound} inbound links"));
            }
            if ["index", "home", "overview", "hub", "map", "moc"]
                .iter()
                .any(|keyword| lower_path.contains(keyword))
            {
                reasons.push("title hints at an index note".to_string());
            }
            if reasons.is_empty() {
                return None;
            }

            let title_bonus = usize::from(
                ["index", "home", "overview", "hub", "map", "moc"]
                    .iter()
                    .any(|keyword| lower_path.contains(keyword)),
            ) * 5;
            Some(GraphMocCandidate {
                document_path: note.path.clone(),
                inbound,
                outbound,
                score: outbound.saturating_mul(3) + inbound + title_bonus,
                reasons,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(right.outbound.cmp(&left.outbound))
            .then(right.inbound.cmp(&left.inbound))
            .then(left.document_path.cmp(&right.document_path))
    });

    GraphMocReport { notes: candidates }
}

fn build_graph_dead_ends_report(
    notes: &IndexedNoteSet,
    adjacency: &GraphAdjacency,
) -> GraphDeadEndsReport {
    let mut dead_ends = notes
        .notes
        .iter()
        .filter(|note| adjacency.outbound_count(&note.id) == 0)
        .map(|note| note.path.clone())
        .collect::<Vec<_>>();
    dead_ends.sort();

    GraphDeadEndsReport { notes: dead_ends }
}

fn build_graph_components_report(
    notes: &IndexedNoteSet,
    adjacency: &GraphAdjacency,
) -> GraphComponentsReport {
    let undirected = adjacency.undirected();
    let path_by_id = notes
        .notes
        .iter()
        .map(|note| (note.id.clone(), note.path.clone()))
        .collect::<HashMap<_, _>>();
    let mut remaining = notes
        .notes
        .iter()
        .map(|note| note.id.clone())
        .collect::<BTreeSet<_>>();
    let mut components = Vec::new();

    while let Some(start) = remaining.pop_first() {
        let mut queue = VecDeque::from([start.clone()]);
        let mut visited = BTreeSet::from([start]);
        while let Some(current) = queue.pop_front() {
            for neighbor in undirected.get(&current).into_iter().flatten() {
                if visited.insert(neighbor.clone()) {
                    remaining.remove(neighbor);
                    queue.push_back(neighbor.clone());
                }
            }
        }

        let notes = visited
            .into_iter()
            .map(|id| path_by_id.get(&id).cloned().unwrap_or(id))
            .collect::<Vec<_>>();
        components.push(GraphComponent {
            size: notes.len(),
            notes,
        });
    }

    components.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then(left.notes.cmp(&right.notes))
    });

    GraphComponentsReport { components }
}

fn build_graph_analytics_report(
    connection: &Connection,
    notes: &IndexedNoteSet,
    adjacency: &GraphAdjacency,
) -> Result<GraphAnalyticsReport, GraphQueryError> {
    let resolved_note_links = adjacency.total_resolved_links();
    let orphan_notes = notes
        .notes
        .iter()
        .filter(|note| adjacency.is_orphan(&note.id))
        .count();
    let note_count = notes.notes.len();

    Ok(GraphAnalyticsReport {
        note_count,
        attachment_count: count_documents_by_extension_group(connection, "attachment")?,
        base_count: count_documents_by_extension_group(connection, "base")?,
        resolved_note_links,
        average_outbound_links: if note_count == 0 {
            0.0
        } else {
            count_as_f64(resolved_note_links) / count_as_f64(note_count)
        },
        orphan_notes,
        top_tags: top_tag_counts(connection)?,
        top_properties: top_property_counts(connection)?,
    })
}

fn open_existing_cache(paths: &VaultPaths) -> Result<Connection, GraphQueryError> {
    if !paths.cache_db().exists() {
        return Err(GraphQueryError::CacheMissing);
    }

    Ok(Connection::open(paths.cache_db())?)
}

fn load_indexed_notes(connection: &Connection) -> Result<IndexedNoteSet, GraphQueryError> {
    let mut alias_statement =
        connection.prepare("SELECT document_id, alias_text FROM aliases ORDER BY alias_text")?;
    let alias_rows = alias_statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut aliases_by_document = HashMap::new();
    for row in alias_rows {
        let (document_id, alias_text) = row?;
        aliases_by_document
            .entry(document_id)
            .or_insert_with(Vec::new)
            .push(alias_text);
    }

    let mut statement = connection
        .prepare("SELECT id, path, filename FROM documents WHERE extension = 'md' ORDER BY path")?;
    let rows = statement.query_map([], |row| {
        let id: String = row.get(0)?;
        Ok(IndexedNote {
            aliases: aliases_by_document.remove(&id).unwrap_or_default(),
            id,
            path: row.get(1)?,
            filename: row.get(2)?,
        })
    })?;

    let notes = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(GraphQueryError::from)?;
    Ok(IndexedNoteSet::build(notes))
}

fn strip_markdown_extension(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

fn resolution_status(link_kind: &str, has_resolved_target: bool) -> ResolutionStatus {
    if link_kind == "external" {
        ResolutionStatus::External
    } else if has_resolved_target {
        ResolutionStatus::Resolved
    } else {
        ResolutionStatus::Unresolved
    }
}

fn load_context(
    paths: &VaultPaths,
    relative_path: &str,
    byte_offset: usize,
    source_cache: &mut HashMap<String, Option<String>>,
) -> Option<LineContext> {
    let source = if let Some(source) = source_cache.get(relative_path) {
        source.clone()
    } else {
        let source = fs::read_to_string(paths.vault_root().join(relative_path)).ok();
        source_cache.insert(relative_path.to_string(), source.clone());
        source
    };

    source.and_then(|text| line_context(&text, byte_offset))
}

fn line_context(source: &str, byte_offset: usize) -> Option<LineContext> {
    let clamped = byte_offset.min(source.len());
    if !source.is_char_boundary(clamped) {
        return None;
    }

    let prefix = &source[..clamped];
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[clamped..]
        .find('\n')
        .map_or(source.len(), |index| clamped + index);
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = source[line_start..clamped].chars().count() + 1;

    Some(LineContext {
        line,
        column,
        text: source[line_start..line_end]
            .trim_end_matches('\r')
            .to_string(),
    })
}

struct OutgoingLinkRow {
    raw_text: String,
    link_kind: String,
    display_text: Option<String>,
    target_path_candidate: Option<String>,
    target_heading: Option<String>,
    target_block: Option<String>,
    byte_offset: usize,
    resolved_target_path: Option<String>,
}

struct BacklinkRow {
    source_path: String,
    raw_text: String,
    link_kind: String,
    display_text: Option<String>,
    byte_offset: usize,
}

fn count_documents_by_extension_group(
    connection: &Connection,
    group: &str,
) -> Result<usize, GraphQueryError> {
    let sql = match group {
        "attachment" => "SELECT COUNT(*) FROM documents WHERE extension NOT IN ('md', 'base')",
        "base" => "SELECT COUNT(*) FROM documents WHERE extension = 'base'",
        _ => unreachable!(),
    };
    let count: i64 = connection.query_row(sql, [], |row| row.get(0))?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

fn top_tag_counts(connection: &Connection) -> Result<Vec<NamedCount>, GraphQueryError> {
    tag_counts(connection, Some(10))
}

fn tag_counts(
    connection: &Connection,
    limit: Option<usize>,
) -> Result<Vec<NamedCount>, GraphQueryError> {
    let sql = match limit {
        Some(limit) => format!(
            "
            SELECT tags.tag_text, COUNT(DISTINCT tags.document_id) AS usage_count
            FROM tags
            JOIN documents ON documents.id = tags.document_id
            WHERE documents.extension = 'md'
            GROUP BY tags.tag_text
            ORDER BY usage_count DESC, tags.tag_text ASC
            LIMIT {limit}
            "
        ),
        None => "
            SELECT tags.tag_text, COUNT(DISTINCT tags.document_id) AS usage_count
            FROM tags
            JOIN documents ON documents.id = tags.document_id
            WHERE documents.extension = 'md'
            GROUP BY tags.tag_text
            ORDER BY usage_count DESC, tags.tag_text ASC
            "
        .to_string(),
    };
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok(NamedCount {
            name: row.get(0)?,
            count: usize::try_from(row.get::<_, i64>(1)?).unwrap_or(usize::MAX),
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(GraphQueryError::from)
}

fn top_property_counts(connection: &Connection) -> Result<Vec<NamedCount>, GraphQueryError> {
    let mut statement = connection.prepare(
        "
        SELECT key, SUM(usage_count) AS total_usage
        FROM property_catalog
        GROUP BY key
        ORDER BY total_usage DESC, key ASC
        LIMIT 10
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(NamedCount {
            name: row.get(0)?,
            count: usize::try_from(row.get::<_, i64>(1)?).unwrap_or(usize::MAX),
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(GraphQueryError::from)
}

fn shortest_path(
    adjacency: &HashMap<String, Vec<String>>,
    start: &str,
    goal: &str,
) -> Option<Vec<String>> {
    if start == goal {
        return Some(vec![start.to_string()]);
    }

    let mut queue = VecDeque::from([start.to_string()]);
    let mut visited = HashSet::from([start.to_string()]);
    let mut predecessor = HashMap::<String, String>::new();
    while let Some(current) = queue.pop_front() {
        for neighbor in adjacency.get(&current).into_iter().flatten() {
            if !visited.insert(neighbor.clone()) {
                continue;
            }
            predecessor.insert(neighbor.clone(), current.clone());
            if neighbor == goal {
                let mut path = vec![goal.to_string()];
                let mut cursor = goal;
                while let Some(previous) = predecessor.get(cursor) {
                    path.push(previous.clone());
                    if previous == start {
                        break;
                    }
                    cursor = previous;
                }
                path.reverse();
                return Some(path);
            }
            queue.push_back(neighbor.clone());
        }
    }

    None
}

fn count_as_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan_vault, ScanMode};
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn query_links_resolves_path_filename_and_alias() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let by_path = query_links(&paths, "Home.md").expect("path query should succeed");
        let by_filename = query_links(&paths, "Bob").expect("filename query should succeed");
        let by_alias = query_links(&paths, "Start").expect("alias query should succeed");

        assert_eq!(by_path.note_path, "Home.md");
        assert_eq!(by_path.matched_by, NoteMatchKind::Path);
        assert_eq!(by_filename.note_path, "People/Bob.md");
        assert_eq!(by_filename.matched_by, NoteMatchKind::Filename);
        assert_eq!(by_alias.note_path, "Home.md");
        assert_eq!(by_alias.matched_by, NoteMatchKind::Alias);
        assert_eq!(by_alias.links.len(), 2);
        assert_eq!(
            by_alias
                .links
                .iter()
                .map(|link| link.resolved_target_path.clone())
                .collect::<Vec<_>>(),
            vec![
                Some("Projects/Alpha.md".to_string()),
                Some("People/Bob.md".to_string())
            ]
        );
    }

    #[test]
    fn query_backlinks_returns_sources_with_context() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = query_backlinks(&paths, "Projects/Alpha").expect("query should succeed");

        assert_eq!(report.note_path, "Projects/Alpha.md");
        assert_eq!(
            report
                .backlinks
                .iter()
                .map(|link| (
                    link.source_path.clone(),
                    link.context.as_ref().map(|context| context.line)
                ))
                .collect::<Vec<_>>(),
            vec![
                ("Home.md".to_string(), Some(10)),
                ("People/Bob.md".to_string(), Some(8))
            ]
        );
    }

    #[test]
    fn ambiguous_identifiers_are_reported() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("ambiguous-links", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let error = query_links(&paths, "Topic").expect_err("query should fail");

        match error {
            GraphQueryError::AmbiguousIdentifier { matches, .. } => assert_eq!(
                matches,
                vec![
                    "Archive/Topic.md".to_string(),
                    "Projects/Topic.md".to_string()
                ]
            ),
            other => panic!("expected ambiguous identifier error, got {other:?}"),
        }
    }

    #[test]
    fn graph_analysis_reports_paths_hubs_components_and_stats() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let path_report =
            query_graph_path(&paths, "Bob", "Home").expect("path query should succeed");
        assert_eq!(
            path_report.path,
            vec![
                "People/Bob.md".to_string(),
                "Projects/Alpha.md".to_string(),
                "Home.md".to_string()
            ]
        );

        let hubs = query_graph_hubs(&paths).expect("hub query should succeed");
        assert_eq!(hubs.notes[0].document_path, "Projects/Alpha.md");
        assert_eq!(hubs.notes[0].total, 4);

        let moc = query_graph_moc_candidates(&paths).expect("moc query should succeed");
        assert_eq!(moc.notes[0].document_path, "Home.md");
        assert!(moc.notes[0]
            .reasons
            .iter()
            .any(|reason| reason.contains("index note")));

        let dead_ends = query_graph_dead_ends(&paths).expect("dead-end query should succeed");
        assert!(dead_ends.notes.is_empty());

        let components = query_graph_components(&paths).expect("component query should succeed");
        assert_eq!(components.components.len(), 1);
        assert_eq!(components.components[0].size, 3);

        let analytics = query_graph_analytics(&paths).expect("analytics query should succeed");
        assert_eq!(analytics.note_count, 3);
        assert_eq!(analytics.attachment_count, 0);
        assert_eq!(analytics.base_count, 0);
        assert_eq!(analytics.resolved_note_links, 5);
        assert_eq!(analytics.orphan_notes, 0);
        assert!(analytics
            .top_properties
            .iter()
            .any(|property| property.name == "status"));
    }

    #[test]
    fn list_note_identities_includes_paths_and_aliases() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let identities = list_note_identities(&paths).expect("listing identities should succeed");

        assert_eq!(identities.len(), 3);
        assert!(identities.iter().any(|identity| {
            identity.path == "Home.md"
                && identity.filename == "Home"
                && identity.aliases == vec!["Start".to_string()]
        }));
    }

    #[test]
    fn list_tags_returns_distinct_tag_counts() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let tags = list_tags(&paths).expect("tag listing should succeed");

        assert_eq!(
            tags,
            vec![
                NamedCount {
                    name: "dashboard".to_string(),
                    count: 1,
                },
                NamedCount {
                    name: "index".to_string(),
                    count: 1,
                },
                NamedCount {
                    name: "people/team".to_string(),
                    count: 1,
                },
                NamedCount {
                    name: "project".to_string(),
                    count: 1,
                },
                NamedCount {
                    name: "work".to_string(),
                    count: 1,
                },
            ]
        );
    }

    #[test]
    fn list_tagged_note_identities_resolves_inline_and_frontmatter_tags() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let dashboard = list_tagged_note_identities(&paths, "dashboard")
            .expect("dashboard tag listing should succeed");
        let people_team = list_tagged_note_identities(&paths, "people/team")
            .expect("people/team tag listing should succeed");

        assert_eq!(
            dashboard,
            vec![NoteIdentity {
                path: "Home.md".to_string(),
                filename: "Home".to_string(),
                aliases: vec!["Start".to_string()],
            }]
        );
        assert_eq!(
            people_team,
            vec![NoteIdentity {
                path: "People/Bob.md".to_string(),
                filename: "Bob".to_string(),
                aliases: vec!["Robert".to_string()],
            }]
        );
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);

        copy_dir_recursive(&source, destination);
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        std::fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in std::fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).expect("parent directory should exist");
                }
                std::fs::copy(entry.path(), target).expect("file should be copied");
            }
        }
    }
}
