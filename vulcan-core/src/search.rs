use crate::properties::build_note_filter_clause;
use crate::vector::query_hybrid_candidates;
use crate::{CacheDatabase, CacheError, PropertyError, VaultPaths};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension};
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
    atoms: Vec<QueryAtom>,
}

#[derive(Debug, Clone)]
enum QueryAtom {
    Positive {
        text: String,
        fts_expr: String,
        fuzzy_term: Option<String>,
    },
    Negative {
        fts_expr: String,
    },
    Or,
}

#[derive(Debug, Clone)]
struct LexedToken {
    text: String,
    quoted: bool,
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

fn open_existing_cache(paths: &VaultPaths) -> Result<CacheDatabase, SearchError> {
    if !paths.cache_db().exists() {
        return Err(SearchError::CacheMissing);
    }

    CacheDatabase::open(paths).map_err(SearchError::from)
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
    let (filter_clause, mut filter_params) = build_note_filter_clause(&prepared.filters)?;
    let mut sql = String::from(
        "
        SELECT
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
          )
        ",
    );
    sql.push_str(&filter_clause);
    sql.push_str(
        "
        ORDER BY 5 ASC, documents.path ASC, chunks.sequence_index ASC
        LIMIT ?5
        ",
    );

    let mut params = vec![
        SqlValue::Text(prepared.effective_query.clone()),
        path_filter,
        tag,
        has_property,
        SqlValue::Integer(limit),
        SqlValue::Integer(i64::try_from(query.context_size).unwrap_or(i64::MAX)),
    ];
    params.append(&mut filter_params);

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
    let filtered_vector_hits = vector_hits
        .into_iter()
        .filter(|hit| {
            matches_filters(
                connection,
                &hit.document_path,
                prepared,
                filtered_paths.as_ref(),
            )
            .unwrap_or(false)
        })
        .collect::<Vec<_>>();

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

fn matches_filters(
    connection: &Connection,
    document_path: &str,
    prepared: &PreparedSearchQuery,
    filtered_paths: Option<&HashSet<String>>,
) -> Result<bool, rusqlite::Error> {
    if let Some(path_prefix) = prepared.path_prefix.as_deref() {
        if !document_path.starts_with(path_prefix) {
            return Ok(false);
        }
    }
    if let Some(paths) = filtered_paths {
        if !paths.contains(document_path) {
            return Ok(false);
        }
    }

    let Some(document_id) = connection
        .query_row(
            "SELECT id FROM documents WHERE path = ?1",
            [document_path],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    if let Some(tag) = prepared.tag.as_deref() {
        let has_tag: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM tags WHERE document_id = ?1 AND tag_text = ?2)",
            params![&document_id, tag],
            |row| row.get(0),
        )?;
        if !has_tag {
            return Ok(false);
        }
    }
    if let Some(property_key) = prepared.has_property.as_deref() {
        let has_property: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM property_values WHERE document_id = ?1 AND key = ?2)",
            params![&document_id, property_key],
            |row| row.get(0),
        )?;
        if !has_property {
            return Ok(false);
        }
    }

    Ok(true)
}

