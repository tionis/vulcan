//! Reusable read-only MCP tool workflows. Transport adapters own response framing.

#![allow(clippy::must_use_candidate)]

use globset::Glob;
use serde_json::{Map, Value};
use vulcan_core::{
    evaluate_dql_with_filter, execute_query_report_with_filter, query_notes_with_filter,
    search_vault_with_filter, NoteQuery, PermissionGuard, ProfilePermissionGuard, QueryAst,
    QueryReport, SearchQuery, SearchSort, VaultPaths,
};

use crate::mcp_protocol::{McpMethodError, McpQueryArgs, McpSearchArgs};
use crate::notes::resolve_existing_markdown_target;

const MCP_QUERY_SOFT_MAX: usize = 200;
pub const MCP_QUERY_HARD_MAX: usize = 1_000;

/// Apply the note-source permission boundary before any MCP note read.
pub fn check_read_markdown_source_access(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    note: &str,
) -> Result<(), McpMethodError> {
    if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    let target = resolve_existing_markdown_target(paths, note)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let Some(relative_path) = target.vault_relative_path.as_deref() else {
        return Err(McpMethodError::tool(format!(
            "permission profiles cannot read markdown files outside the selected vault root: {}",
            target.display_path
        )));
    };
    guard
        .check_read_path(relative_path)
        .map_err(|error| McpMethodError::tool(error.to_string()))
}

