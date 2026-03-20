use crate::{CacheDatabase, CacheError, VaultPaths};
use rusqlite::params;
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum SearchError {
    CacheMissing,
    Cache(CacheError),
    Json(serde_json::Error),
    Sqlite(rusqlite::Error),
}

impl Display for SearchError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CacheMissing => {
                formatter.write_str("cache is missing; run `vulcan scan` before searching")
            }
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for SearchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::CacheMissing => None,
        }
    }
}

impl From<CacheError> for SearchError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchQuery {
    pub text: String,
    pub tag: Option<String>,
    pub path_prefix: Option<String>,
    pub has_property: Option<String>,
    pub limit: Option<usize>,
    pub context_size: usize,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            tag: None,
            path_prefix: None,
            has_property: None,
            limit: None,
            context_size: 18,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchReport {
    pub query: String,
    pub tag: Option<String>,
    pub path_prefix: Option<String>,
    pub has_property: Option<String>,
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    pub document_path: String,
    pub chunk_id: String,
    pub heading_path: Vec<String>,
    pub snippet: String,
    pub rank: f64,
}

pub fn search_vault(paths: &VaultPaths, query: &SearchQuery) -> Result<SearchReport, SearchError> {
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let limit = query
        .limit
        .map_or(i64::MAX, |value| i64::try_from(value).unwrap_or(i64::MAX));
    let tag = query.tag.as_deref();
    let path_prefix = query.path_prefix.as_deref();
    let has_property = query.has_property.as_deref();
    let path_filter = path_prefix.map(|prefix| format!("{prefix}%"));
    let mut statement = connection.prepare(
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
        ORDER BY 5 ASC, documents.path ASC, chunks.sequence_index ASC
        LIMIT ?5
        ",
    )?;
    let rows = statement.query_map(
        params![
            query.text,
            path_filter,
            tag,
            has_property,
            limit,
            i64::try_from(query.context_size).unwrap_or(i64::MAX),
        ],
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
            })
        },
    )?;
    let hits = rows.collect::<Result<Vec<_>, _>>()?;

    Ok(SearchReport {
        query: query.text.clone(),
        tag: query.tag.clone(),
        path_prefix: query.path_prefix.clone(),
        has_property: query.has_property.clone(),
        hits,
    })
}

fn open_existing_cache(paths: &VaultPaths) -> Result<CacheDatabase, SearchError> {
    if !paths.cache_db().exists() {
        return Err(SearchError::CacheMissing);
    }

    CacheDatabase::open(paths).map_err(SearchError::from)
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
    fn search_filters_by_property_presence() {
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
