//! Named external-content routes and topology validation.

use crate::AppError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;
use vulcan_core::config::{
    IntegrationMissingPolicyConfig, IntegrationRouteConfig, IntegrationRouteDirectionConfig,
    OutlinePublishProfileConfig, VaultConfig,
};
use vulcan_core::VaultPaths;

const ROUTE_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteDiagnostic {
    pub severity: RouteDiagnosticSeverity,
    pub route: String,
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteValidationReport {
    pub valid: bool,
    pub route_count: usize,
    pub diagnostics: Vec<RouteDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationRouteView {
    pub name: String,
    #[serde(flatten)]
    pub config: IntegrationRouteConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRunRecord {
    pub operation_id: String,
    pub started_unix_seconds: u64,
    pub completed_unix_seconds: Option<u64>,
    pub dry_run: bool,
    pub outcome: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRuntimeState {
    pub version: u32,
    pub route: String,
    pub last_run: Option<RouteRunRecord>,
    pub last_successful_unix_seconds: Option<u64>,
    #[serde(default)]
    pub history: Vec<RouteRunRecord>,
}

pub struct RouteRunLock {
    file: File,
    state_path: PathBuf,
    state: RouteRuntimeState,
}

impl RouteRunLock {
    pub fn finish(mut self, outcome: &str, message: Option<String>) -> Result<(), AppError> {
        let now = unix_seconds()?;
        let completed_record = {
            let record = self.state.last_run.as_mut().ok_or_else(|| {
                AppError::operation("route run state omitted its active operation")
            })?;
            record.completed_unix_seconds = Some(now);
            record.outcome = outcome.to_string();
            record.message = message;
            record.clone()
        };
        if outcome == "completed" {
            self.state.last_successful_unix_seconds = Some(now);
        }
        self.state.history.push(completed_record);
        trim_route_history(&mut self.state.history);
        save_route_state(&self.state_path, &self.state)
    }
}

impl Drop for RouteRunLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn begin_route_run(
    paths: &VaultPaths,
    route: &str,
    dry_run: bool,
) -> Result<RouteRunLock, AppError> {
    validate_state_route_name(route)?;
    let state_path = route_state_path(paths, route)?;
    let directory = state_path.parent().expect("route state has a parent");
    fs::create_dir_all(directory).map_err(AppError::operation)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(format!("{route}.lock")))
        .map_err(AppError::operation)?;
    file.try_lock_exclusive().map_err(|_| {
        AppError::operation(format!("integration route `{route}` is already running"))
    })?;
    let mut state = load_route_runtime_state(paths, route)?.unwrap_or(RouteRuntimeState {
        version: ROUTE_STATE_VERSION,
        route: route.to_string(),
        last_run: None,
        last_successful_unix_seconds: None,
        history: Vec::new(),
    });
    let now = unix_seconds()?;
    if let Some(previous) = state
        .last_run
        .as_mut()
        .filter(|run| run.completed_unix_seconds.is_none())
    {
        previous.completed_unix_seconds = Some(now);
        previous.outcome = "interrupted".to_string();
        previous.message = Some("a later run recovered an incomplete route checkpoint".to_string());
        state.history.push(previous.clone());
        trim_route_history(&mut state.history);
    }
    state.last_run = Some(RouteRunRecord {
        operation_id: ulid::Ulid::new().to_string(),
        started_unix_seconds: now,
        completed_unix_seconds: None,
        dry_run,
        outcome: "running".to_string(),
        message: None,
    });
    save_route_state(&state_path, &state)?;
    Ok(RouteRunLock {
        file,
        state_path,
        state,
    })
}

fn trim_route_history(history: &mut Vec<RouteRunRecord>) {
    const MAX_HISTORY: usize = 50;
    if history.len() > MAX_HISTORY {
        history.drain(..history.len() - MAX_HISTORY);
    }
}

pub fn load_route_runtime_state(
    paths: &VaultPaths,
    route: &str,
) -> Result<Option<RouteRuntimeState>, AppError> {
    let path = route_state_path(paths, route)?;
    if !path.exists() {
        return Ok(None);
    }
    let state: RouteRuntimeState =
        serde_json::from_slice(&fs::read(&path).map_err(AppError::operation)?)
            .map_err(|_| AppError::operation("integration route state contains malformed JSON"))?;
    if state.version != ROUTE_STATE_VERSION || state.route != route {
        return Err(AppError::operation(
            "integration route state belongs to another route or version",
        ));
    }
    Ok(Some(state))
}

fn route_state_path(paths: &VaultPaths, route: &str) -> Result<PathBuf, AppError> {
    validate_state_route_name(route)?;
    Ok(paths
        .vulcan_dir()
        .join("integrations")
        .join("routes")
        .join(format!("{route}.json")))
}

fn validate_state_route_name(route: &str) -> Result<(), AppError> {
    if route.is_empty()
        || !route
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::operation(
            "route names may contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    Ok(())
}

fn save_route_state(path: &Path, state: &RouteRuntimeState) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(state).map_err(AppError::operation)?;
    let parent = path.parent().expect("route state parent");
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    temporary.write_all(&bytes).map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    temporary
        .persist(path)
        .map_err(|error| AppError::operation(error.error))?;
    #[cfg(unix)]
    sync_parent_directory(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), AppError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(AppError::operation)
}

fn unix_seconds() -> Result<u64, AppError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(AppError::operation)
}

#[must_use]
pub fn list_routes(config: &VaultConfig) -> Vec<IntegrationRouteView> {
    config
        .integrations
        .routes
        .iter()
        .map(|(name, route)| IntegrationRouteView {
            name: name.clone(),
            config: route.clone(),
        })
        .collect()
}

#[must_use]
pub fn route(config: &VaultConfig, name: &str) -> Option<IntegrationRouteView> {
    config
        .integrations
        .routes
        .get(name)
        .cloned()
        .map(|config| IntegrationRouteView {
            name: name.to_string(),
            config,
        })
}

#[must_use]
pub fn validate_routes(config: &VaultConfig) -> RouteValidationReport {
    let mut diagnostics = Vec::new();
    for (name, route) in &config.integrations.routes {
        validate_route(config, name, route, &mut diagnostics);
    }
    validate_route_ownership(&config.integrations.routes, &mut diagnostics);
    let valid = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != RouteDiagnosticSeverity::Error);
    RouteValidationReport {
        valid,
        route_count: config.integrations.routes.len(),
        diagnostics,
    }
}

#[allow(clippy::too_many_lines)]
fn validate_route(
    config: &VaultConfig,
    name: &str,
    route: &IntegrationRouteConfig,
    diagnostics: &mut Vec<RouteDiagnostic>,
) {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        error(
            diagnostics,
            name,
            "name",
            "route names may contain only ASCII letters, digits, `_`, and `-`",
        );
    }
    if route.profile.trim().is_empty() {
        error(
            diagnostics,
            name,
            "profile",
            "an Outline profile is required",
        );
        return;
    }
    if !route
        .profile
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        error(
            diagnostics,
            name,
            "profile",
            "Outline profile names used by routes may contain only ASCII letters, digits, `_`, and `-`",
        );
    }
    let Some(profile) = config.publish.outline.profiles.get(&route.profile) else {
        error(
            diagnostics,
            name,
            "profile",
            format!("Outline profile `{}` does not exist", route.profile),
        );
        return;
    };
    validate_profile(name, route.direction, profile, diagnostics);

    if route.direction.pulls() {
        match route.local_root.as_deref() {
            Some(path) if valid_local_root(path) => {}
            Some(_) => error(
                diagnostics,
                name,
                "local_root",
                "pulling requires a non-internal relative vault directory",
            ),
            None => error(
                diagnostics,
                name,
                "local_root",
                "pulling requires `local_root`",
            ),
        }
    }
    if route.max_depth.is_some() && route.remote_roots.is_empty() {
        error(
            diagnostics,
            name,
            "max_depth",
            "`max_depth` requires at least one `remote_roots` entry",
        );
    }
    validate_remote_ids(name, "remote_roots", &route.remote_roots, diagnostics);
    validate_remote_ids(
        name,
        "excluded_documents",
        &route.excluded_documents,
        diagnostics,
    );
    validate_document_bindings(name, route, diagnostics);
    let roots = route.remote_roots.iter().collect::<BTreeSet<_>>();
    for excluded in &route.excluded_documents {
        if roots.contains(excluded) {
            error(
                diagnostics,
                name,
                "excluded_documents",
                format!("remote document `{excluded}` is both included and excluded"),
            );
        }
    }
    if let Some(schedule) = route.schedule.as_deref() {
        if !valid_schedule(schedule) {
            error(
                diagnostics,
                name,
                "schedule",
                "use `@hourly`, `@daily`, `@weekly`, or `every <N>[m|h|d]`",
            );
        }
    }
    validate_archive_policy(
        name,
        "missing",
        route.missing_policy,
        route.missing_archive.as_deref(),
        diagnostics,
    );
    validate_archive_policy(
        name,
        "stale_attachment",
        route.stale_attachment_policy,
        route.stale_attachment_archive.as_deref(),
        diagnostics,
    );
    for (field, limit) in [
        ("max_documents", route.max_documents),
        ("max_content_bytes", route.max_content_bytes),
        ("max_attachments", route.max_attachments),
        ("max_attachment_bytes", route.max_attachment_bytes),
        (
            "max_total_attachment_bytes",
            route.max_total_attachment_bytes,
        ),
    ] {
        if limit == Some(0) {
            error(
                diagnostics,
                name,
                field,
                "work limits must be greater than zero",
            );
        }
    }
}

