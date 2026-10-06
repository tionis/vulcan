//! Reusable read-only MCP tool workflows. Transport adapters own response framing.

#![allow(clippy::must_use_candidate)]

use globset::Glob;
use serde_json::{Map, Value};
use std::fs;
use vulcan_core::config::TasksDefaultSource;
use vulcan_core::{
    execute_query_report_with_filter, query_notes_with_filter, search_vault_with_filter, NoteQuery,
    PermissionGuard, ProfilePermissionGuard, QueryAst, QueryReport, SearchQuery, SearchSort,
    TasksQueryResult, VaultPaths,
};

use crate::mcp_access;
use crate::mcp_protocol::{
    McpDailyArgs, McpDailyListArgs, McpDailyShowArgs, McpMethodError, McpNoteGetArgs,
    McpNoteInfoArgs, McpNoteOutlineArgs, McpQueryArgs, McpSearchArgs, McpTaskListArgs,
    McpTaskQueryArgs,
};
use crate::notes::{
    build_note_info_report,
    check_read_markdown_source_access as app_check_read_markdown_source_access, read_note,
    read_note_outline, NoteGetOptions, NoteGetReport, NoteInfoReport, NoteOutlineReport,
    NoteReadMode,
};
use crate::periodic::{
    current_local_date_string, list_daily_notes, normalize_date_argument, read_daily_note,
    read_latest_daily_note_where, show_periodic_note, DailyNoteReadReport, DailyReadTarget,
};
use crate::tasks::{
    build_tasks_list_report_with_guard, build_tasks_query_result_with_guard, TaskListRequest,
};

const MCP_QUERY_SOFT_MAX: usize = 200;
pub const MCP_QUERY_HARD_MAX: usize = 1_000;
const MCP_DAILY_LIST_MAX_LIMIT: usize = 200;

/// Remove unreadable task rows from both flat and grouped task-query output.
pub fn filter_tasks_query_report(guard: &ProfilePermissionGuard, report: &mut TasksQueryResult) {
    let readable = |task: &Value| {
        task.get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| guard.check_read_path(path).is_ok())
    };
    report.tasks.retain(&readable);
    for group in &mut report.groups {
        group.tasks.retain(&readable);
    }
    report.groups.retain(|group| !group.tasks.is_empty());
    report.result_count = report.tasks.len();
}

/// Execute the MCP task-list read with authority applied before evaluation.
pub fn task_list(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: McpTaskListArgs,
) -> Result<TasksQueryResult, McpMethodError> {
    build_tasks_list_report_with_guard(
        paths,
        &TaskListRequest {
            filter: args.filter,
            source: parse_tasks_default_source(args.source.as_deref())?,
            status: args.status,
            priority: args.priority,
            due_before: args.due_before,
            due_after: args.due_after,
            project: args.project,
            context: args.context,
            group_by: args.group_by,
            sort_by: args.sort_by,
            include_archived: args.include_archived,
        },
        guard,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))
}

/// Execute the MCP task query over the caller's authorized note universe.
pub fn task_query(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpTaskQueryArgs,
) -> Result<TasksQueryResult, McpMethodError> {
    build_tasks_query_result_with_guard(paths, &args.query, guard)
        .map_err(|error| McpMethodError::tool(error.to_string()))
}

fn parse_tasks_default_source(
    value: Option<&str>,
) -> Result<Option<TasksDefaultSource>, McpMethodError> {
    match value {
        None => Ok(None),
        Some("all") => Ok(Some(TasksDefaultSource::All)),
        Some("inline") => Ok(Some(TasksDefaultSource::Inline)),
        Some("tasknotes" | "file") => Ok(Some(TasksDefaultSource::Tasknotes)),
        Some(other) => Err(McpMethodError::invalid_params(format!(
            "unsupported `task_list.source`: {other}"
        ))),
    }
}