fn matching_note_paths(
    connection: &Connection,
    filters: &[String],
) -> Result<Option<HashSet<String>>, SearchError> {
    if filters.is_empty() {
        return Ok(None);
    }
    let (filter_clause, params) = build_note_filter_clause(filters)?;
    let sql = format!(
        "
        SELECT documents.path
        FROM documents
        LEFT JOIN properties ON properties.document_id = documents.id
        WHERE documents.extension = 'md' {filter_clause}
        "
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(params.iter()), |row| {
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
            atoms: Vec::new(),
        });
    }

    let tokens = lex_search_query(trimmed);
    let mut atoms = Vec::new();
    let mut parsed_tag = None;
    let mut parsed_path_prefix = None;
    let mut parsed_has_property = None;
    let mut positive_terms = 0_usize;

    for token in tokens {
        if !token.quoted && token.text.eq_ignore_ascii_case("or") {
            atoms.push(QueryAtom::Or);
            continue;
        }

        let (negated, body) = token
            .text
            .strip_prefix('-')
            .map_or((false, token.text.as_str()), |value| (true, value));
        if !negated && !token.quoted {
            if let Some(value) = inline_filter_value(body, "tag") {
                parsed_tag = Some(value.to_string());
                continue;
            }
            if let Some(value) = inline_filter_value(body, "path") {
                parsed_path_prefix = Some(value.to_string());
                continue;
            }
            if let Some(value) =
                inline_filter_value(body, "has").or_else(|| inline_filter_value(body, "property"))
            {
                parsed_has_property = Some(value.to_string());
                continue;
            }
        }

        let expr = quote_fts_term(body);
        if negated {
            atoms.push(QueryAtom::Negative { fts_expr: expr });
        } else {
            positive_terms += 1;
            atoms.push(QueryAtom::Positive {
                text: body.to_string(),
                fts_expr: expr,
                fuzzy_term: (!token.quoted).then(|| normalize_term(body)).flatten(),
            });
        }
    }

    if positive_terms == 0 {
        return Err(SearchError::InvalidQuery(
            "search query must contain at least one term or quoted phrase".to_string(),
        ));
    }

    let effective_query = compose_fts_query(&atoms, &HashMap::new())?;
    let semantic_text = atoms
        .iter()
        .filter_map(|atom| match atom {
            QueryAtom::Positive { text, .. } => Some(text.clone()),
            QueryAtom::Negative { .. } | QueryAtom::Or => None,
        })
        .collect::<Vec<_>>()
        .join(" ");

    Ok(PreparedSearchQuery {
        effective_query,
        semantic_text,
        tag: query.tag.clone().or(parsed_tag),
        path_prefix: query.path_prefix.clone().or(parsed_path_prefix),
        has_property: query.has_property.clone().or(parsed_has_property),
        filters: query.filters.clone(),
        raw_query: false,
        fuzzy_requested: query.fuzzy,
        fuzzy_fallback_used: false,
        fuzzy_expansions: Vec::new(),
        atoms,
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

        let mut negated = false;
        if characters[index] == '-' {
            negated = true;
            index += 1;
        }

        if index < characters.len() && characters[index] == '"' {
            index += 1;
            let start = index;
            while index < characters.len() && characters[index] != '"' {
                index += 1;
            }
            let text = characters[start..index].iter().collect::<String>();
            if index < characters.len() && characters[index] == '"' {
                index += 1;
            }
            tokens.push(LexedToken {
                text: if negated { format!("-{text}") } else { text },
                quoted: true,
            });
            continue;
        }

        let start = index;
        while index < characters.len() && !characters[index].is_whitespace() {
            index += 1;
        }
        let text = characters[start..index].iter().collect::<String>();
        tokens.push(LexedToken {
            text: if negated { format!("-{text}") } else { text },
            quoted: false,
        });
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

fn compose_fts_query(
    atoms: &[QueryAtom],
    fuzzy_map: &HashMap<String, Vec<String>>,
) -> Result<String, SearchError> {
    let mut rendered = String::new();
    let mut last_was_operator = true;

    for atom in atoms {
        match atom {
            QueryAtom::Or => {
                if !rendered.is_empty() && !last_was_operator {
                    rendered.push_str(" OR ");
                    last_was_operator = true;
                }
            }
            QueryAtom::Negative { fts_expr } => {
                if !rendered.is_empty() {
                    rendered.push(' ');
                }
                rendered.push_str("NOT ");
                rendered.push_str(fts_expr);
                last_was_operator = false;
            }
            QueryAtom::Positive {
                fts_expr,
                fuzzy_term,
                ..
            } => {
                if !rendered.is_empty() && !last_was_operator {
                    rendered.push(' ');
                }
                if let Some(expansions) = fuzzy_term
                    .as_ref()
                    .and_then(|term| fuzzy_map.get(term))
                    .filter(|values| !values.is_empty())
                {
                    let mut variants = Vec::with_capacity(expansions.len() + 1);
                    variants.push(fts_expr.clone());
                    variants.extend(expansions.iter().map(|value| quote_fts_term(value)));
                    rendered.push('(');
                    rendered.push_str(&variants.join(" OR "));
                    rendered.push(')');
                } else {
                    rendered.push_str(fts_expr);
                }
                last_was_operator = false;
            }
        }
    }

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

    for atom in &prepared.atoms {
        let QueryAtom::Positive {
            text, fuzzy_term, ..
        } = atom
        else {
            continue;
        };
        let Some(term) = fuzzy_term.as_deref() else {
            continue;
        };
        if !seen.insert(term.to_string()) {
            continue;
        }

        let candidates = fuzzy_candidates(&vocabulary, term);
        if !candidates.is_empty() {
            expansions.push(SearchFuzzyExpansion {
                term: text.clone(),
                candidates,
            });
        }
    }

    Ok(expansions)
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
        self.effective_query = compose_fts_query(&self.atoms, &fuzzy_map)
            .expect("prepared query should remain valid after fuzzy expansion");
        self.fuzzy_fallback_used = true;
        self.fuzzy_expansions = expansions;
    }

    fn plan(&self) -> SearchPlan {
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
        assert!(!report.hits[0]
            .explain
            .as_ref()
            .expect("hit explain should be populated")
            .strategy
            .is_empty());
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