fn validate_archive_policy(
    route: &str,
    field: &str,
    policy: IntegrationMissingPolicyConfig,
    directory: Option<&Path>,
    diagnostics: &mut Vec<RouteDiagnostic>,
) {
    match (policy, directory) {
        (IntegrationMissingPolicyConfig::Archive, Some(path)) if valid_local_root(path) => {}
        (IntegrationMissingPolicyConfig::Archive, _) => error(
            diagnostics,
            route,
            format!("{field}_archive"),
            "archive policy requires a safe relative archive directory",
        ),
        (IntegrationMissingPolicyConfig::Retain, Some(_)) => error(
            diagnostics,
            route,
            format!("{field}_archive"),
            "an archive directory is only valid with archive policy",
        ),
        (IntegrationMissingPolicyConfig::Retain, None) => {}
    }
}

fn validate_document_bindings(
    route_name: &str,
    route: &IntegrationRouteConfig,
    diagnostics: &mut Vec<RouteDiagnostic>,
) {
    let mut local_paths = BTreeSet::new();
    for (remote_id, local_path) in &route.document_bindings {
        if remote_id.trim().is_empty() {
            error(
                diagnostics,
                route_name,
                "document_bindings",
                "bound remote document ids cannot be empty",
            );
        }
        let valid = route.local_root.as_deref().is_some_and(|root| {
            valid_local_root(local_path)
                && local_path.extension().and_then(|value| value.to_str()) == Some("md")
                && local_path.starts_with(root)
        });
        if !valid {
            error(
                diagnostics,
                route_name,
                "document_bindings",
                format!("binding `{remote_id}` must target a Markdown file beneath `local_root`"),
            );
        }
        let key = local_path
            .to_string_lossy()
            .nfkc()
            .flat_map(char::to_lowercase)
            .collect::<String>();
        if !local_paths.insert(key) {
            error(
                diagnostics,
                route_name,
                "document_bindings",
                "multiple remote documents target the same portable local path",
            );
        }
    }
}