pub fn search(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: McpSearchArgs,
) -> Result<Value, McpMethodError> {
    if args.limit == 0 {
        return Err(McpMethodError::invalid_params(
            "`search.limit` must be at least 1",
        ));
    }
    let report = search_vault_with_filter(
        paths,
        &SearchQuery {
            text: args.query,
            tag: args.tag,
            path_prefix: args.path_prefix,
            has_property: args.has_property,
            filters: args.filters,
            provider: None,
            mode: parse_search_mode(args.mode.as_deref())?,
            sort: parse_search_sort(args.sort.as_deref())?,
            match_case: args.match_case.then_some(true),
            limit: Some(args.limit),
            context_size: args.context_size,
            raw_query: args.raw_query,
            fuzzy: args.fuzzy,
            explain: args.explain,
        },
        Some(&guard.read_filter()),
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    serde_json::to_value(report).map_err(|error| {
        McpMethodError::internal(format!("failed to serialize `search` report: {error}"))
    })
}

#[allow(clippy::too_many_lines)] // Keep DQL and structural routing in one shared MCP contract.
pub fn query(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    mut args: McpQueryArgs,
) -> Result<Value, McpMethodError> {
    validate_mcp_query_page(&args)?;
    if args.query.is_some() && args.json.is_some() {
        return Err(McpMethodError::invalid_params(
            "`query` accepts either `query` or `json`, not both",
        ));
    }
    let use_dql = match args.engine.as_deref().unwrap_or("auto") {
        "dql" => true,
        "dsl" => false,
        "auto" => args.query.as_deref().is_some_and(|query| {
            query.trim_start().to_ascii_uppercase().starts_with("TABLE")
                || query.trim_start().to_ascii_uppercase().starts_with("LIST")
                || query.trim_start().to_ascii_uppercase().starts_with("TASK")
        }),
        other => {
            return Err(McpMethodError::invalid_params(format!(
                "unsupported `query.engine`: {other}"
            )));
        }
    };
    if use_dql {
        let dql = args
            .query
            .as_deref()
            .ok_or_else(|| McpMethodError::invalid_params("DQL queries require `query`"))?;
        if !args.filters.is_empty()
            || args.sort.is_some()
            || args.desc
            || args.path_prefix.is_some()
            || args.filename_pattern.is_some()
            || !args.fields.is_empty()
            || args.include_properties
        {
            return Err(McpMethodError::invalid_params(
                "DQL supports only `query`, `engine`, `limit`, and `offset`; use structural query mode for filters, path_prefix, filename_pattern, fields, or include_properties",
            ));
        }
        let mut result = evaluate_dql_with_filter(paths, dql, None, Some(&guard.read_filter()))
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        let total_count = result.rows.len();
        let start = args.offset.min(total_count);
        let end = start.saturating_add(args.limit).min(total_count);
        result.rows = result.rows[start..end].to_vec();
        result.result_count = result.rows.len();
        let mut structured = serde_json::to_value(result)
            .map_err(|error| McpMethodError::internal(error.to_string()))?;
        structured
            .as_object_mut()
            .expect("DQL result serializes as an object")
            .insert(
                "page".to_string(),
                serde_json::json!({
                    "limit": args.limit,
                    "offset": args.offset,
                    "returned": end.saturating_sub(start),
                    "total_count": total_count,
                    "has_more": end < total_count,
                    "next_offset": (end < total_count).then_some(end),
                }),
            );
        return Ok(structured);
    }
    if let Some(path_prefix) = args.path_prefix.as_deref() {
        if args.query.is_none() && args.json.is_none() {
            args.filters.push(format!(
                "file.path starts_with {}",
                serde_json::to_string(path_prefix).expect("string serialization")
            ));
        }
    }
    let report = match (args.query.as_deref(), args.json.as_deref()) {
        (Some(dsl), None) => {
            if !args.filters.is_empty() || args.sort.is_some() || args.desc {
                return Err(McpMethodError::invalid_params(
                    "`query.filters`, `sort`, and `desc` cannot be combined with a DSL string or JSON query",
                ));
            }
            let ast =
                QueryAst::from_dsl(dsl).map_err(|error| McpMethodError::tool(error.to_string()))?;
            execute_query_report_with_filter(paths, ast, Some(&guard.read_filter()))
                .map_err(|error| McpMethodError::tool(error.to_string()))?
        }
        (None, Some(json)) => {
            if !args.filters.is_empty() || args.sort.is_some() || args.desc {
                return Err(McpMethodError::invalid_params(
                    "`query.filters`, `sort`, and `desc` cannot be combined with a DSL string or JSON query",
                ));
            }
            let ast = QueryAst::from_json(json)
                .map_err(|error| McpMethodError::tool(error.to_string()))?;
            execute_query_report_with_filter(paths, ast, Some(&guard.read_filter()))
                .map_err(|error| McpMethodError::tool(error.to_string()))?
        }
        (None, None) => {
            let note_query = NoteQuery {
                filters: args.filters.clone(),
                sort_by: args.sort.clone(),
                sort_descending: args.desc,
            };
            let notes_report =
                query_notes_with_filter(paths, &note_query, Some(&guard.read_filter()))
                    .map_err(|error| McpMethodError::tool(error.to_string()))?;
            let ast = QueryAst::from_note_query(&note_query)
                .map_err(|error| McpMethodError::tool(error.to_string()))?;
            QueryReport {
                query: ast,
                notes: notes_report.notes,
                selection: None,
                selection_provenance: Vec::new(),
            }
        }
        (Some(_), Some(_)) => unreachable!("checked above"),
    };
    bounded_mcp_query_report(report, &args)
}

fn validate_mcp_query_page(args: &McpQueryArgs) -> Result<(), McpMethodError> {
    if args.limit == 0 {
        return Err(McpMethodError::invalid_params(
            "`query.limit` must be at least 1",
        ));
    }
    if args.limit > MCP_QUERY_HARD_MAX {
        return Err(McpMethodError::invalid_params(format!(
            "`query.limit` cannot exceed {MCP_QUERY_HARD_MAX}"
        )));
    }
    if args.limit > MCP_QUERY_SOFT_MAX && !args.allow_large_results {
        return Err(McpMethodError::invalid_params(format!(
            "`query.limit` above {MCP_QUERY_SOFT_MAX} requires `allow_large_results: true`"
        )));
    }
    Ok(())
}

fn bounded_mcp_query_report(
    report: QueryReport,
    args: &McpQueryArgs,
) -> Result<Value, McpMethodError> {
    let matcher = args
        .filename_pattern
        .as_deref()
        .map(|pattern| {
            Glob::new(pattern)
                .map(|glob| glob.compile_matcher())
                .map_err(|error| {
                    McpMethodError::invalid_params(format!(
                        "invalid `query.filename_pattern` glob: {error}"
                    ))
                })
        })
        .transpose()?;
    let path_prefix = args
        .path_prefix
        .as_deref()
        .map(|value| value.trim_matches('/'));
    let notes = report
        .notes
        .into_iter()
        .filter(|note| {
            path_prefix.is_none_or(|prefix| {
                prefix.is_empty()
                    || note.document_path == prefix
                    || note
                        .document_path
                        .strip_prefix(prefix)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            }) && matcher.as_ref().is_none_or(|matcher| {
                std::path::Path::new(&note.document_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| matcher.is_match(name))
            })
        })
        .collect::<Vec<_>>();
    let matched_count = notes.len();
    let query_start = report.query.offset.min(matched_count);
    let query_end = report.query.limit.map_or(matched_count, |limit| {
        query_start.saturating_add(limit).min(matched_count)
    });
    let query_notes = &notes[query_start..query_end];
    let total_count = query_notes.len();
    let start = args.offset.min(total_count);
    let limit = args.limit;
    let end = start.saturating_add(limit).min(total_count);
    let rows = query_notes[start..end]
        .iter()
        .map(|note| {
            let value = serde_json::to_value(note)
                .map_err(|error| McpMethodError::internal(error.to_string()))?;
            if !args.fields.is_empty() {
                return Ok(mcp_select_fields(&value, &args.fields));
            }
            let source = value.as_object().cloned().unwrap_or_default();
            let mut object = Map::new();
            for field in [
                "document_id",
                "document_path",
                "file_name",
                "file_ext",
                "file_mtime",
                "file_ctime",
                "file_size",
                "tags",
                "starred",
                "aliases",
                "periodic_type",
                "periodic_date",
            ] {
                if let Some(value) = source.get(field) {
                    object.insert(field.to_string(), value.clone());
                }
            }
            if args.include_properties {
                if let Some(properties) = source.get("properties") {
                    object.insert("properties".to_string(), properties.clone());
                }
            }
            Ok(Value::Object(object))
        })
        .collect::<Result<Vec<_>, McpMethodError>>()?;
    Ok(serde_json::json!({
        "query": report.query,
        "notes": rows,
        "page": {
            "limit": limit,
            "offset": start,
            "returned": end.saturating_sub(start),
            "total_count": total_count,
            "matched_count": matched_count,
            "query_offset": query_start,
            "has_more": end < total_count,
            "next_offset": (end < total_count).then_some(end),
        }
    }))
}

fn mcp_select_fields(value: &Value, fields: &[String]) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let mut selected = Map::new();
    for field in fields {
        let direct = object.get(field).cloned();
        let nested = field.split_once('.').and_then(|(namespace, key)| {
            object
                .get(namespace)
                .and_then(Value::as_object)
                .and_then(|values| values.get(key))
                .cloned()
        });
        let alias = match field.as_str() {
            "file.path" => object.get("document_path").cloned(),
            "file.name" => object.get("file_name").cloned(),
            "file.ext" | "file.extension" => object.get("file_ext").cloned(),
            "file.mtime" => object.get("file_mtime").cloned(),
            "file.ctime" => object.get("file_ctime").cloned(),
            "file.tags" => object.get("tags").cloned(),
            _ => None,
        };
        if let Some(field_value) = direct.or(nested).or(alias) {
            selected.insert(field.clone(), field_value);
        }
    }
    Value::Object(selected)
}

