use crate::properties::build_note_filter_clause;
use crate::vector::query_hybrid_candidates;
use crate::{CacheDatabase, CacheError, PropertyError, VaultPaths};
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum SearchError {
    CacheMissing,
    Cache(CacheError),
    InvalidQuery(String),
    Json(serde_json::Error),
    Property(PropertyError),
    Sqlite(rusqlite::Error),
    Vector(crate::VectorError),
}

impl Display for SearchError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CacheMissing => {
                formatter.write_str("cache is missing; run `vulcan scan` before searching")
            }
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::InvalidQuery(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Property(error) => write!(formatter, "{error}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
            Self::Vector(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for SearchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Property(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Vector(error) => Some(error),
            Self::CacheMissing | Self::InvalidQuery(_) => None,
        }
    }
}

impl From<CacheError> for SearchError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<PropertyError> for SearchError {
    fn from(error: PropertyError) -> Self {
        Self::Property(error)
    }
}

impl From<serde_json::Error> for SearchError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<rusqlite::Error> for SearchError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<crate::VectorError> for SearchError {
    fn from(error: crate::VectorError) -> Self {
        Self::Vector(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Keyword,
    Hybrid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    pub tag: Option<String>,
    pub path_prefix: Option<String>,
    pub has_property: Option<String>,
    #[serde(default)]
    pub filters: Vec<String>,
    pub provider: Option<String>,
    pub mode: SearchMode,
    pub limit: Option<usize>,
    pub context_size: usize,
    #[serde(default)]
    pub raw_query: bool,
    #[serde(default)]
    pub fuzzy: bool,
    #[serde(default)]
    pub explain: bool,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            tag: None,
            path_prefix: None,
            has_property: None,
            filters: Vec::new(),
            provider: None,
            mode: SearchMode::Keyword,
            limit: None,
            context_size: 18,
            raw_query: false,
            fuzzy: false,
            explain: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchPlan {
    pub effective_query: String,
    pub semantic_text: String,
    pub tag: Option<String>,
    pub path_prefix: Option<String>,
    pub has_property: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub property_filters: Vec<String>,
    pub raw_query: bool,
    pub fuzzy_requested: bool,
    pub fuzzy_fallback_used: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parsed_query_explanation: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fuzzy_expansions: Vec<SearchFuzzyExpansion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchFuzzyExpansion {
    pub term: String,
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHitExplain {
    pub strategy: String,
    pub effective_query: String,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyword_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyword_contribution: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_contribution: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchReport {
    pub query: String,
    pub mode: SearchMode,
    pub tag: Option<String>,
    pub path_prefix: Option<String>,
    pub has_property: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<SearchPlan>,
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaticSearchIndexReport {
    pub version: u32,
    pub documents: usize,
    pub chunks: usize,
    pub entries: Vec<StaticSearchIndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaticSearchIndexEntry {
    pub document_path: String,
    pub chunk_id: String,
    pub sequence_index: usize,
    pub heading_path: Vec<String>,
    pub content: String,
    pub document_title: String,
    pub aliases_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    pub document_path: String,
    pub chunk_id: String,
    pub heading_path: Vec<String>,
    pub snippet: String,
    pub rank: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<SearchHitExplain>,
}

#[derive(Debug, Clone)]
struct PreparedSearchQuery {
    effective_query: String,
    semantic_text: String,
    tag: Option<String>,
    path_prefix: Option<String>,
    has_property: Option<String>,
    filters: Vec<String>,
    raw_query: bool,
    fuzzy_requested: bool,
    fuzzy_fallback_used: bool,
    fuzzy_expansions: Vec<SearchFuzzyExpansion>,
    expression: Option<SearchExpr>,
}

#[derive(Debug, Clone)]
enum SearchExpr {
    Term(SearchTerm),
    And(Vec<SearchExpr>),
    Or(Vec<SearchExpr>),
    Not(Box<SearchExpr>),
}

#[derive(Debug, Clone)]
struct SearchTerm {
    text: String,
    quoted: bool,
}

#[derive(Debug, Clone)]
enum LexedToken {
    Term(SearchTerm),
    Negation,
    OpenParen,
    CloseParen,
}

#[derive(Debug, Default)]
struct InlineFilterState {
    tag: Option<String>,
    path_prefix: Option<String>,
    has_property: Option<String>,
    positive_terms: usize,
}

pub fn search_vault(paths: &VaultPaths, query: &SearchQuery) -> Result<SearchReport, SearchError> {
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let mut prepared = prepare_search_query(query)?;
    let mut hits = execute_search(paths, connection, query, &prepared)?;

    if hits.is_empty() && query.fuzzy && !query.raw_query {
        let expansions = fuzzy_expansions(connection, &prepared)?;
        if !expansions.is_empty() {
            prepared.apply_fuzzy_expansions(expansions);
            hits = execute_search(paths, connection, query, &prepared)?;
        }
    }

    Ok(SearchReport {
        query: query.text.clone(),
        mode: query.mode,
        tag: prepared.tag.clone(),
        path_prefix: prepared.path_prefix.clone(),
        has_property: prepared.has_property.clone(),
        filters: prepared.filters.clone(),
        plan: query.explain.then(|| prepared.plan()),
        hits,
    })
}

pub fn export_static_search_index(
    paths: &VaultPaths,
) -> Result<StaticSearchIndexReport, SearchError> {
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let entries = load_static_search_index_entries(connection)?;
    let documents = entries
        .iter()
        .map(|entry| entry.document_path.as_str())
        .collect::<HashSet<_>>()
        .len();

    Ok(StaticSearchIndexReport {
        version: 1,
        documents,
        chunks: entries.len(),
        entries,
    })
}

fn open_existing_cache(paths: &VaultPaths) -> Result<CacheDatabase, SearchError> {
    if !paths.cache_db().exists() {
        return Err(SearchError::CacheMissing);
    }

    CacheDatabase::open(paths).map_err(SearchError::from)
}

fn load_static_search_index_entries(
    connection: &Connection,
) -> Result<Vec<StaticSearchIndexEntry>, SearchError> {
    let mut statement = connection.prepare(
        "
        SELECT
            documents.path,
            chunks.id,
            chunks.sequence_index,
            chunks.heading_path,
            search_chunk_content.content,
            search_chunk_content.document_title,
            search_chunk_content.aliases
        FROM search_chunk_content
        JOIN chunks ON chunks.id = search_chunk_content.chunk_id
        JOIN documents ON documents.id = search_chunk_content.document_id
        ORDER BY documents.path, chunks.sequence_index
        ",
    )?;

    let rows = statement.query_map([], |row| {
        let heading_path =
            serde_json::from_str::<Vec<String>>(&row.get::<_, String>(3)?).unwrap_or_default();
        Ok(StaticSearchIndexEntry {
            document_path: row.get(0)?,
            chunk_id: row.get(1)?,
            sequence_index: row.get(2)?,
            heading_path,
            content: row.get(4)?,
            document_title: row.get(5)?,
            aliases_text: row.get(6)?,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SearchError::from)
}

fn execute_search(
    paths: &VaultPaths,
    connection: &Connection,
    query: &SearchQuery,
    prepared: &PreparedSearchQuery,
) -> Result<Vec<SearchHit>, SearchError> {
    match query.mode {
        SearchMode::Keyword => keyword_search_hits(connection, query, prepared, query.limit),
        SearchMode::Hybrid => hybrid_search_hits(paths, connection, query, prepared),
    }
}

#[allow(clippy::too_many_lines)]
fn keyword_search_hits(
    connection: &Connection,
    query: &SearchQuery,
    prepared: &PreparedSearchQuery,
    limit_override: Option<usize>,
) -> Result<Vec<SearchHit>, SearchError> {
    let limit = limit_override
        .or(query.limit)
        .map_or(i64::MAX, |value| i64::try_from(value).unwrap_or(i64::MAX));
    let path_filter = prepared
        .path_prefix
        .as_deref()
        .map_or(SqlValue::Null, |prefix| {
            SqlValue::Text(format!("{prefix}%"))
        });
    let tag = prepared.tag.clone().map_or(SqlValue::Null, SqlValue::Text);
    let has_property = prepared
        .has_property
        .clone()
        .map_or(SqlValue::Null, SqlValue::Text);
    let filter_sql = build_note_filter_clause(&prepared.filters)?;
    let mut sql = filter_sql.cte;
    sql.push_str(
        "SELECT
            documents.path,
            chunks.id,
            chunks.heading_path,
            snippet(search_chunks_fts, 0, '[', ']', '…', ?6),
            bm25(search_chunks_fts, 10.0, 5.0, 3.0, 2.0)
        FROM search_chunks_fts
        JOIN search_chunk_content ON search_chunks_fts.rowid = search_chunk_content.id
        JOIN chunks ON chunks.id = search_chunk_content.chunk_id
        JOIN documents ON documents.id = search_chunk_content.document_id
        WHERE search_chunks_fts MATCH ?1
          AND (?2 IS NULL OR documents.path LIKE ?2)
          AND (
                ?3 IS NULL
                OR EXISTS (
                    SELECT 1
                    FROM tags
                    WHERE tags.document_id = documents.id
                      AND tags.tag_text = ?3
                )
          )
          AND (
                ?4 IS NULL
                OR EXISTS (
                    SELECT 1
                    FROM property_values
                    WHERE property_values.document_id = documents.id
                      AND property_values.key = ?4
                )
          )",
    );
    sql.push_str(&filter_sql.clause);
    sql.push_str(
        " ORDER BY 5 ASC, documents.path ASC, chunks.sequence_index ASC
        LIMIT ?5",
    );

    let mut params = vec![
        SqlValue::Text(prepared.effective_query.clone()),
        path_filter,
        tag,
        has_property,
        SqlValue::Integer(limit),
        SqlValue::Integer(i64::try_from(query.context_size).unwrap_or(i64::MAX)),
    ];
    params.append(&mut filter_sql.params.clone());

    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        params_from_iter(params.iter()),
        |row| -> Result<SearchHit, rusqlite::Error> {
            let heading_path = row.get::<_, String>(2)?;
            Ok(SearchHit {
                document_path: row.get(0)?,
                chunk_id: row.get(1)?,
                heading_path: serde_json::from_str(&heading_path).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                snippet: row.get(3)?,
                rank: row.get(4)?,
                explain: None,
            })
        },
    )?;
    let mut hits = rows.collect::<Result<Vec<_>, _>>()?;

    if query.explain {
        for (index, hit) in hits.iter_mut().enumerate() {
            hit.explain = Some(SearchHitExplain {
                strategy: "keyword".to_string(),
                effective_query: prepared.effective_query.clone(),
                score: hit.rank,
                bm25: Some(hit.rank),
                keyword_rank: Some(index + 1),
                keyword_contribution: None,
                vector_rank: None,
                vector_contribution: None,
            });
        }
    }

    Ok(hits)
}

fn hybrid_search_hits(
    paths: &VaultPaths,
    connection: &Connection,
    query: &SearchQuery,
    prepared: &PreparedSearchQuery,
) -> Result<Vec<SearchHit>, SearchError> {
    let requested_limit = query.limit.unwrap_or(10).max(1);
    let candidate_limit = requested_limit.saturating_mul(4).max(10);
    let keyword_hits = keyword_search_hits(connection, query, prepared, Some(candidate_limit))?;
    let vector_hits = query_hybrid_candidates(
        paths,
        query.provider.as_deref(),
        &prepared.semantic_text,
        candidate_limit,
    )?;
    let filtered_paths = matching_note_paths(connection, &prepared.filters)?;
    let filtered_vector_hits =
        batch_filter_vector_hits(connection, vector_hits, prepared, filtered_paths.as_ref())?;

    let mut combined = HashMap::<String, SearchHit>::new();
    let mut scores = HashMap::<String, f64>::new();
    let mut keyword_positions = HashMap::<String, usize>::new();
    let mut keyword_contributions = HashMap::<String, f64>::new();
    let mut vector_positions = HashMap::<String, usize>::new();
    let mut vector_contributions = HashMap::<String, f64>::new();

    for (index, hit) in keyword_hits.iter().enumerate() {
        let score = reciprocal_rank(index);
        scores
            .entry(hit.chunk_id.clone())
            .and_modify(|current| *current += score)
            .or_insert(score);
        combined
            .entry(hit.chunk_id.clone())
            .or_insert_with(|| hit.clone());
        keyword_positions.insert(hit.chunk_id.clone(), index + 1);
        keyword_contributions.insert(hit.chunk_id.clone(), score);
    }

    for (index, hit) in filtered_vector_hits.iter().enumerate() {
        let score = reciprocal_rank(index);
        scores
            .entry(hit.chunk_id.clone())
            .and_modify(|current| *current += score)
            .or_insert(score);
        combined
            .entry(hit.chunk_id.clone())
            .or_insert_with(|| SearchHit {
                document_path: hit.document_path.clone(),
                chunk_id: hit.chunk_id.clone(),
                heading_path: hit.heading_path.clone(),
                snippet: hit.snippet.clone(),
                rank: 0.0,
                explain: None,
            });
        vector_positions.insert(hit.chunk_id.clone(), index + 1);
        vector_contributions.insert(hit.chunk_id.clone(), score);
    }

    let mut hits = combined
        .into_iter()
        .map(|(chunk_id, mut hit)| {
            hit.rank = scores.get(&chunk_id).copied().unwrap_or_default();
            if query.explain {
                hit.explain = Some(SearchHitExplain {
                    strategy: "hybrid".to_string(),
                    effective_query: prepared.effective_query.clone(),
                    score: hit.rank,
                    bm25: None,
                    keyword_rank: keyword_positions.get(&chunk_id).copied(),
                    keyword_contribution: keyword_contributions.get(&chunk_id).copied(),
                    vector_rank: vector_positions.get(&chunk_id).copied(),
                    vector_contribution: vector_contributions.get(&chunk_id).copied(),
                });
            }
            hit
        })
        .collect::<Vec<_>>();
    hits.sort_by(|left, right| {
        right
            .rank
            .partial_cmp(&left.rank)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.document_path.cmp(&right.document_path))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    hits.truncate(requested_limit);
    Ok(hits)
}

fn reciprocal_rank(index: usize) -> f64 {
    1.0 / (60.0 + f64::from(u32::try_from(index).unwrap_or(u32::MAX)) + 1.0)
}

fn batch_filter_vector_hits(
    connection: &Connection,
    vector_hits: Vec<crate::vector::VectorNeighborHit>,
    prepared: &PreparedSearchQuery,
    filtered_paths: Option<&HashSet<String>>,
) -> Result<Vec<crate::vector::VectorNeighborHit>, SearchError> {
    // Apply path_prefix and filtered_paths inline — no DB needed
    let mut candidates: Vec<crate::vector::VectorNeighborHit> = vector_hits
        .into_iter()
        .filter(|hit| {
            if let Some(prefix) = prepared.path_prefix.as_deref() {
                if !hit.document_path.starts_with(prefix) {
                    return false;
                }
            }
            if let Some(paths) = filtered_paths {
                if !paths.contains(&hit.document_path) {
                    return false;
                }
            }
            true
        })
        .collect();

    if candidates.is_empty() {
        return Ok(candidates);
    }

    // If neither tag nor property filter is active, no DB queries needed
    if prepared.tag.is_none() && prepared.has_property.is_none() {
        return Ok(candidates);
    }

    // Batch query: get document IDs for all candidate paths in one query
    let placeholders = candidates
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT path, id FROM documents WHERE path IN ({placeholders})");
    let path_params: Vec<&dyn rusqlite::types::ToSql> = candidates
        .iter()
        .map(|hit| &hit.document_path as &dyn rusqlite::types::ToSql)
        .collect();
    let mut stmt = connection.prepare(&sql)?;
    let path_to_id: HashMap<String, String> = stmt
        .query_map(path_params.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<_, _>>()?;

    // Collect the document IDs that passed the path lookup
    let doc_ids: Vec<&String> = candidates
        .iter()
        .filter_map(|hit| path_to_id.get(&hit.document_path))
        .collect();

    if doc_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Build set of passing document IDs (starts with all that have a known path)
    let mut passing_ids: HashSet<&str> = doc_ids.iter().map(|id| id.as_str()).collect();

    // Batch tag filter
    if let Some(tag) = prepared.tag.as_deref() {
        let id_placeholders = (1..=doc_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let tag_sql = format!(
            "SELECT DISTINCT document_id FROM tags WHERE document_id IN ({id_placeholders}) AND tag_text = ?{}",
            doc_ids.len() + 1
        );
        let mut tag_params: Vec<&dyn rusqlite::types::ToSql> = doc_ids
            .iter()
            .map(|id| *id as &dyn rusqlite::types::ToSql)
            .collect();
        tag_params.push(&tag);
        let mut tag_stmt = connection.prepare(&tag_sql)?;
        let tagged_ids: HashSet<String> = tag_stmt
            .query_map(tag_params.as_slice(), |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        passing_ids.retain(|id| tagged_ids.contains(*id));
    }

    // Batch property filter
    if let Some(property_key) = prepared.has_property.as_deref() {
        let remaining_ids: Vec<&&str> = passing_ids.iter().collect();
        if remaining_ids.is_empty() {
            return Ok(Vec::new());
        }
        let id_placeholders = (1..=remaining_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let prop_sql = format!(
            "SELECT DISTINCT document_id FROM property_values WHERE document_id IN ({id_placeholders}) AND key = ?{}",
            remaining_ids.len() + 1
        );
        let mut prop_params: Vec<&dyn rusqlite::types::ToSql> = remaining_ids
            .iter()
            .map(|id| &**id as &dyn rusqlite::types::ToSql)
            .collect();
        prop_params.push(&property_key);
        let mut prop_stmt = connection.prepare(&prop_sql)?;
        let property_ids: HashSet<String> = prop_stmt
            .query_map(prop_params.as_slice(), |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        passing_ids.retain(|id| property_ids.contains(*id));
    }

    // Filter candidates to those whose document_id passed all filters
    candidates.retain(|hit| {
        path_to_id
            .get(&hit.document_path)
            .is_some_and(|id| passing_ids.contains(id.as_str()))
    });

    Ok(candidates)
}

fn matching_note_paths(
    connection: &Connection,
    filters: &[String],
) -> Result<Option<HashSet<String>>, SearchError> {
    if filters.is_empty() {
        return Ok(None);
    }
    let filter_sql = build_note_filter_clause(filters)?;
    let mut sql = filter_sql.cte;
    sql.push_str(
        "SELECT documents.path
        FROM documents
        LEFT JOIN properties ON properties.document_id = documents.id
        WHERE documents.extension = 'md'",
    );
    sql.push_str(&filter_sql.clause);
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(filter_sql.params.iter()), |row| {
        row.get::<_, String>(0)
    })?;
    Ok(Some(rows.collect::<Result<HashSet<_>, _>>()?))
}

fn prepare_search_query(query: &SearchQuery) -> Result<PreparedSearchQuery, SearchError> {
    let trimmed = query.text.trim();
    if trimmed.is_empty() {
        return Err(SearchError::InvalidQuery(
            "search query must not be empty".to_string(),
        ));
    }
    if query.raw_query {
        return Ok(PreparedSearchQuery {
            effective_query: trimmed.to_string(),
            semantic_text: trimmed.to_string(),
            tag: query.tag.clone(),
            path_prefix: query.path_prefix.clone(),
            has_property: query.has_property.clone(),
            filters: query.filters.clone(),
            raw_query: true,
            fuzzy_requested: query.fuzzy,
            fuzzy_fallback_used: false,
            fuzzy_expansions: Vec::new(),
            expression: None,
        });
    }

    let tokens = lex_search_query(trimmed);
    let expression = parse_search_expression(&tokens)?;
    let mut filter_state = InlineFilterState::default();
    let filtered_expression = extract_inline_filters(expression, &mut filter_state, false);

    if filter_state.positive_terms == 0 {
        return Err(SearchError::InvalidQuery(
            "search query must contain at least one term or quoted phrase".to_string(),
        ));
    }

    let expression = filtered_expression.ok_or_else(|| {
        SearchError::InvalidQuery(
            "search query must contain at least one term or quoted phrase".to_string(),
        )
    })?;
    let effective_query = compose_fts_query(&expression, &HashMap::new())?;
    let semantic_text = semantic_terms(&expression);

    Ok(PreparedSearchQuery {
        effective_query,
        semantic_text,
        tag: query.tag.clone().or(filter_state.tag),
        path_prefix: query.path_prefix.clone().or(filter_state.path_prefix),
        has_property: query.has_property.clone().or(filter_state.has_property),
        filters: query.filters.clone(),
        raw_query: false,
        fuzzy_requested: query.fuzzy,
        fuzzy_fallback_used: false,
        fuzzy_expansions: Vec::new(),
        expression: Some(expression),
    })
}

fn lex_search_query(text: &str) -> Vec<LexedToken> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0_usize;

    while index < characters.len() {
        while index < characters.len() && characters[index].is_whitespace() {
            index += 1;
        }
        if index >= characters.len() {
            break;
        }

        match characters[index] {
            '(' => {
                tokens.push(LexedToken::OpenParen);
                index += 1;
                continue;
            }
            ')' => {
                tokens.push(LexedToken::CloseParen);
                index += 1;
                continue;
            }
            '-' if index + 1 < characters.len() && !characters[index + 1].is_whitespace() => {
                tokens.push(LexedToken::Negation);
                index += 1;
                continue;
            }
            _ => {}
        }

        if characters[index] == '"' {
            index += 1;
            let start = index;
            while index < characters.len() && characters[index] != '"' {
                index += 1;
            }
            let text = characters[start..index].iter().collect::<String>();
            if index < characters.len() && characters[index] == '"' {
                index += 1;
            }
            tokens.push(LexedToken::Term(SearchTerm { text, quoted: true }));
            continue;
        }

        let start = index;
        while index < characters.len()
            && !characters[index].is_whitespace()
            && !matches!(characters[index], '(' | ')')
        {
            index += 1;
        }
        let text = characters[start..index].iter().collect::<String>();
        tokens.push(LexedToken::Term(SearchTerm {
            text,
            quoted: false,
        }));
    }

    tokens
}

fn inline_filter_value<'a>(token: &'a str, key: &str) -> Option<&'a str> {
    let (prefix, value) = token.split_once(':')?;
    if prefix.eq_ignore_ascii_case(key) && !value.trim().is_empty() {
        Some(value.trim())
    } else {
        None
    }
}

fn quote_fts_term(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

fn parse_search_expression(tokens: &[LexedToken]) -> Result<SearchExpr, SearchError> {
    let mut index = 0_usize;
    let expression = parse_or_expression(tokens, &mut index)?;
    if index != tokens.len() {
        return Err(SearchError::InvalidQuery(
            "unexpected token after end of search expression".to_string(),
        ));
    }
    Ok(expression)
}

fn parse_or_expression(
    tokens: &[LexedToken],
    index: &mut usize,
) -> Result<SearchExpr, SearchError> {
    let mut branches = vec![parse_and_expression(tokens, index)?];

    while *index < tokens.len() && is_or_token(&tokens[*index]) {
        *index += 1;
        branches.push(parse_and_expression(tokens, index)?);
    }

    Ok(collapse_expression(branches, SearchExpr::Or))
}

fn parse_and_expression(
    tokens: &[LexedToken],
    index: &mut usize,
) -> Result<SearchExpr, SearchError> {
    let mut factors = Vec::new();

    while *index < tokens.len() {
        if matches!(tokens[*index], LexedToken::CloseParen) || is_or_token(&tokens[*index]) {
            break;
        }
        factors.push(parse_unary_expression(tokens, index)?);
    }

    if factors.is_empty() {
        return Err(SearchError::InvalidQuery(
            "expected a search term or group".to_string(),
        ));
    }

    Ok(collapse_expression(factors, SearchExpr::And))
}

fn parse_unary_expression(
    tokens: &[LexedToken],
    index: &mut usize,
) -> Result<SearchExpr, SearchError> {
    let mut negated = false;
    while *index < tokens.len() && matches!(tokens[*index], LexedToken::Negation) {
        negated = !negated;
        *index += 1;
    }

    let expression = parse_primary_expression(tokens, index)?;
    if negated {
        Ok(SearchExpr::Not(Box::new(expression)))
    } else {
        Ok(expression)
    }
}

fn parse_primary_expression(
    tokens: &[LexedToken],
    index: &mut usize,
) -> Result<SearchExpr, SearchError> {
    let token = tokens
        .get(*index)
        .ok_or_else(|| SearchError::InvalidQuery("expected a search term or group".to_string()))?;

    match token {
        LexedToken::Term(term) => {
            *index += 1;
            Ok(SearchExpr::Term(term.clone()))
        }
        LexedToken::OpenParen => {
            *index += 1;
            let expression = parse_or_expression(tokens, index)?;
            if !matches!(tokens.get(*index), Some(LexedToken::CloseParen)) {
                return Err(SearchError::InvalidQuery(
                    "missing closing ')' in search query".to_string(),
                ));
            }
            *index += 1;
            Ok(expression)
        }
        LexedToken::CloseParen => Err(SearchError::InvalidQuery(
            "unexpected ')' in search query".to_string(),
        )),
        LexedToken::Negation => Err(SearchError::InvalidQuery(
            "dangling negation in search query".to_string(),
        )),
    }
}

fn collapse_expression(
    mut expressions: Vec<SearchExpr>,
    make_group: impl FnOnce(Vec<SearchExpr>) -> SearchExpr,
) -> SearchExpr {
    if expressions.len() == 1 {
        expressions.remove(0)
    } else {
        make_group(expressions)
    }
}

fn is_or_token(token: &LexedToken) -> bool {
    matches!(
        token,
        LexedToken::Term(SearchTerm {
            text,
            quoted: false
        }) if text.eq_ignore_ascii_case("or")
    )
}

fn extract_inline_filters(
    expression: SearchExpr,
    state: &mut InlineFilterState,
    negated: bool,
) -> Option<SearchExpr> {
    match expression {
        SearchExpr::Term(term) => {
            if !negated && !term.quoted {
                if let Some(value) = inline_filter_value(&term.text, "tag") {
                    state.tag = Some(value.to_string());
                    return None;
                }
                if let Some(value) = inline_filter_value(&term.text, "path") {
                    state.path_prefix = Some(value.to_string());
                    return None;
                }
                if let Some(value) = inline_filter_value(&term.text, "has")
                    .or_else(|| inline_filter_value(&term.text, "property"))
                {
                    state.has_property = Some(value.to_string());
                    return None;
                }
            }
            if !negated {
                state.positive_terms += 1;
            }
            Some(SearchExpr::Term(term))
        }
        SearchExpr::And(children) => {
            collapse_rewritten_group(children, state, negated, SearchExpr::And)
        }
        SearchExpr::Or(children) => {
            collapse_rewritten_group(children, state, negated, SearchExpr::Or)
        }
        SearchExpr::Not(child) => extract_inline_filters(*child, state, !negated)
            .map(|rewritten| SearchExpr::Not(Box::new(rewritten))),
    }
}

fn collapse_rewritten_group(
    children: Vec<SearchExpr>,
    state: &mut InlineFilterState,
    negated: bool,
    make_group: impl FnOnce(Vec<SearchExpr>) -> SearchExpr,
) -> Option<SearchExpr> {
    let rewritten = children
        .into_iter()
        .filter_map(|child| extract_inline_filters(child, state, negated))
        .collect::<Vec<_>>();

    match rewritten.len() {
        0 => None,
        1 => rewritten.into_iter().next(),
        _ => Some(make_group(rewritten)),
    }
}

fn render_fts_expression(
    expression: &SearchExpr,
    fuzzy_map: &HashMap<String, Vec<String>>,
    parent_precedence: u8,
) -> String {
    let (rendered, precedence) = match expression {
        SearchExpr::Term(term) => (render_fts_term(term, fuzzy_map), 4),
        SearchExpr::Not(child) => (
            format!("NOT {}", render_fts_expression(child, fuzzy_map, 3)),
            3,
        ),
        SearchExpr::And(children) => (render_fts_and_children(children, fuzzy_map), 2),
        SearchExpr::Or(children) => (
            children
                .iter()
                .map(|child| render_fts_expression(child, fuzzy_map, 1))
                .collect::<Vec<_>>()
                .join(" OR "),
            1,
        ),
    };

    if precedence < parent_precedence {
        format!("({rendered})")
    } else {
        rendered
    }
}

fn render_fts_term(term: &SearchTerm, fuzzy_map: &HashMap<String, Vec<String>>) -> String {
    let base = quote_fts_term(&term.text);
    if term.quoted {
        return base;
    }
    let Some(normalized) = normalize_term(&term.text) else {
        return base;
    };
    let Some(expansions) = fuzzy_map
        .get(&normalized)
        .filter(|values| !values.is_empty())
    else {
        return base;
    };

    let mut variants = Vec::with_capacity(expansions.len() + 1);
    variants.push(base);
    variants.extend(expansions.iter().map(|value| quote_fts_term(value)));
    format!("({})", variants.join(" OR "))
}

fn render_fts_and_children(
    children: &[SearchExpr],
    fuzzy_map: &HashMap<String, Vec<String>>,
) -> String {
    let mut rendered = String::new();

    for (index, child) in children.iter().enumerate() {
        let piece = render_fts_expression(child, fuzzy_map, 2);
        if index > 0 {
            if matches!(child, SearchExpr::Not(_)) {
                rendered.push(' ');
            } else {
                rendered.push_str(" AND ");
            }
        }
        rendered.push_str(&piece);
    }

    rendered
}

fn semantic_terms(expression: &SearchExpr) -> String {
    let mut terms = Vec::new();
    collect_semantic_terms(expression, false, &mut terms);
    terms.join(" ")
}

fn collect_semantic_terms(expression: &SearchExpr, negated: bool, terms: &mut Vec<String>) {
    match expression {
        SearchExpr::Term(term) if !negated => terms.push(term.text.clone()),
        SearchExpr::Term(_) => {}
        SearchExpr::And(children) | SearchExpr::Or(children) => {
            for child in children {
                collect_semantic_terms(child, negated, terms);
            }
        }
        SearchExpr::Not(child) => collect_semantic_terms(child, !negated, terms),
    }
}

fn explain_search_expression(expression: &SearchExpr) -> Vec<String> {
    let mut lines = Vec::new();
    append_expression_lines(expression, 0, &mut lines);
    lines
}

fn append_expression_lines(expression: &SearchExpr, indent: usize, lines: &mut Vec<String>) {
    let prefix = "  ".repeat(indent);
    match expression {
        SearchExpr::Term(term) => {
            lines.push(format!("{prefix}TERM {}", display_search_term(term)));
        }
        SearchExpr::And(children) => {
            lines.push(format!("{prefix}AND"));
            for child in children {
                append_expression_lines(child, indent + 1, lines);
            }
        }
        SearchExpr::Or(children) => {
            lines.push(format!("{prefix}OR"));
            for child in children {
                append_expression_lines(child, indent + 1, lines);
            }
        }
        SearchExpr::Not(child) => {
            lines.push(format!("{prefix}NOT"));
            append_expression_lines(child, indent + 1, lines);
        }
    }
}

fn display_search_term(term: &SearchTerm) -> String {
    if term.quoted {
        format!("\"{}\"", term.text)
    } else {
        term.text.clone()
    }
}

fn compose_fts_query(
    expression: &SearchExpr,
    fuzzy_map: &HashMap<String, Vec<String>>,
) -> Result<String, SearchError> {
    let rendered = render_fts_expression(expression, fuzzy_map, 0);
    if rendered.trim().is_empty() {
        return Err(SearchError::InvalidQuery(
            "search query must contain at least one term or quoted phrase".to_string(),
        ));
    }

    Ok(rendered)
}

fn fuzzy_expansions(
    connection: &Connection,
    prepared: &PreparedSearchQuery,
) -> Result<Vec<SearchFuzzyExpansion>, SearchError> {
    let vocabulary = load_search_vocabulary(connection)?;
    let mut expansions = Vec::new();
    let mut seen = HashSet::new();
    if let Some(expression) = prepared.expression.as_ref() {
        collect_fuzzy_expansions(expression, &mut seen, &mut expansions, &vocabulary, false);
    }

    Ok(expansions)
}

fn collect_fuzzy_expansions(
    expression: &SearchExpr,
    seen: &mut HashSet<String>,
    expansions: &mut Vec<SearchFuzzyExpansion>,
    vocabulary: &BTreeSet<String>,
    negated: bool,
) {
    match expression {
        SearchExpr::Term(term) if !negated && !term.quoted => {
            let Some(normalized) = normalize_term(&term.text) else {
                return;
            };
            if !seen.insert(normalized.clone()) {
                return;
            }
            let candidates = fuzzy_candidates(vocabulary, &normalized);
            if !candidates.is_empty() {
                expansions.push(SearchFuzzyExpansion {
                    term: term.text.clone(),
                    candidates,
                });
            }
        }
        SearchExpr::Term(_) => {}
        SearchExpr::And(children) | SearchExpr::Or(children) => {
            for child in children {
                collect_fuzzy_expansions(child, seen, expansions, vocabulary, negated);
            }
        }
        SearchExpr::Not(child) => {
            collect_fuzzy_expansions(child, seen, expansions, vocabulary, !negated);
        }
    }
}

fn load_search_vocabulary(connection: &Connection) -> Result<BTreeSet<String>, SearchError> {
    let mut statement = connection.prepare(
        "
        SELECT content, document_title, aliases, headings
        FROM search_chunk_content
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut vocabulary = BTreeSet::new();
    for row in rows {
        let (content, title, aliases, headings) = row?;
        for field in [content, title, aliases, headings] {
            for token in tokenize_search_text(&field) {
                vocabulary.insert(token);
            }
        }
    }
    Ok(vocabulary)
}

fn tokenize_search_text(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for character in text.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            if current.chars().count() >= 3 {
                tokens.push(current.clone());
            }
            current.clear();
        }
    }
    if !current.is_empty() && current.chars().count() >= 3 {
        tokens.push(current);
    }

    tokens
}

fn normalize_term(term: &str) -> Option<String> {
    let normalized = term
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    (normalized.chars().count() >= 4).then_some(normalized)
}

fn fuzzy_candidates(vocabulary: &BTreeSet<String>, term: &str) -> Vec<String> {
    let max_distance = match term.chars().count() {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    };
    let mut matches = vocabulary
        .iter()
        .filter(|candidate| candidate.as_str() != term)
        .filter(|candidate| candidate.chars().next() == term.chars().next())
        .filter_map(|candidate| {
            let length_gap = candidate.chars().count().abs_diff(term.chars().count());
            if length_gap > max_distance {
                return None;
            }
            let distance = levenshtein(term, candidate);
            (distance <= max_distance).then_some((distance, candidate.clone()))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.len().cmp(&right.1.len()))
            .then(left.1.cmp(&right.1))
    });
    matches.truncate(4);
    matches.into_iter().map(|(_, value)| value).collect()
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

impl PreparedSearchQuery {
    fn apply_fuzzy_expansions(&mut self, expansions: Vec<SearchFuzzyExpansion>) {
        let fuzzy_map = expansions
            .iter()
            .map(|expansion| {
                (
                    normalize_term(&expansion.term)
                        .unwrap_or_else(|| expansion.term.to_lowercase()),
                    expansion.candidates.clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        if let Some(expression) = self.expression.as_ref() {
            self.effective_query = compose_fts_query(expression, &fuzzy_map)
                .expect("prepared query should remain valid after fuzzy expansion");
        }
        self.fuzzy_fallback_used = true;
        self.fuzzy_expansions = expansions;
    }

    fn plan(&self) -> SearchPlan {
        let mut parsed_query_explanation = self.expression.as_ref().map_or_else(
            || vec![format!("RAW FTS5: {}", self.effective_query)],
            explain_search_expression,
        );
        if let Some(tag) = self.tag.as_deref() {
            parsed_query_explanation.push(format!("FILTER tag:{tag}"));
        }
        if let Some(path_prefix) = self.path_prefix.as_deref() {
            parsed_query_explanation.push(format!("FILTER path:{path_prefix}"));
        }
        if let Some(has_property) = self.has_property.as_deref() {
            parsed_query_explanation.push(format!("FILTER has:{has_property}"));
        }
        parsed_query_explanation
            .extend(self.filters.iter().map(|filter| format!("WHERE {filter}")));

        SearchPlan {
            effective_query: self.effective_query.clone(),
            semantic_text: self.semantic_text.clone(),
            tag: self.tag.clone(),
            path_prefix: self.path_prefix.clone(),
            has_property: self.has_property.clone(),
            property_filters: self.filters.clone(),
            raw_query: self.raw_query,
            fuzzy_requested: self.fuzzy_requested,
            fuzzy_fallback_used: self.fuzzy_fallback_used,
            parsed_query_explanation,
            fuzzy_expansions: self.fuzzy_expansions.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan_vault, ScanMode};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn search_returns_ranked_chunk_hits_with_snippets() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = search_vault(
            &paths,
            &SearchQuery {
                text: "dashboard".to_string(),
                ..SearchQuery::default()
            },
        )
        .expect("search should succeed");

        assert_eq!(report.hits.len(), 1);
        assert_eq!(report.hits[0].document_path, "Home.md");
        assert!(report.hits[0].snippet.contains("[dashboard]"));
    }

    #[test]
    fn search_matches_aliases_and_filters_by_tag_and_path_prefix() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let alias_report = search_vault(
            &paths,
            &SearchQuery {
                text: "Robert".to_string(),
                ..SearchQuery::default()
            },
        )
        .expect("alias search should succeed");
        assert_eq!(
            alias_report
                .hits
                .iter()
                .map(|hit| hit.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["People/Bob.md".to_string()]
        );

        let tag_report = search_vault(
            &paths,
            &SearchQuery {
                text: "Owned".to_string(),
                tag: Some("project".to_string()),
                ..SearchQuery::default()
            },
        )
        .expect("tag-filtered search should succeed");
        assert_eq!(
            tag_report
                .hits
                .iter()
                .map(|hit| hit.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Projects/Alpha.md".to_string()]
        );

        let path_report = search_vault(
            &paths,
            &SearchQuery {
                text: "Bob".to_string(),
                path_prefix: Some("People/".to_string()),
                ..SearchQuery::default()
            },
        )
        .expect("path-filtered search should succeed");
        assert_eq!(
            path_report
                .hits
                .iter()
                .map(|hit| hit.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["People/Bob.md".to_string()]
        );
    }

    #[test]
    fn search_matches_extracted_attachment_text() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("attachments", &vault_root);
        write_attachment_sidecar(
            &vault_root,
            "assets/guide.pdf.txt",
            "dashboard manual reference",
        );
        write_attachment_sidecar(&vault_root, "assets/logo.png.txt", "dashboard logo");
        write_attachment_extraction_config(&vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = search_vault(
            &paths,
            &SearchQuery {
                text: "manual".to_string(),
                ..SearchQuery::default()
            },
        )
        .expect("search should succeed");

        assert!(report
            .hits
            .iter()
            .any(|hit| hit.document_path == "assets/guide.pdf"));
    }

    #[test]
    fn search_filters_by_property_presence_and_where_clauses() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("mixed-properties", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let report = search_vault(
            &paths,
            &SearchQuery {
                text: "release".to_string(),
                has_property: Some("empty_text".to_string()),
                filters: vec!["reviewed = true".to_string()],
                ..SearchQuery::default()
            },
        )
        .expect("property-filtered search should succeed");
        assert_eq!(
            report
                .hits
                .iter()
                .map(|hit| hit.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Done.md".to_string()]
        );
    }

    #[test]
    fn static_search_index_export_includes_chunk_content_and_headings() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = export_static_search_index(&paths).expect("export should succeed");

        assert_eq!(report.version, 1);
        assert_eq!(report.documents, 3);
        assert_eq!(report.chunks, report.entries.len());
        assert!(report.entries.iter().any(|entry| {
            entry.document_path == "Home.md"
                && entry.heading_path == vec!["Home".to_string()]
                && entry.content.contains("dashboard")
        }));
    }

    #[test]
    fn search_parses_inline_filters_and_explain_plan() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let report = search_vault(
            &paths,
            &SearchQuery {
                text: "tag:project \"Owned by\" -Robert".to_string(),
                explain: true,
                ..SearchQuery::default()
            },
        )
        .expect("search should succeed");

        assert_eq!(report.tag.as_deref(), Some("project"));
        assert_eq!(report.hits[0].document_path, "Projects/Alpha.md");
        let plan = report.plan.expect("plan should be populated");
        assert_eq!(plan.effective_query, "\"Owned by\" NOT \"Robert\"");
        assert!(plan
            .parsed_query_explanation
            .iter()
            .any(|line| line == "AND"));
        assert!(plan
            .parsed_query_explanation
            .iter()
            .any(|line| line == "FILTER tag:project"));
        assert!(!report.hits[0]
            .explain
            .as_ref()
            .expect("hit explain should be populated")
            .strategy
            .is_empty());
    }

    #[test]
    fn search_parenthesized_grouping_preserves_or_scope() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(&vault_root).expect("vault root should exist");
        fs::write(vault_root.join("Alpha.md"), "alpha project").expect("note should write");
        fs::write(vault_root.join("Beta.md"), "beta project").expect("note should write");
        fs::write(vault_root.join("Gamma.md"), "alpha only").expect("note should write");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = search_vault(
            &paths,
            &SearchQuery {
                text: "(alpha or beta) project".to_string(),
                explain: true,
                ..SearchQuery::default()
            },
        )
        .expect("search should succeed");

        let mut hit_paths = report
            .hits
            .iter()
            .map(|hit| hit.document_path.clone())
            .collect::<Vec<_>>();
        hit_paths.sort();
        assert_eq!(
            hit_paths,
            vec!["Alpha.md".to_string(), "Beta.md".to_string()]
        );
        assert_eq!(
            report.plan.expect("plan should exist").effective_query,
            "(\"alpha\" OR \"beta\") AND \"project\""
        );
    }

    #[test]
    fn search_grouped_negation_requires_all_terms_in_negated_group() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(&vault_root).expect("vault root should exist");
        fs::write(vault_root.join("Both.md"), "alpha work meetup").expect("note should write");
        fs::write(vault_root.join("Work.md"), "alpha work").expect("note should write");
        fs::write(vault_root.join("Meetup.md"), "alpha meetup").expect("note should write");
        fs::write(vault_root.join("Other.md"), "alpha unrelated").expect("note should write");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = search_vault(
            &paths,
            &SearchQuery {
                text: "alpha -(work meetup)".to_string(),
                explain: true,
                ..SearchQuery::default()
            },
        )
        .expect("search should succeed");

        let mut hit_paths = report
            .hits
            .iter()
            .map(|hit| hit.document_path.clone())
            .collect::<Vec<_>>();
        hit_paths.sort();
        assert_eq!(
            hit_paths,
            vec![
                "Meetup.md".to_string(),
                "Other.md".to_string(),
                "Work.md".to_string(),
            ]
        );
        assert_eq!(
            report.plan.expect("plan should exist").effective_query,
            "\"alpha\" NOT (\"work\" AND \"meetup\")"
        );
    }

    #[test]
    fn search_fuzzy_fallback_matches_nearby_terms() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = search_vault(
            &paths,
            &SearchQuery {
                text: "dashbord".to_string(),
                fuzzy: true,
                explain: true,
                ..SearchQuery::default()
            },
        )
        .expect("fuzzy search should succeed");

        assert_eq!(
            report
                .hits
                .iter()
                .map(|hit| hit.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Home.md".to_string()]
        );
        let plan = report.plan.expect("plan should be present");
        assert!(plan.fuzzy_fallback_used);
        assert!(plan
            .fuzzy_expansions
            .iter()
            .any(|expansion| expansion.term == "dashbord"));
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);

        copy_dir_recursive(&source, destination);
    }

    fn write_attachment_extraction_config(vault_root: &Path) {
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[extraction]\ncommand = \"sh\"\nargs = [\"-c\", \"cat \\\"$1.txt\\\"\", \"sh\", \"{path}\"]\nextensions = [\"pdf\", \"png\"]\nmax_output_bytes = 4096\n",
        )
        .expect("config should write");
    }

    fn write_attachment_sidecar(vault_root: &Path, relative_path: &str, contents: &str) {
        let path = vault_root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("sidecar parent should exist");
        }
        fs::write(path, contents).expect("sidecar should write");
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