fn validate_profile(
    route_name: &str,
    direction: IntegrationRouteDirectionConfig,
    profile: &OutlinePublishProfileConfig,
    diagnostics: &mut Vec<RouteDiagnostic>,
) {
    for (field, value) in [
        ("base_url", profile.base_url.as_deref()),
        ("collection_id", profile.collection_id.as_deref()),
        ("token_env", profile.token_env.as_deref()),
    ] {
        if value.is_none_or(str::is_empty) {
            error(
                diagnostics,
                route_name,
                format!("profile.{field}"),
                format!("Outline route profile requires `{field}`"),
            );
        }
    }
    if direction.pushes() {
        let selectors = usize::from(profile.query.is_some())
            + usize::from(profile.query_json.is_some())
            + usize::from(profile.selection.is_some());
        if selectors != 1 {
            error(
                diagnostics,
                route_name,
                "profile.selection",
                "push routes require exactly one of `query`, `query_json`, or `selection`",
            );
        }
    }
}

fn validate_remote_ids(
    route: &str,
    field: &str,
    values: &[String],
    diagnostics: &mut Vec<RouteDiagnostic>,
) {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() {
            error(
                diagnostics,
                route,
                field,
                "remote document ids cannot be empty",
            );
        } else if !seen.insert(value) {
            error(
                diagnostics,
                route,
                field,
                format!("remote document id `{value}` is duplicated"),
            );
        }
    }
}