/// Apply the read boundary even for a daily report that omits its content.
pub fn include_daily_content_after_access(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    report: &mut DailyNoteReadReport,
    include_content: bool,
) -> Result<(), McpMethodError> {
    let Some(path) = report.path.as_deref() else {
        return Ok(());
    };
    guard
        .check_read_path(path)
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    if include_content && report.exists {
        report.content = Some(
            fs::read_to_string(paths.vault_root().join(path))
                .map_err(|error| McpMethodError::tool(error.to_string()))?,
        );
    }
    Ok(())
}

/// Bound and project daily-list reports identically for stdio and HTTP MCP hosts.
pub fn bounded_daily_list<T: serde::Serialize>(
    items: Vec<T>,
    limit: usize,
    offset: usize,
    order: Option<&str>,
    include_events: bool,
) -> Result<Value, McpMethodError> {
    if limit == 0 || limit > MCP_DAILY_LIST_MAX_LIMIT {
        return Err(McpMethodError::invalid_params(format!(
            "`daily.limit` must be between 1 and {MCP_DAILY_LIST_MAX_LIMIT}"
        )));
    }
    let descending = match order.unwrap_or("desc") {
        "asc" => false,
        "desc" => true,
        other => {
            return Err(McpMethodError::invalid_params(format!(
                "unsupported `daily.order`: {other}"
            )));
        }
    };
    let mut items = items
        .into_iter()
        .map(|item| {
            serde_json::to_value(item).map_err(|error| McpMethodError::internal(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    items.sort_by(|left, right| {
        left.get("date")
            .and_then(Value::as_str)
            .cmp(&right.get("date").and_then(Value::as_str))
    });
    if descending {
        items.reverse();
    }
    if !include_events {
        for item in &mut items {
            item.as_object_mut().map(|object| object.remove("events"));
        }
    }
    let total_count = items.len();
    let start = offset.min(total_count);
    let end = start.saturating_add(limit).min(total_count);
    Ok(serde_json::json!({
        "items": items[start..end],
        "page": {
            "limit": limit,
            "offset": offset,
            "returned": end.saturating_sub(start),
            "total_count": total_count,
            "has_more": end < total_count,
            "next_offset": (end < total_count).then_some(end),
        }
    }))
}

/// Execute the compact daily MCP tool with the caller's read boundary.
pub fn daily(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: McpDailyArgs,
) -> Result<Value, McpMethodError> {
    match args.operation.as_str() {
        "latest" => {
            let mut report = read_latest_daily_note_where(paths, false, |path| {
                guard.check_read_path(path).is_ok()
            })
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
            include_daily_content_after_access(paths, guard, &mut report, args.include_content)?;
            serde_json::to_value(report)
                .map_err(|error| McpMethodError::internal(error.to_string()))
        }
        "today" | "show" => {
            let date = if args.operation == "today" {
                current_local_date_string()
            } else {
                let raw = args.date.as_deref().ok_or_else(|| {
                    McpMethodError::invalid_params("daily operation `show` requires `date`")
                })?;
                normalize_date_argument(Some(raw))
                    .map_err(|error| McpMethodError::tool(error.to_string()))?
            };
            let mut report = read_daily_note(paths, DailyReadTarget::Date(&date), false)
                .map_err(|error| McpMethodError::tool(error.to_string()))?;
            report.operation.clone_from(&args.operation);
            include_daily_content_after_access(paths, guard, &mut report, args.include_content)?;
            serde_json::to_value(report)
                .map_err(|error| McpMethodError::internal(error.to_string()))
        }
        "list" | "range" => {
            let items = list_daily_notes(
                paths,
                args.from.as_deref(),
                args.to.as_deref(),
                args.week,
                args.month,
            )
            .map_err(|error| McpMethodError::tool(error.to_string()))?
            .into_iter()
            .filter(|item| guard.check_read_path(&item.path).is_ok())
            .collect::<Vec<_>>();
            let mut page = bounded_daily_list(
                items,
                args.limit,
                args.offset,
                args.order.as_deref(),
                args.include_events,
            )?;
            page.as_object_mut()
                .expect("daily list page is an object")
                .insert("operation".to_string(), Value::String(args.operation));
            Ok(page)
        }
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `daily.operation`: {other}"
        ))),
    }
}

/// Read a single daily note through the same scoped MCP access check.
pub fn daily_show(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpDailyShowArgs,
) -> Result<Value, McpMethodError> {
    let report = show_periodic_note(paths, args.date.as_deref(), "daily")
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    mcp_access::check_read_note_access(paths, guard, &report.path)?;
    serde_json::to_value(report).map_err(|error| McpMethodError::internal(error.to_string()))
}

/// List readable daily notes with the legacy MCP pagination contract.
pub fn daily_list(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpDailyListArgs,
) -> Result<Value, McpMethodError> {
    let items = list_daily_notes(
        paths,
        args.from.as_deref(),
        args.to.as_deref(),
        args.week,
        args.month,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?
    .into_iter()
    .filter(|item| guard.check_read_path(&item.path).is_ok())
    .collect::<Vec<_>>();
    bounded_daily_list(
        items,
        args.limit,
        args.offset,
        args.order.as_deref(),
        args.include_events,
    )
}

/// Apply the note-source permission boundary before any MCP note read.
pub fn check_read_markdown_source_access(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    note: &str,
) -> Result<(), McpMethodError> {
    app_check_read_markdown_source_access(paths, guard, note)
        .map_err(|error| McpMethodError::tool(error.to_string()))
}

/// Resolve and read a Markdown note under the MCP caller's source boundary.
pub fn note_get(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpNoteGetArgs,
) -> Result<NoteGetReport, McpMethodError> {
    check_read_markdown_source_access(paths, guard, &args.note)?;
    read_note(
        paths,
        NoteGetOptions {
            note: &args.note,
            mode: parse_note_get_mode(args.mode.as_deref())?,
            section_id: args.section_id.as_deref(),
            heading: args.heading.as_deref(),
            block_ref: args.block_ref.as_deref(),
            lines: args.lines.as_deref(),
            match_pattern: args.match_pattern.as_deref(),
            context: args.context,
            no_frontmatter: args.no_frontmatter,
            raw: args.raw,
        },
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))
}

/// Outline a Markdown note only after the same scoped source check.
pub fn note_outline(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpNoteOutlineArgs,
) -> Result<NoteOutlineReport, McpMethodError> {
    check_read_markdown_source_access(paths, guard, &args.note)?;
    read_note_outline(paths, &args.note, args.section_id.as_deref(), args.depth)
        .map_err(|error| McpMethodError::tool(error.to_string()))
}

/// Build note metadata with backlinks filtered by the caller's read grant.
pub fn note_info(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpNoteInfoArgs,
) -> Result<NoteInfoReport, McpMethodError> {
    mcp_access::check_read_note_access(paths, guard, &args.note)?;
    build_note_info_report(paths, &args.note, Some(&guard.read_filter()))
        .map_err(|error| McpMethodError::tool(error.to_string()))
}

fn parse_note_get_mode(mode: Option<&str>) -> Result<NoteReadMode, McpMethodError> {
    match mode.unwrap_or("markdown") {
        "markdown" => Ok(NoteReadMode::Markdown),
        "html" => Ok(NoteReadMode::Html),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `note_get.mode`: {other}"
        ))),
    }
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
        let mut result =
            crate::browse::build_dataview_query_report_with_guard(paths, dql, None, guard)
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
            let ast = QueryAst::from_note_query(&note_query);
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
    use vulcan_core::paths::initialize_vulcan_dir;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode, TasksQueryGroup};

    #[test]
    fn note_read_workflows_preserve_modes_and_deny_hidden_sources() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(
            paths.config_file(),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(temporary.path().join("Home.md"), "# Home\n\nHello MCP.\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let readable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        let get_args =
            || serde_json::from_value::<McpNoteGetArgs>(json!({"note": "Home.md"})).unwrap();
        assert!(note_get(&paths, &readable, &get_args())
            .unwrap()
            .content
            .contains("Hello MCP"));
        assert!(note_get(&paths, &blind, &get_args()).is_err());
        let html =
            serde_json::from_value::<McpNoteGetArgs>(json!({"note": "Home.md", "mode": "html"}))
                .unwrap();
        assert!(note_get(&paths, &readable, &html)
            .unwrap()
            .content
            .contains("<h1"));
        let invalid = serde_json::from_value::<McpNoteGetArgs>(
            json!({"note": "Home.md", "mode": "unsupported"}),
        )
        .unwrap();
        assert!(matches!(
            note_get(&paths, &readable, &invalid),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
        let outline_args =
            serde_json::from_value::<McpNoteOutlineArgs>(json!({"note": "Home.md"})).unwrap();
        assert_eq!(
            note_outline(&paths, &readable, &outline_args)
                .unwrap()
                .sections
                .len(),
            1
        );
        assert!(note_outline(&paths, &blind, &outline_args).is_err());
        let info_args =
            serde_json::from_value::<McpNoteInfoArgs>(json!({"note": "Home.md"})).unwrap();
        assert_eq!(
            note_info(&paths, &readable, &info_args).unwrap().path,
            "Home.md"
        );
        assert!(note_info(&paths, &blind, &info_args).is_err());
    }

    #[test]
    fn daily_workflows_filter_lists_and_deny_hidden_content_free_reads() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::create_dir_all(temporary.path().join("Journal/Daily")).unwrap();
        fs::write(
            paths.config_file(),
            "[periodic.daily]\nschedule_heading = \"Schedule\"\n[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(
            temporary.path().join("Journal/Daily/2026-04-03.md"),
            "# Friday\n\n## Schedule\n- 09:00 Team standup\n",
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
        let show_args = || {
            serde_json::from_value::<McpDailyArgs>(
                json!({"operation": "show", "date": "2026-04-03", "include_content": false}),
            )
            .unwrap()
        };
        let shown = daily(&paths, &readable, show_args()).unwrap();
        assert_eq!(shown["path"], "Journal/Daily/2026-04-03.md");
        assert!(shown["content"].is_null());
        assert!(daily(&paths, &blind, show_args()).is_err());
        let list_args =
            || serde_json::from_value::<McpDailyListArgs>(json!({"from": "2026-04-03"})).unwrap();
        assert_eq!(
            daily_list(&paths, &readable, &list_args()).unwrap()["page"]["total_count"],
            1
        );
        assert_eq!(
            daily_list(&paths, &blind, &list_args()).unwrap()["page"]["total_count"],
            0
        );
        let legacy_show =
            serde_json::from_value::<McpDailyShowArgs>(json!({"date": "2026-04-03"})).unwrap();
        assert_eq!(
            daily_show(&paths, &readable, &legacy_show).unwrap()["path"],
            shown["path"]
        );
    }

    #[test]
    fn task_query_filter_removes_unreadable_flat_and_grouped_rows() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(temporary.path().join(".vulcan")).unwrap();
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        let task = json!({"path": "Private.md", "text": "Hidden"});
        let mut report = TasksQueryResult {
            tasks: vec![task.clone()],
            groups: vec![TasksQueryGroup {
                field: "status".to_string(),
                key: json!("open"),
                tasks: vec![task],
            }],
            result_count: 1,
            hidden_fields: Vec::new(),
            shown_fields: Vec::new(),
            short_mode: false,
            plan: None,
        };
        filter_tasks_query_report(&guard, &mut report);
        assert!(report.tasks.is_empty());
        assert!(report.groups.is_empty());
        assert_eq!(report.result_count, 0);
    }

    #[test]
    fn task_queries_reject_stale_selected_grants() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(
            paths.config_file(),
            "[permissions.profiles.scoped]\nread = \"all\"\nwrite = \"none\"\n",
        )
        .unwrap();
        fs::write(temporary.path().join("Task.md"), "- [ ] Task\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("scoped")).unwrap(),
        );
        let query_args = serde_json::from_value(json!({"query": "not done"})).unwrap();
        assert_eq!(
            task_query(&paths, &guard, &query_args)
                .unwrap()
                .result_count,
            1
        );
        fs::write(
            paths.config_file(),
            "[permissions.profiles.scoped]\nread = \"none\"\nwrite = \"none\"\n",
        )
        .unwrap();
        let list_args = || serde_json::from_value(json!({"source": "inline"})).unwrap();
        assert!(task_query(&paths, &guard, &query_args).is_err());
        assert!(task_list(&paths, &guard, list_args()).is_err());
        let current = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("scoped")).unwrap(),
        );
        assert_eq!(
            task_query(&paths, &current, &query_args)
                .unwrap()
                .result_count,
            0
        );
        assert_eq!(
            task_list(&paths, &current, list_args())
                .unwrap()
                .result_count,
            0
        );
    }

    #[test]
    fn task_queries_preserve_tag_grants_and_denies_before_shaping() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"tag:visible\"], deny = [\"tag:secret\"] }\nwrite = \"none\"\n").unwrap();
        for (path, tags) in [
            ("AHidden.md", "[]"),
            ("BDenied.md", "[visible, secret]"),
            ("CVisible.md", "[visible]"),
        ] {
            fs::write(
                temporary.path().join(path),
                format!("---\ntags: {tags}\n---\n- [ ] Task in {path}\n"),
            )
            .unwrap();
        }
        scan_vault(&paths, ScanMode::Full).unwrap();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("scoped")).unwrap(),
        );
        let source = "not done\ngroup by path\nlimit 1";
        // Core SQL selection has document tags; task-row tags need not contain
        // the frontmatter tags authorizing their enclosing note.
        let selected = crate::tasks::build_tasks_query_result_with_filter(
            &paths,
            source,
            Some(&guard.read_filter()),
        )
        .unwrap();
        assert_eq!(selected.result_count, 1);
        assert_eq!(selected.tasks[0]["path"], "CVisible.md");
        let query_args = serde_json::from_value(json!({"query": source})).unwrap();
        let list_args =
            serde_json::from_value(json!({"source": "inline", "filter": source})).unwrap();
        for report in [
            task_query(&paths, &guard, &query_args).unwrap(),
            task_list(&paths, &guard, list_args).unwrap(),
        ] {
            assert_eq!(report.result_count, 1);
            assert_eq!(report.tasks[0]["path"], "CVisible.md");
            assert_eq!(report.groups.len(), 1);
            assert_eq!(report.groups[0].tasks.len(), 1);
            assert_eq!(report.groups[0].tasks[0]["path"], "CVisible.md");
        }
    }

    #[test]
    fn task_queries_apply_read_scope_before_limits_and_groups() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\nwrite = \"none\"\n").unwrap();
        for folder in ["Hidden", "Public"] {
            fs::create_dir_all(temporary.path().join(folder)).unwrap();
            fs::write(
                temporary.path().join(folder).join(format!("{folder}.md")),
                format!("- [ ] {folder} task\n"),
            )
            .unwrap();
        }
        scan_vault(&paths, ScanMode::Full).unwrap();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("scoped")).unwrap(),
        );
        let source = "not done\ngroup by path\nlimit 1";
        let unfiltered = crate::tasks::build_tasks_query_result(&paths, source).unwrap();
        assert_eq!(unfiltered.tasks[0]["path"], "Hidden/Hidden.md");
        let query_args = serde_json::from_value(json!({"query": source})).unwrap();
        let list_args =
            serde_json::from_value(json!({"source": "inline", "filter": source})).unwrap();
        for report in [
            task_query(&paths, &guard, &query_args).unwrap(),
            task_list(&paths, &guard, list_args).unwrap(),
        ] {
            assert_eq!(report.result_count, 1);
            assert_eq!(report.tasks[0]["path"], "Public/Public.md");
            assert_eq!(report.groups.len(), 1);
            assert_eq!(report.groups[0].tasks.len(), 1);
            assert_eq!(report.groups[0].tasks[0]["path"], "Public/Public.md");
        }
    }

    #[test]
    fn task_list_and_query_apply_the_same_read_filter_and_validate_source() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(
            paths.config_file(),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(temporary.path().join("Tasks.md"), "- [ ] Visible task\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let readable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        let list_args =
            || serde_json::from_value::<McpTaskListArgs>(json!({"source": "inline"})).unwrap();
        assert_eq!(
            task_list(&paths, &readable, list_args())
                .unwrap()
                .result_count,
            1
        );
        assert_eq!(
            task_list(&paths, &blind, list_args()).unwrap().result_count,
            0
        );
        let query_args = serde_json::from_value::<McpTaskQueryArgs>(json!({"query": ""})).unwrap();
        assert_eq!(
            task_query(&paths, &readable, &query_args)
                .unwrap()
                .result_count,
            1
        );
        assert_eq!(
            task_query(&paths, &blind, &query_args)
                .unwrap()
                .result_count,
            0
        );
        let invalid =
            serde_json::from_value::<McpTaskListArgs>(json!({"source": "unknown"})).unwrap();
        assert!(matches!(
            task_list(&paths, &readable, invalid),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
    }

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

    #[test]
    fn daily_list_bounds_sorting_projection_and_invalid_options() {
        let items = vec![
            json!({"date": "2026-09-01", "path": "Daily/2026-09-01.md", "events": [1]}),
            json!({"date": "2026-09-03", "path": "Daily/2026-09-03.md", "events": [2]}),
            json!({"date": "2026-09-02", "path": "Daily/2026-09-02.md", "events": [3]}),
        ];
        let first = bounded_daily_list(items.clone(), 1, 0, None, false).unwrap();
        assert_eq!(first["items"][0]["date"], "2026-09-03");
        assert!(first["items"][0].get("events").is_none());
        assert_eq!(first["page"]["total_count"], 3);
        assert_eq!(first["page"]["next_offset"], 1);
        let ascending = bounded_daily_list(items, 2, 1, Some("asc"), true).unwrap();
        assert_eq!(ascending["items"][0]["date"], "2026-09-02");
        assert_eq!(ascending["items"][1]["events"], json!([2]));
        for (limit, order) in [(0, None), (201, None), (1, Some("newest"))] {
            assert!(matches!(
                bounded_daily_list(vec![json!({"date": "2026-09-01"})], limit, 0, order, false),
                Err(McpMethodError::JsonRpc { code: -32602, .. })
            ));
        }
    }

    #[test]
    fn daily_content_requires_read_access_even_when_content_is_omitted() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(temporary.path().join(".vulcan")).unwrap();
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(temporary.path().join("2026-09-01.md"), "# Daily\n").unwrap();
        let readable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        let report = || DailyNoteReadReport {
            operation: "show".to_string(),
            date: Some("2026-09-01".to_string()),
            path: Some("2026-09-01.md".to_string()),
            exists: true,
            content: None,
            reason: None,
        };
        assert!(matches!(
            include_daily_content_after_access(&paths, &blind, &mut report(), false),
            Err(McpMethodError::Tool { .. })
        ));
        let mut visible = report();
        include_daily_content_after_access(&paths, &readable, &mut visible, true).unwrap();
        assert_eq!(visible.content.as_deref(), Some("# Daily\n"));
    }
}
