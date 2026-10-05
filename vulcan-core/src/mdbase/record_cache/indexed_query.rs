//! Indexed execution of the physically executable query subset over a cache
//! whose every visible row is proven current by stat fingerprint.
//!
//! Results, exact totals, ordering, pagination, and diagnostics equal ordinary
//! execution over the same visible snapshot. Rows whose lowered predicate is
//! decided exactly by SQL need no CEL work; the rest are evaluated by the same
//! CEL filter as ordinary execution. Only the returned page is hydrated. Any
//! missing evidence, unsupported plan shape, or cache failure returns `None`
//! so the caller runs ordinary execution, which reports the canonical errors.
//!
//! Coherence: one discovery walk stats every visible record, and every read
//! (fingerprints, candidates, residual records, page) happens inside one
//! `SQLite` read transaction. A record that changes before its stat mismatches
//! its cached fingerprint and forces fallback; one that changes after its stat
//! is a later change. The result therefore equals the vault as of an instant
//! during the walk, without a second walk.

use super::{
    cache_collection_root, has_dynamic_local_membership, parse_json_column, stat_fingerprint,
    LocalRecordSnapshot, MdbaseRecordCacheError,
};
use crate::mdbase::query::{compare_query_values, indexed_query_row, IndexedSource};
use crate::mdbase::{
    discover_mdbase_record_stats_parallel, verify_mdbase_control_snapshots, MdbaseCelLimits,
    MdbaseCollection, MdbaseContractRegistry, MdbasePreparedQuery, MdbaseQueryError,
    MdbaseQueryMeta, MdbaseQueryResult, MdbaseRecordError, MdbaseTypeRegistry,
    MDBASE_RECORD_MODEL_VERSION,
};
use crate::permissions::PermissionFilter;
use chrono::{DateTime, Utc};
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

/// Work counters and stage timings for one indexed execution; no paths or values.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct MdbaseIndexedQueryMetrics {
    pub freshness_seconds: f64,
    pub execution_seconds: f64,
    pub visible_records: usize,
    pub type_candidates: usize,
    pub sql_decided: usize,
    pub residual_evaluations: usize,
    pub matched: usize,
    pub hydrated: usize,
}

struct CandidateRow {
    path: String,
    decided: bool,
    matched: bool,
    input_ok: bool,
    /// JSON text of each ordering key; `None` when the field is absent.
    keys: Vec<Option<String>>,
}

/// Execute `query` over the stat-proven current cache, or return `None` when the
/// plan, freshness proof, or cached evidence cannot guarantee results identical
/// to ordinary execution.
#[allow(clippy::too_many_arguments)]
pub fn execute_indexed_mdbase_query(
    connection: &Connection,
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    contracts: &MdbaseContractRegistry,
    query: &MdbasePreparedQuery,
    filter: Option<&PermissionFilter>,
    now: DateTime<Utc>,
    metrics: &mut MdbaseIndexedQueryMetrics,
) -> Result<Option<MdbaseQueryResult>, MdbaseQueryError> {
    *metrics = MdbaseIndexedQueryMetrics::default();
    let Some(indexed) = query.indexed_plan() else {
        return Ok(None);
    };
    // Inferred membership may depend on the clock; cached types are not proof.
    if has_dynamic_local_membership(types) {
        return Ok(None);
    }
    let start = Instant::now();
    let Ok(transaction) = connection.unchecked_transaction() else {
        return Ok(None);
    };
    let Ok(Some((dependency_digest, visible))) =
        prove_fresh(&transaction, collection, types, contracts, filter)
    else {
        return Ok(None);
    };
    metrics.visible_records = visible.len();
    metrics.freshness_seconds = start.elapsed().as_secs_f64();
    let start = Instant::now();
    let result = execute_proven(
        &transaction,
        collection,
        types,
        query,
        &indexed,
        &dependency_digest,
        &visible,
        filter,
        now,
        metrics,
    );
    metrics.execution_seconds = start.elapsed().as_secs_f64();
    result
}

