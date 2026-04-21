use crate::browse::{
    build_dataview_eval_report, build_dataview_inline_report, build_dataview_query_js_report,
    build_dataview_query_report,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use vulcan_core::{
    query_graph_analytics_with_filter, query_notes_with_filter, query_related_notes_with_filter,
    resolve_permission_profile, search_vault_with_filter, NoteQuery, PermissionFilter,
    PermissionGuard, ProfilePermissionGuard, RelatedNotesQuery, SearchQuery, SearchSort,
    VaultPaths, WatchReport,
};

#[derive(Debug, Clone, Default, Serialize)]
pub struct ServeHealthState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_watch_report: Option<WatchReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeRequest {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeRouteOptions {
    pub permissions: Option<String>,
    pub watch_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct ServeResponse {
    pub status: u16,
    pub body: Value,
}

impl ServeResponse {
    fn ok(body: Value) -> Self {
        Self { status: 200, body }
    }

    fn error(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({
                "ok": false,
                "error": message.into(),
            }),
        }
    }
}

#[allow(clippy::too_many_lines)]
pub fn route_request(
    paths: &VaultPaths,
    options: &ServeRouteOptions,
    state: &ServeHealthState,
    request: &ServeRequest,
) -> ServeResponse {
    if request.method != "GET" {
        return ServeResponse::error(405, "only GET requests are supported");
    }
    let permissions = match serve_permission_guard(paths, options) {
        Ok(permissions) => permissions,
        Err(error) => return ServeResponse::error(500, error),
    };
    let read_filter = match serve_read_filter(paths, options) {
        Ok(filter) => filter,
        Err(error) => return ServeResponse::error(500, error),
    };

    match request.path.as_str() {
        "/" => ServeResponse::ok(json!({
            "ok": true,
            "service": "vulcan",
            "endpoints": [
                "/health",
                "/search",
                "/notes",
                "/graph/stats",
                "/related",
                "/dataview/inline",
                "/dataview/query",
                "/dataview/query-js",
                "/dataview/eval"
            ],
        })),
        "/health" => ServeResponse::ok(json!({
            "ok": true,
            "watch_enabled": options.watch_enabled,
            "watch_error": state.watch_error,
            "last_watch_report": state.last_watch_report,
        })),
        "/search" => {
            let Some(query) = first_param(&request.query, "q") else {
                return ServeResponse::error(400, "missing required query parameter: q");
            };
            let sort = match parse_optional_search_sort(&request.query, "sort") {
                Ok(sort) => sort,
                Err(error) => return ServeResponse::error(400, error),
            };
            let search_query = SearchQuery {
                text: query.to_string(),
                tag: first_param(&request.query, "tag").map(ToOwned::to_owned),
                path_prefix: first_param(&request.query, "path_prefix").map(ToOwned::to_owned),
                has_property: first_param(&request.query, "has_property").map(ToOwned::to_owned),
                filters: request.query.get("where").cloned().unwrap_or_default(),
                provider: first_param(&request.query, "provider").map(ToOwned::to_owned),
                mode: match first_param(&request.query, "mode") {
                    Some("hybrid") => vulcan_core::search::SearchMode::Hybrid,
                    _ => vulcan_core::search::SearchMode::Keyword,
                },
                sort,
                match_case: parse_optional_bool(&request.query, "match_case"),
                limit: parse_optional_usize(&request.query, "limit"),
                context_size: parse_optional_usize(&request.query, "context_size").unwrap_or(18),
                raw_query: parse_optional_bool(&request.query, "raw_query").unwrap_or(false),
                fuzzy: parse_optional_bool(&request.query, "fuzzy").unwrap_or(false),
                explain: parse_optional_bool(&request.query, "explain").unwrap_or(false),
            };
            match search_vault_with_filter(paths, &search_query, read_filter.as_ref()) {
                Ok(report) => ServeResponse::ok(json!({ "ok": true, "result": report })),
                Err(error) => ServeResponse::error(500, error.to_string()),
            }
        }
        "/notes" => {
            let filters = request.query.get("where").cloned().unwrap_or_default();
            let query = NoteQuery {
                filters,
                sort_by: first_param(&request.query, "sort").map(ToOwned::to_owned),
                sort_descending: parse_optional_bool(&request.query, "desc").unwrap_or(false),
            };
            match query_notes_with_filter(paths, &query, read_filter.as_ref()) {
                Ok(mut report) => {
                    let offset = parse_optional_usize(&request.query, "offset").unwrap_or(0);
                    let limit = parse_optional_usize(&request.query, "limit");
                    let start = offset.min(report.notes.len());
                    let end = limit.map_or(report.notes.len(), |limit| {
                        start.saturating_add(limit).min(report.notes.len())
                    });
                    report.notes = report.notes[start..end].to_vec();
                    ServeResponse::ok(json!({ "ok": true, "result": report }))
                }
                Err(error) => ServeResponse::error(500, error.to_string()),
            }
        }
        "/graph/stats" => match query_graph_analytics_with_filter(paths, read_filter.as_ref()) {
            Ok(report) => ServeResponse::ok(json!({ "ok": true, "result": report })),
            Err(error) => ServeResponse::error(500, error.to_string()),
        },
        "/related" => {
            let Some(note) = first_param(&request.query, "note") else {
                return ServeResponse::error(400, "missing required query parameter: note");
            };
            let query = RelatedNotesQuery {
                provider: first_param(&request.query, "provider").map(ToOwned::to_owned),
                note: note.to_string(),
                limit: parse_optional_usize(&request.query, "limit").unwrap_or(10),
            };
            match query_related_notes_with_filter(paths, &query, read_filter.as_ref()) {
                Ok(report) => ServeResponse::ok(json!({ "ok": true, "result": report })),
                Err(error) => ServeResponse::error(500, error.to_string()),
            }
        }
        "/dataview/inline" => {
            let Some(file) = first_param(&request.query, "file") else {
                return ServeResponse::error(400, "missing required query parameter: file");
            };
            match build_dataview_inline_report(paths, file, Some(&permissions)) {
                Ok(report) => ServeResponse::ok(json!({ "ok": true, "result": report })),
                Err(error) => ServeResponse::error(500, error.to_string()),
            }
        }
        "/dataview/query" => {
            let Some(dql) = first_param(&request.query, "dql") else {
                return ServeResponse::error(400, "missing required query parameter: dql");
            };
            match build_dataview_query_report(paths, dql, None, read_filter.as_ref()) {
                Ok(result) => ServeResponse::ok(json!({ "ok": true, "result": result })),
                Err(error) => ServeResponse::error(500, error.to_string()),
            }
        }
        "/dataview/query-js" => {
            let Some(js) = first_param(&request.query, "js") else {
                return ServeResponse::error(400, "missing required query parameter: js");
            };
            match build_dataview_query_js_report(
                paths,
                js,
                first_param(&request.query, "file"),
                options.permissions.as_deref(),
            ) {
                Ok(result) => ServeResponse::ok(json!({ "ok": true, "result": result })),
                Err(error) => ServeResponse::error(500, error.to_string()),
            }
        }
        "/dataview/eval" => {
            let Some(file) = first_param(&request.query, "file") else {
                return ServeResponse::error(400, "missing required query parameter: file");
            };
            match build_dataview_eval_report(
                paths,
                file,
                parse_optional_usize(&request.query, "block"),
                options.permissions.as_deref(),
                Some(&permissions),
            ) {
                Ok(report) => ServeResponse::ok(json!({ "ok": true, "result": report })),
                Err(error) => ServeResponse::error(500, error.to_string()),
            }
        }
        _ => ServeResponse::error(404, "unknown endpoint"),
    }
}

fn serve_read_filter(
    paths: &VaultPaths,
    options: &ServeRouteOptions,
) -> Result<Option<PermissionFilter>, String> {
    let filter = serve_permission_guard(paths, options)?.read_filter();
    Ok((!filter.path_permission().is_unrestricted()).then_some(filter))
}

fn serve_permission_guard(
    paths: &VaultPaths,
    options: &ServeRouteOptions,
) -> Result<ProfilePermissionGuard, String> {
    let selection = resolve_permission_profile(paths, options.permissions.as_deref())
        .map_err(|error| error.to_string())?;
    Ok(ProfilePermissionGuard::new(paths, selection))
}

fn first_param<'a>(params: &'a HashMap<String, Vec<String>>, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(|values| values.first())
        .map(String::as_str)
}

fn parse_optional_usize(params: &HashMap<String, Vec<String>>, key: &str) -> Option<usize> {
    first_param(params, key).and_then(|value| value.parse::<usize>().ok())
}

fn parse_optional_bool(params: &HashMap<String, Vec<String>>, key: &str) -> Option<bool> {
    first_param(params, key).and_then(|value| match value {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    })
}

fn parse_optional_search_sort(
    params: &HashMap<String, Vec<String>>,
    key: &str,
) -> Result<Option<SearchSort>, String> {
    let Some(value) = first_param(params, key) else {
        return Ok(None);
    };

    let sort = match value {
        "relevance" => SearchSort::Relevance,
        "path-asc" => SearchSort::PathAsc,
        "path-desc" => SearchSort::PathDesc,
        "modified-newest" => SearchSort::ModifiedNewest,
        "modified-oldest" => SearchSort::ModifiedOldest,
        "created-newest" => SearchSort::CreatedNewest,
        "created-oldest" => SearchSort::CreatedOldest,
        _ => {
            return Err(format!(
                "invalid search sort `{value}`; expected relevance, path-asc, path-desc, modified-newest, modified-oldest, created-newest, or created-oldest"
            ))
        }
    };

    Ok(Some(sort))
}

#[cfg(test)]
mod tests {
    use super::{route_request, ServeHealthState, ServeRequest, ServeRouteOptions};
    use std::collections::HashMap;
    use std::fs;
    use tempfile::TempDir;
    use vulcan_core::{scan_vault, ScanMode, VaultPaths};

    #[test]
    fn route_request_reports_missing_search_query() {
        let temp_dir = TempDir::new().expect("temp dir");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(&vault_root).expect("vault root should exist");
        fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should exist");
        let paths = VaultPaths::new(&vault_root);

        let response = route_request(
            &paths,
            &ServeRouteOptions {
                permissions: None,
                watch_enabled: false,
            },
            &ServeHealthState::default(),
            &ServeRequest {
                method: "GET".to_string(),
                path: "/search".to_string(),
                query: HashMap::new(),
            },
        );

        assert_eq!(response.status, 400);
        assert_eq!(response.body["ok"], false);
    }

    #[test]
    fn route_request_search_returns_shared_json_shape() {
        let temp_dir = TempDir::new().expect("temp dir");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");
        let paths = VaultPaths::new(&vault_root);

        let response = route_request(
            &paths,
            &ServeRouteOptions {
                permissions: None,
                watch_enabled: false,
            },
            &ServeHealthState::default(),
            &ServeRequest {
                method: "GET".to_string(),
                path: "/search".to_string(),
                query: HashMap::from([("q".to_string(), vec!["dashboard".to_string()])]),
            },
        );

        assert_eq!(response.status, 200);
        assert_eq!(response.body["ok"], true);
        assert_eq!(
            response.body["result"]["hits"][0]["document_path"],
            "Home.md"
        );
    }

    fn copy_fixture_vault(name: &str, destination: &std::path::Path) {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_recursive(&source, destination);
        fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
    }

    fn copy_dir_recursive(source: &std::path::Path, destination: &std::path::Path) {
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