fn parse_search_mode(
    mode: Option<&str>,
) -> Result<vulcan_core::search::SearchMode, McpMethodError> {
    match mode.unwrap_or("keyword") {
        "keyword" => Ok(vulcan_core::search::SearchMode::Keyword),
        "hybrid" => Ok(vulcan_core::search::SearchMode::Hybrid),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `search.mode`: {other}"
        ))),
    }
}

fn parse_search_sort(sort: Option<&str>) -> Result<Option<SearchSort>, McpMethodError> {
    let value = match sort {
        None => return Ok(None),
        Some("relevance") => SearchSort::Relevance,
        Some("path_asc") => SearchSort::PathAsc,
        Some("path_desc") => SearchSort::PathDesc,
        Some("modified_newest") => SearchSort::ModifiedNewest,
        Some("modified_oldest") => SearchSort::ModifiedOldest,
        Some("created_newest") => SearchSort::CreatedNewest,
        Some("created_oldest") => SearchSort::CreatedOldest,
        Some(other) => {
            return Err(McpMethodError::invalid_params(format!(
                "unsupported `search.sort`: {other}"
            )));
        }
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode};

    #[test]
    fn search_validates_mcp_arguments_before_running() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        for arguments in [
            json!({"query": "alpha", "limit": 0}),
            json!({"query": "alpha", "mode": "unsupported"}),
            json!({"query": "alpha", "sort": "unsupported"}),
        ] {
            let args = serde_json::from_value(arguments).unwrap();
            assert!(matches!(
                search(&paths, &guard, args),
                Err(McpMethodError::JsonRpc { code: -32602, .. })
            ));
        }
    }

    #[test]
    fn search_respects_the_caller_read_filter() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(temporary.path().join(".vulcan")).unwrap();
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(
            temporary.path().join("Home.md"),
            "alpha searchable content\n",
        )
        .unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let readable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        let args = || serde_json::from_value(json!({"query": "alpha"})).unwrap();
        let visible = search(&paths, &readable, args()).unwrap();
        let hidden = search(&paths, &blind, args()).unwrap();
        assert!(visible.to_string().contains("Home.md"));
        assert!(!hidden.to_string().contains("Home.md"));
    }

    #[test]
    fn query_keeps_bounded_projection_and_read_filtering() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(temporary.path().join(".vulcan")).unwrap();
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(
            temporary.path().join("Home.md"),
            "---\ncategory: test\n---\n# Home\n",
        )
        .unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let readable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        let arguments = || {
            serde_json::from_value(json!({"fields": ["file.path", "properties.category"]})).unwrap()
        };
        let visible = query(&paths, &readable, arguments()).unwrap();
        assert_eq!(visible["notes"][0]["file.path"], "Home.md");
        assert_eq!(visible["notes"][0]["properties.category"], "test");
        assert_eq!(visible["page"]["returned"], 1);
        let hidden = query(&paths, &blind, arguments()).unwrap();
        assert_eq!(hidden["notes"], json!([]));

        let dql = query(
            &paths,
            &readable,
            serde_json::from_value(json!({"query": "LIST", "engine": "dql", "limit": 1})).unwrap(),
        )
        .unwrap();
        assert_eq!(dql["page"]["limit"], 1);
        assert_eq!(dql["page"]["returned"], 1);
    }

    #[test]
    fn query_rejects_widening_and_incompatible_options() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        for arguments in [
            json!({"limit": 0}),
            json!({"limit": 201}),
            json!({"query": "LIST", "json": "{}"}),
            json!({"query": "LIST", "engine": "dql", "path_prefix": "Notes"}),
        ] {
            let args = serde_json::from_value(arguments).unwrap();
            assert!(matches!(
                query(&paths, &guard, args),
                Err(McpMethodError::JsonRpc { code: -32602, .. })
            ));
        }
    }

    #[test]
    fn note_source_access_denies_external_files_for_restricted_profiles() {
        let temporary = tempfile::tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir_all(vault.join(".vulcan")).unwrap();
        fs::write(
            vault.join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(vault.join("Home.md"), "# Home\n").unwrap();
        let external = temporary.path().join("External.md");
        fs::write(&external, "# External\n").unwrap();
        let paths = VaultPaths::new(&vault);
        let unrestricted = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        let external = external.to_str().unwrap();
        assert!(check_read_markdown_source_access(&paths, &unrestricted, external).is_ok());
        let denied = check_read_markdown_source_access(&paths, &blind, external).unwrap_err();
        assert!(matches!(
            denied,
            McpMethodError::Tool { message, .. } if message.contains("outside the selected vault root")
        ));
        assert!(check_read_markdown_source_access(&paths, &blind, "Home.md").is_err());
    }
}