/// Verify controls, walk and stat every visible record once, and compare each
/// fingerprint with its cached row in the open read transaction. Returns the
/// dependency digest and visible paths, or `None` on any miss.
fn prove_fresh(
    transaction: &Connection,
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    contracts: &MdbaseContractRegistry,
    filter: Option<&PermissionFilter>,
) -> Result<Option<(String, BTreeSet<String>)>, MdbaseRecordCacheError> {
    let controls = verify_mdbase_control_snapshots(collection, types, contracts, filter)?;
    let root = cache_collection_root(collection)?;
    let current = discover_mdbase_record_stats_parallel(collection)
        .map_err(MdbaseRecordError::Discovery)?
        .into_iter()
        .filter(|(path, _)| filter.is_none_or(|filter| filter.is_allowed(path)))
        .map(|(path, metadata)| (path, metadata.as_ref().and_then(stat_fingerprint)))
        .collect::<Vec<_>>();
    // Merge-join the sorted walk with the path-ordered cache rows. Rows for
    // paths absent from the walk are ignored only when the caller cannot read
    // them; a readable cached row without a file means the cache is stale.
    let mut statement = transaction.prepare_cached(
        "SELECT path, stat_fingerprint FROM mdbase_record_query
         WHERE collection_root = ?1 AND dependency_digest = ?2
           AND record_model_version = ?3 AND stat_fingerprint IS NOT NULL
         ORDER BY path",
    )?;
    let mut rows = statement.query(rusqlite::params![
        root,
        controls.combined,
        MDBASE_RECORD_MODEL_VERSION
    ])?;
    let mut walked = current.iter().peekable();
    while let Some(row) = rows.next()? {
        let (Ok(path), Ok(fingerprint)) = (row.get_ref(0)?.as_str(), row.get_ref(1)?.as_blob())
        else {
            return Ok(None);
        };
        match walked.peek() {
            Some((walked_path, walked_fingerprint)) if walked_path == path => {
                if walked_fingerprint.as_ref().map(<[u8; 40]>::as_slice) != Some(fingerprint) {
                    return Ok(None);
                }
                walked.next();
            }
            Some((walked_path, _)) if walked_path.as_str() < path => return Ok(None),
            _ => {
                if filter.is_none_or(|filter| filter.is_allowed(path)) {
                    return Ok(None);
                }
            }
        }
    }
    if walked.next().is_some() {
        return Ok(None);
    }
    Ok(Some((
        controls.combined,
        current.into_iter().map(|(path, _)| path).collect(),
    )))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn execute_proven(
    transaction: &Connection,
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    query: &MdbasePreparedQuery,
    indexed: &crate::mdbase::query::IndexedPlan<'_>,
    dependency_digest: &str,
    visible: &BTreeSet<String>,
    filter: Option<&PermissionFilter>,
    now: DateTime<Utc>,
    metrics: &mut MdbaseIndexedQueryMetrics,
) -> Result<Option<MdbaseQueryResult>, MdbaseQueryError> {
    let plan = query.plan();
    let mut sources = Vec::with_capacity(plan.order_by.len());
    for key in &plan.order_by {
        let source = indexed.source(&key.field, true);
        // SQLite JSON paths cannot quote these characters in a label.
        if let IndexedSource::File(name) | IndexedSource::Effective(name) = source {
            if name.contains(['"', '\\']) {
                return Ok(None);
            }
        }
        sources.push(source);
    }
    let Ok(rows) = select_candidates(
        transaction,
        collection,
        plan,
        dependency_digest,
        filter
            .is_some_and(|filter| !filter.path_permission().is_unrestricted())
            .then_some(visible),
        indexed.predicate,
        &sources,
    ) else {
        return Ok(None);
    };
    let clock = query.clock(collection.config.settings.timezone.as_deref(), now)?;
    let mut diagnostics = Vec::new();
    let mut matched = Vec::new();
    let mut residual = Vec::new();
    for row in rows {
        if !visible.contains(&row.path) {
            continue;
        }
        metrics.type_candidates += 1;
        if indexed.predicate.is_none() {
            matched.push(row);
            continue;
        }
        // Ordinary execution checks every type candidate's filter input, even
        // when the filter is false; an over-limit record must fail the query.
        if !row.input_ok {
            return Ok(None);
        }
        if row.decided {
            metrics.sql_decided += 1;
            if row.matched {
                matched.push(row);
            }
        } else {
            residual.push(row);
        }
    }
    if !residual.is_empty() {
        let Ok(records) = load_local_records(
            transaction,
            collection,
            dependency_digest,
            residual.iter().map(|row| row.path.as_str()),
        ) else {
            return Ok(None);
        };
        // Rows arrive in path order, matching ordinary diagnostic order.
        for row in residual {
            let Some(record) = records.get(&row.path) else {
                return Ok(None);
            };
            metrics.residual_evaluations += 1;
            if query.evaluate_residual_filter(&record.record, types, &clock, &mut diagnostics)? {
                matched.push(row);
            }
        }
    }
    metrics.matched = matched.len();

    let mut keyed = Vec::with_capacity(matched.len());
    for row in matched {
        let mut keys = Vec::with_capacity(row.keys.len());
        for key in row.keys {
            // An absent field orders as null, exactly like `candidate_value`.
            keys.push(match key {
                Some(json) => match serde_json::from_str::<serde_json::Value>(&json) {
                    Ok(value) => value,
                    Err(_) => return Ok(None),
                },
                None => serde_json::Value::Null,
            });
        }
        keyed.push((row.path, keys));
    }
    keyed.sort_by(|(left_path, left), (right_path, right)| {
        for ((key, left), right) in plan.order_by.iter().zip(left).zip(right) {
            let order = compare_query_values(left, right, key.direction);
            if order != std::cmp::Ordering::Equal {
                return order;
            }
        }
        left_path.cmp(right_path)
    });

    let total_count = keyed.len();
    let start = plan.offset.min(total_count);
    let end = plan.limit.map_or(total_count, |limit| {
        start.saturating_add(limit).min(total_count)
    });
    let page = keyed[start..end]
        .iter()
        .map(|(path, _)| path.as_str())
        .collect::<Vec<_>>();
    let Ok(hydrated) = load_page(
        transaction,
        collection,
        dependency_digest,
        &page,
        needs_persisted(plan),
    ) else {
        return Ok(None);
    };
    let mut results = Vec::with_capacity(page.len());
    for path in page {
        let Some((effective, file, frontmatter)) = hydrated.get(path) else {
            return Ok(None);
        };
        let values = indexed.selections.as_ref().map(|selections| {
            selections
                .iter()
                .map(|(output, field)| {
                    (
                        (*output).to_string(),
                        indexed.value(field, effective, file, false),
                    )
                })
                .collect::<serde_json::Map<_, _>>()
        });
        results.push(indexed_query_row(
            plan,
            path,
            frontmatter.clone(),
            effective.clone(),
            values,
        ));
    }
    metrics.hydrated = results.len();
    Ok(Some(MdbaseQueryResult {
        results,
        meta: MdbaseQueryMeta {
            total_count,
            has_more: end < total_count,
            context: None,
            groups: None,
        },
        diagnostics,
    }))
}

#[allow(clippy::too_many_lines)]
fn select_candidates(
    connection: &Connection,
    collection: &MdbaseCollection,
    plan: &crate::query::StructuredQueryPlan,
    dependency_digest: &str,
    restricted_visible: Option<&BTreeSet<String>>,
    predicate: Option<&crate::mdbase::MdbaseSqlPredicate>,
    sources: &[IndexedSource<'_>],
) -> Result<Vec<CandidateRow>, MdbaseRecordCacheError> {
    let root = cache_collection_root(collection)?;
    let limits = MdbaseCelLimits::default();
    let limit = |value: usize| SqlValue::Integer(i64::try_from(value).unwrap_or(i64::MAX));
    // Same bound as `MdbaseQueryInputEvidence::passes`: a resolved link path
    // can add at most six bytes per path byte plus a constant per link.
    let max_path_bytes: i64 = connection.query_row(
        "SELECT COALESCE(MAX(length(CAST(path AS BLOB))), 0)
         FROM mdbase_record_query WHERE collection_root = ?1",
        [&root],
        |row| row.get(0),
    )?;
    let per_link = max_path_bytes.saturating_mul(6).saturating_add(64);
    let mut parameters = vec![
        SqlValue::Text(root),
        SqlValue::Text(dependency_digest.to_string()),
        SqlValue::Integer(i64::from(MDBASE_RECORD_MODEL_VERSION)),
        limit(limits.max_value_nodes),
        limit(limits.max_collection_items),
        limit(limits.max_value_bytes),
        SqlValue::Integer(per_link),
    ];
    // Type membership drives the scan through its index; visibility filters
    // rows before any select-list JSON expression runs, so a hidden payload is
    // never parsed.
    let membership = if plan.types.is_empty() {
        "mdbase_record_query AS record".to_string()
    } else {
        let types = plan
            .types
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        parameters.push(SqlValue::Text(serde_json::to_string(&types)?));
        format!(
            "(SELECT DISTINCT path FROM mdbase_record_types
              WHERE collection_root = ?1
                AND type_name IN (SELECT value FROM json_each(?{}))) AS membership
             JOIN mdbase_record_query AS record
               ON record.collection_root = ?1 AND record.path = membership.path",
            parameters.len()
        )
    };
    let visible = if let Some(paths) = restricted_visible {
        parameters.push(SqlValue::Text(serde_json::to_string(paths)?));
        format!(
            "AND record.path IN (SELECT value FROM json_each(?{}))",
            parameters.len()
        )
    } else {
        String::new()
    };
    let (atoms, decided, matched) = match predicate {
        Some(predicate) => {
            let (columns, valid, matches) =
                predicate.render_columns(&mut parameters, "record.effective_frontmatter_jsonb");
            (
                columns.iter().fold(String::new(), |mut sql, column| {
                    sql.push_str(", ");
                    sql.push_str(column);
                    sql
                }),
                format!("COALESCE({valid}, 0)"),
                format!("COALESCE({matches}, 0)"),
            )
        }
        None => (String::new(), "1".to_string(), "1".to_string()),
    };
    // Ordering keys as JSON text, read only for rows that may match.
    let keys = sources
        .iter()
        .map(|source| match source {
            IndexedSource::Null => "NULL".to_string(),
            IndexedSource::File(name) => {
                parameters.push(SqlValue::Text(format!("$.\"{name}\"")));
                format!("file_json -> ?{}", parameters.len())
            }
            IndexedSource::Effective(name) => {
                parameters.push(SqlValue::Text(format!("$.\"{name}\"")));
                format!("effective_frontmatter_json -> ?{}", parameters.len())
            }
        })
        .fold(String::new(), |mut sql, key| {
            sql.push_str(", CASE WHEN NOT decided OR matched THEN ");
            sql.push_str(&key);
            sql.push_str(" END");
            sql
        });
    // Each level is a LIMIT -1 subquery so SQLite cannot flatten it: every
    // JSON extraction runs once per row, never once per reference.
    let sql = format!(
        "SELECT path, decided, matched, input_ok {keys}
         FROM (
             SELECT path, input_ok, effective_frontmatter_json, file_json,
                    {decided} AS decided, {matched} AS matched
             FROM (
                 SELECT record.path AS path,
                        record.input_converted != 0
                          AND record.input_nodes <= ?4
                          AND record.input_width <= ?5
                          AND record.input_bytes + record.input_links * ?7 <= ?6
                          AS input_ok,
                        record.effective_frontmatter_jsonb AS effective_frontmatter_json,
                        record.file_json AS file_json {atoms}
                 FROM {membership}
                 WHERE record.collection_root = ?1 AND record.dependency_digest = ?2
                   AND record.record_model_version = ?3 {visible}
                 LIMIT -1
             ) AS record
             LIMIT -1
         )
         ORDER BY path"
    );
    let key_count = sources.len();
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
        Ok(CandidateRow {
            path: row.get(0)?,
            decided: row.get::<_, i64>(1)? != 0,
            matched: row.get::<_, i64>(2)? != 0,
            input_ok: row.get::<_, i64>(3)? != 0,
            keys: (0..key_count)
                .map(|index| row.get::<_, Option<String>>(4 + index))
                .collect::<Result<_, _>>()?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn load_local_records<'a>(
    connection: &Connection,
    collection: &MdbaseCollection,
    dependency_digest: &str,
    paths: impl Iterator<Item = &'a str>,
) -> Result<BTreeMap<String, LocalRecordSnapshot>, MdbaseRecordCacheError> {
    let paths = paths.collect::<Vec<_>>();
    let mut statement = connection.prepare_cached(
        "SELECT cache.path, cache.local_record_json
         FROM mdbase_record_query AS query
         JOIN mdbase_record_cache AS cache
           ON cache.collection_root = query.collection_root AND cache.path = query.path
          AND cache.revision = query.revision
         WHERE query.collection_root = ?1 AND query.dependency_digest = ?2
           AND query.record_model_version = ?3 AND cache.local_record_json IS NOT NULL
           AND query.path IN (SELECT value FROM json_each(?4))",
    )?;
    let rows = statement.query_map(
        rusqlite::params![
            cache_collection_root(collection)?,
            dependency_digest,
            MDBASE_RECORD_MODEL_VERSION,
            serde_json::to_string(&paths)?
        ],
        |row| {
            let path: String = row.get(0)?;
            let json: String = row.get(1)?;
            Ok((path, parse_json_column::<LocalRecordSnapshot>(1, &json)?))
        },
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn needs_persisted(plan: &crate::query::StructuredQueryPlan) -> bool {
    matches!(
        plan.frontmatter_mode,
        crate::query::QueryFrontmatterMode::Persisted | crate::query::QueryFrontmatterMode::Both
    )
}

type PageRows = BTreeMap<String, (serde_json::Value, serde_json::Value, serde_json::Value)>;

/// Hydrate only the returned page: effective frontmatter and file metadata
/// from narrow rows, persisted frontmatter from the wide row only if returned.
fn load_page(
    connection: &Connection,
    collection: &MdbaseCollection,
    dependency_digest: &str,
    paths: &[&str],
    persisted: bool,
) -> Result<PageRows, MdbaseRecordCacheError> {
    let persisted = if persisted {
        "(SELECT json_extract(cache.metadata_json, '$.frontmatter')
          FROM mdbase_record_cache AS cache
          WHERE cache.collection_root = query.collection_root AND cache.path = query.path
            AND cache.revision = query.revision)"
    } else {
        "'null'"
    };
    let mut statement = connection.prepare_cached(&format!(
        "SELECT query.path, json(query.effective_frontmatter_jsonb), query.file_json, {persisted}
         FROM mdbase_record_query AS query
         WHERE query.collection_root = ?1 AND query.dependency_digest = ?2
           AND query.record_model_version = ?3
           AND query.path IN (SELECT value FROM json_each(?4))"
    ))?;
    let rows = statement.query_map(
        rusqlite::params![
            cache_collection_root(collection)?,
            dependency_digest,
            MDBASE_RECORD_MODEL_VERSION,
            serde_json::to_string(paths)?
        ],
        |row| {
            let path: String = row.get(0)?;
            let effective: String = row.get(1)?;
            let file: String = row.get(2)?;
            let frontmatter: Option<String> = row.get(3)?;
            Ok((
                path,
                (
                    parse_json_column(1, &effective)?,
                    parse_json_column(2, &file)?,
                    // A missing or mismatched wide row is not a snapshot.
                    parse_json_column(3, &frontmatter.ok_or(rusqlite::Error::InvalidQuery)?)?,
                ),
            ))
        },
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheDatabase;
    use crate::mdbase::{
        compile_mdbase_prepared_query, load_mdbase_collection, load_mdbase_contract_registry,
        load_mdbase_records_with_contracts_filtered, load_mdbase_type_registry,
        refresh_mdbase_record_cache,
    };
    use crate::paths::VaultPaths;
    use crate::permissions::{PathPermission, ResourceSpecifier};
    use std::fs;
    use std::path::Path;
    use tempfile::{tempdir, TempDir};

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    const TASK_TYPE: &str = "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    properties:\n      title: {type: string}\n      status: {type: string}\n      priority: {type: integer}\n      flag: {type: boolean}\ncollection:\n  read_defaults: {status: open}\n---\n";
    const CONTACT_TYPE: &str = "---\nkind: mdbase.type\nname: contact\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    properties:\n      email: {type: string}\n---\n";

    fn fixture() -> (TempDir, VaultPaths) {
        let directory = tempdir().unwrap();
        let root = directory.path();
        write(&root.join("mdbase.yaml"), "spec_version: 0.3.0\n");
        write(&root.join("_types/task.md"), TASK_TYPE);
        write(&root.join("_types/contact.md"), CONTACT_TYPE);
        for (path, fields) in [
            (
                "tasks/a.md",
                "title: Alpha\nstatus: open\npriority: 2\nflag: true\n",
            ),
            ("tasks/b.md", "title: Beta\nstatus: done\npriority: 5\n"),
            ("tasks/c.md", "title: Alpha\npriority: 1\n"),
            ("tasks/d.md", "status: null\npriority: 3\n"),
            ("tasks/e.md", "title: 7\nstatus: [open]\npriority: high\n"),
            (
                "tasks/f.md",
                "title: \"Zed\\0x\"\nstatus: \"open\\0x\"\npriority: 4\nflag: false\n",
            ),
            (
                "tasks/g.md",
                "title: Gamma\nstatus: open\npriority: 9007199254740993\n",
            ),
            ("tasks/h.md", "title: Eta\nstatus: OPEN\npriority: -1\n"),
            ("private/p.md", "title: Hidden\nstatus: open\npriority: 2\n"),
        ] {
            write(
                &root.join(path),
                &format!("---\ntype: task\n{fields}---\nBody [[tasks/a]]\n"),
            );
        }
        write(
            &root.join("people/x.md"),
            "---\ntype: contact\nemail: x@example.invalid\n---\n",
        );
        write(&root.join("people/y.md"), "---\ntype: contact\n---\n");
        write(&root.join("loose.md"), "---\nstatus: open\n---\n");
        let paths = VaultPaths::new(root);
        crate::initialize_vulcan_dir(&paths).unwrap();
        refresh(&paths);
        (directory, paths)
    }

    fn refresh(paths: &VaultPaths) {
        let mut database = CacheDatabase::open(paths).unwrap();
        let (collection, types, contracts) = registries(paths);
        refresh_mdbase_record_cache(&mut database, &collection, &types, &contracts).unwrap();
    }

    fn registries(
        paths: &VaultPaths,
    ) -> (MdbaseCollection, MdbaseTypeRegistry, MdbaseContractRegistry) {
        let collection = load_mdbase_collection(paths.vault_root()).unwrap().unwrap();
        let types = load_mdbase_type_registry(&collection).unwrap();
        let contracts = load_mdbase_contract_registry(&collection, &types).unwrap();
        (collection, types, contracts)
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-05T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Run the indexed path and ordinary source-derived execution on the same
    /// snapshot; returns the indexed result (if eligible) and the oracle.
    fn both(
        paths: &VaultPaths,
        query: &serde_json::Value,
        filter: Option<&PermissionFilter>,
    ) -> (
        Option<MdbaseQueryResult>,
        Result<MdbaseQueryResult, MdbaseQueryError>,
        MdbaseIndexedQueryMetrics,
    ) {
        let (collection, types, contracts) = registries(paths);
        let prepared = compile_mdbase_prepared_query(query).unwrap();
        let connection = Connection::open(paths.cache_db()).unwrap();
        let mut metrics = MdbaseIndexedQueryMetrics::default();
        let indexed = execute_indexed_mdbase_query(
            &connection,
            &collection,
            &types,
            &contracts,
            &prepared,
            filter,
            now(),
            &mut metrics,
        )
        .unwrap();
        let records = load_mdbase_records_with_contracts_filtered(
            &collection,
            &types,
            &contracts,
            false,
            filter,
        )
        .unwrap();
        let oracle = prepared.execute(
            &records,
            &types,
            &collection.config.settings.id_field,
            collection.config.settings.timezone.as_deref(),
            now(),
        );
        (indexed, oracle, metrics)
    }

    fn private_hidden() -> PermissionFilter {
        PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::All],
            deny: vec![ResourceSpecifier::Folder("private/**".into())],
        })
    }

    #[test]
    fn indexed_results_equal_ordinary_execution() {
        let (_directory, paths) = fixture();
        let hidden = private_hidden();
        let queries = [
            serde_json::json!({"types": ["task"], "where": "status == 'open'",
                "order_by": [{"field": "title"}], "select": ["file.path", "title", "status"], "limit": 3}),
            serde_json::json!({"types": ["TASK"], "where": "status == 'open'",
                "order_by": [{"field": "title", "direction": "desc"}], "offset": 1}),
            serde_json::json!({"types": ["task"], "where": "priority >= 2",
                "order_by": [{"field": "priority", "direction": "desc"}], "select": ["priority"]}),
            serde_json::json!({"types": ["task"], "where": "status != 'done' && priority < 5",
                "order_by": [{"field": "title"}, {"field": "priority", "direction": "desc"}]}),
            serde_json::json!({"where": "file.path.startsWith('tasks/')",
                "order_by": [{"field": "file.name", "direction": "desc"}], "frontmatter_mode": "both"}),
            serde_json::json!({"types": ["contact"], "order_by": [{"field": "email"}],
                "select": ["email"], "frontmatter_mode": "persisted"}),
            serde_json::json!({"types": ["task"], "where": "flag == true || flag == false"}),
            serde_json::json!({"types": ["task"], "where": "flag == false", "select": ["file.name"],
                "order_by": [{"field": "name"}]}),
            serde_json::json!({"types": ["task"], "where": "undeclared == 'x'"}),
            serde_json::json!({"types": ["task", "contact"], "order_by": [{"field": "title"}], "limit": 4}),
            serde_json::json!({"types": ["task"], "where": "title == 'Alpha' && status == 'open'",
                "order_by": [{"field": "projection.missing"}]}),
            serde_json::json!({"types": ["missing"]}),
        ];
        for query in &queries {
            for filter in [None, Some(&hidden)] {
                let (indexed, oracle, _) = both(&paths, query, filter);
                let oracle = oracle.unwrap();
                match indexed {
                    Some(indexed) => assert_eq!(indexed, oracle, "{query}"),
                    // `||` is not lowered; such plans must decline.
                    None => assert!(
                        query["where"].as_str().is_some_and(|w| w.contains("||")),
                        "{query}"
                    ),
                }
            }
        }
        let (indexed, _, metrics) = both(
            &paths,
            &serde_json::json!({"types": ["task"], "where": "status == 'open'"}),
            Some(&hidden),
        );
        assert!(indexed.is_some());
        // Hidden records are neither candidates nor visible.
        assert_eq!(metrics.visible_records, 11);
        assert_eq!(metrics.type_candidates, 8);
        // Missing, null, wrong-typed, and NUL-bearing statuses need residual CEL.
        assert!(metrics.residual_evaluations >= 3, "{metrics:?}");
    }

    #[test]
    fn indexed_execution_declines_when_freshness_cannot_be_proven() {
        let (directory, paths) = fixture();
        let query = serde_json::json!({"types": ["task"], "where": "status == 'open'"});
        assert!(both(&paths, &query, None).0.is_some());

        // A same-size edit with a restored mtime still changes the ctime.
        let path = directory.path().join("tasks/a.md");
        let before = fs::metadata(&path).unwrap();
        let source = fs::read_to_string(&path).unwrap().replace("Alpha", "Omega");
        fs::write(&path, source).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(before.modified().unwrap())
            .unwrap();
        assert!(both(&paths, &query, None).0.is_none());
        refresh(&paths);
        assert!(both(&paths, &query, None).0.is_some());

        write(
            &directory.path().join("tasks/new.md"),
            "---\ntype: task\nstatus: open\n---\n",
        );
        assert!(both(&paths, &query, None).0.is_none());
        refresh(&paths);
        fs::remove_file(directory.path().join("tasks/new.md")).unwrap();
        assert!(both(&paths, &query, None).0.is_none());
        refresh(&paths);
        assert!(both(&paths, &query, None).0.is_some());

        // A control change alters the dependency digest of every row.
        write(
            &directory.path().join("_types/task.md"),
            &TASK_TYPE.replace("status: open", "status: done"),
        );
        assert!(both(&paths, &query, None).0.is_none());
        refresh(&paths);
        let (indexed, oracle, _) = both(&paths, &query, None);
        assert_eq!(indexed.unwrap(), oracle.unwrap());
    }

    #[test]
    fn indexed_execution_declines_over_limit_inputs_and_inferred_types() {
        let (directory, paths) = fixture();
        let items = "  - 1\n".repeat(3000);
        write(
            &directory.path().join("tasks/huge.md"),
            &format!("---\ntype: task\nstatus: done\nitems:\n{items}---\n"),
        );
        refresh(&paths);
        let query = serde_json::json!({"types": ["task"], "where": "status == 'open'"});
        let (indexed, oracle, _) = both(&paths, &query, None);
        // Ordinary execution fails on the over-width input; indexed declines.
        assert!(oracle.is_err());
        assert!(indexed.is_none());
        // Without a filter there is no input check, so the indexed path runs.
        let (indexed, oracle, _) = both(&paths, &serde_json::json!({"types": ["task"]}), None);
        assert_eq!(indexed.unwrap(), oracle.unwrap());

        fs::remove_file(directory.path().join("tasks/huge.md")).unwrap();
        write(
            &directory.path().join("_types/inferred.md"),
            "---\nkind: mdbase.type\nname: inferred\nmatch:\n  expr: {$expr: 'true'}\nschema:\n  dialect: json-schema-2020-12\n  value: {type: object}\n---\n",
        );
        refresh(&paths);
        assert!(both(&paths, &query, None).0.is_none());
    }
}