fn validate_route_ownership(
    routes: &BTreeMap<String, IntegrationRouteConfig>,
    diagnostics: &mut Vec<RouteDiagnostic>,
) {
    let enabled = routes
        .iter()
        .filter(|(_, route)| route.enabled)
        .collect::<Vec<_>>();
    for (index, (left_name, left)) in enabled.iter().enumerate() {
        for (right_name, right) in enabled.iter().skip(index + 1) {
            if left.direction.pulls()
                && right.direction.pulls()
                && local_roots_overlap(left.local_root.as_deref(), right.local_root.as_deref())
            {
                error(
                    diagnostics,
                    *left_name,
                    "local_root",
                    format!("overlaps enabled route `{right_name}`; a local subtree must have one pull owner"),
                );
                error(
                    diagnostics,
                    *right_name,
                    "local_root",
                    format!("overlaps enabled route `{left_name}`; a local subtree must have one pull owner"),
                );
            }
            if left.profile == right.profile && left.direction.pushes() && right.direction.pushes()
            {
                error(
                    diagnostics,
                    *left_name,
                    "profile",
                    format!("shares push profile state with enabled route `{right_name}`"),
                );
                error(
                    diagnostics,
                    *right_name,
                    "profile",
                    format!("shares push profile state with enabled route `{left_name}`"),
                );
            }
            if left.profile == right.profile
                && left.direction.pulls()
                && right.direction.pulls()
                && remote_selections_overlap(left, right)
            {
                error(
                    diagnostics,
                    *left_name,
                    "remote_roots",
                    format!("overlaps the remote selection owned by enabled route `{right_name}`"),
                );
                error(
                    diagnostics,
                    *right_name,
                    "remote_roots",
                    format!("overlaps the remote selection owned by enabled route `{left_name}`"),
                );
            }
        }
    }
}

fn valid_local_root(path: &Path) -> bool {
    let rendered = path.to_string_lossy().replace('\\', "/");
    !rendered.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        && ![".vulcan", ".obsidian", ".git"]
            .iter()
            .any(|internal| rendered == *internal || rendered.starts_with(&format!("{internal}/")))
}

fn local_roots_overlap(left: Option<&Path>, right: Option<&Path>) -> bool {
    left.zip(right)
        .is_some_and(|(left, right)| left.starts_with(right) || right.starts_with(left))
}

fn remote_selections_overlap(
    left: &IntegrationRouteConfig,
    right: &IntegrationRouteConfig,
) -> bool {
    left.remote_roots.is_empty()
        || right.remote_roots.is_empty()
        || left
            .remote_roots
            .iter()
            .any(|root| right.remote_roots.contains(root))
}

fn valid_schedule(value: &str) -> bool {
    route_schedule_interval_seconds(value).is_some()
}

#[must_use]
pub fn route_schedule_interval_seconds(value: &str) -> Option<u64> {
    let value = value.trim();
    if value == "@hourly" {
        return Some(60 * 60);
    }
    if value == "@daily" {
        return Some(24 * 60 * 60);
    }
    if value == "@weekly" {
        return Some(7 * 24 * 60 * 60);
    }
    if let Some(interval) = value.strip_prefix("every ") {
        let (digits, unit) = interval.split_at_checked(interval.len().saturating_sub(1))?;
        let number = digits.parse::<u64>().ok().filter(|number| *number > 0)?;
        let multiplier = match unit {
            "m" => 60,
            "h" => 60 * 60,
            "d" => 24 * 60 * 60,
            _ => return None,
        };
        return number.checked_mul(multiplier);
    }
    None
}

#[must_use]
pub fn route_is_due(schedule: &str, last_successful_unix_seconds: Option<u64>, now: u64) -> bool {
    let Some(interval) = route_schedule_interval_seconds(schedule) else {
        return false;
    };
    last_successful_unix_seconds.is_none_or(|last| now.saturating_sub(last) >= interval)
}

fn error(
    diagnostics: &mut Vec<RouteDiagnostic>,
    route: impl Into<String>,
    field: impl Into<String>,
    message: impl Into<String>,
) {
    diagnostics.push(RouteDiagnostic {
        severity: RouteDiagnosticSeverity::Error,
        route: route.into(),
        field: field.into(),
        message: message.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use vulcan_core::config::{IntegrationRouteAuthorityConfig, IntegrationRouteDirectionConfig};
    use vulcan_core::{initialize_vulcan_dir, VaultPaths};

    fn profile() -> OutlinePublishProfileConfig {
        OutlinePublishProfileConfig {
            base_url: Some("https://outline.example".into()),
            collection_id: Some("collection".into()),
            token_env: Some("OUTLINE_TOKEN".into()),
            query: Some("path:Players".into()),
            ..OutlinePublishProfileConfig::default()
        }
    }

    fn route(local_root: &str) -> IntegrationRouteConfig {
        IntegrationRouteConfig {
            profile: "players".into(),
            local_root: Some(PathBuf::from(local_root)),
            authority: IntegrationRouteAuthorityConfig::Review,
            ..IntegrationRouteConfig::default()
        }
    }

    #[test]
    fn validates_a_complete_outline_route() {
        let mut config = VaultConfig::default();
        config
            .publish
            .outline
            .profiles
            .insert("players".into(), profile());
        config
            .integrations
            .routes
            .insert("campaign".into(), route("Players/Campaign"));

        assert_eq!(
            validate_routes(&config),
            RouteValidationReport {
                valid: true,
                route_count: 1,
                diagnostics: Vec::new(),
            }
        );
    }

    #[test]
    fn rejects_overlapping_local_remote_and_push_ownership() {
        let mut config = VaultConfig::default();
        config
            .publish
            .outline
            .profiles
            .insert("players".into(), profile());
        let mut first = route("Players");
        first.remote_roots = vec!["root".into()];
        let mut second = route("Players/Campaign");
        second.remote_roots = vec!["root".into()];
        config.integrations.routes.insert("one".into(), first);
        config.integrations.routes.insert("two".into(), second);

        let report = validate_routes(&config);
        assert!(!report.valid);
        assert_eq!(report.diagnostics.len(), 6);
    }

    #[test]
    fn pull_only_route_does_not_require_a_publish_selector() {
        let mut config = VaultConfig::default();
        let mut profile = profile();
        profile.query = None;
        config
            .publish
            .outline
            .profiles
            .insert("players".into(), profile);
        let mut route = route("Players");
        route.direction = IntegrationRouteDirectionConfig::Pull;
        config.integrations.routes.insert("pull".into(), route);

        assert!(validate_routes(&config).valid);
    }

    #[test]
    fn schedule_validation_accepts_supported_forms() {
        for value in ["@hourly", "@daily", "@weekly", "every 15m"] {
            assert!(valid_schedule(value), "{value}");
        }
        for value in [
            "hourly",
            "every 0m",
            "every ten minutes",
            "* * *",
            "0 3 * * *",
        ] {
            assert!(!valid_schedule(value), "{value}");
        }
        assert!(route_is_due("every 15m", None, 1_000));
        assert!(!route_is_due("every 15m", Some(500), 1_000));
        assert!(route_is_due("every 15m", Some(100), 1_000));
    }

    #[test]
    fn route_runtime_state_checkpoints_completion_and_prevents_concurrent_runs() {
        let temp = tempdir().unwrap();
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).unwrap();

        let run = begin_route_run(&paths, "players", false).unwrap();
        let running = load_route_runtime_state(&paths, "players")
            .unwrap()
            .unwrap();
        assert_eq!(running.last_run.as_ref().unwrap().outcome, "running");
        assert!(begin_route_run(&paths, "players", false).is_err());
        run.finish("completed", None).unwrap();

        let completed = load_route_runtime_state(&paths, "players")
            .unwrap()
            .unwrap();
        assert_eq!(completed.last_run.as_ref().unwrap().outcome, "completed");
        assert!(completed.last_successful_unix_seconds.is_some());
        assert_eq!(completed.history.len(), 1);
    }
}
