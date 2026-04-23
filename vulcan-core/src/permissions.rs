use crate::config::{
    load_permission_profiles, ConfigPermissionMode, JsRuntimeSandbox, PathPermissionConfig,
    PathPermissionKeyword, PathPermissionRules, PermissionLimit, PermissionLimitKeyword,
    PermissionMode, PermissionProfile,
};
use crate::dataview_js::{evaluate_dataview_js_with_options, DataviewJsEvalOptions};
use crate::paths::VaultPaths;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceSpecifier {
    Folder(String),
    Tag(String),
    Note(String),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PathPermission {
    #[serde(default)]
    pub allow: Vec<ResourceSpecifier>,
    #[serde(default)]
    pub deny: Vec<ResourceSpecifier>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ResourceLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit_ms: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit_mb: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_limit_kb: Option<usize>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionGrant {
    pub read: PathPermission,
    pub write: PathPermission,
    pub refactor: PathPermission,
    pub git: bool,
    pub network: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_domains: Vec<String>,
    pub index: bool,
    pub config_read: bool,
    pub config_write: bool,
    pub execute: bool,
    pub shell: bool,
    pub limits: ResourceLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPermissionProfile {
    pub name: String,
    pub profile: PermissionProfile,
    pub grant: PermissionGrant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionError {
    UnknownProfile {
        requested: String,
        available: Vec<String>,
        diagnostics: Vec<String>,
    },
    PathDenied {
        profile: String,
        action: &'static str,
        path: String,
    },
    CapabilityDenied {
        profile: String,
        capability: &'static str,
    },
    NetworkDenied {
        profile: String,
        target: String,
        domains: Vec<String>,
    },
    PolicyHookDenied {
        profile: String,
        action: &'static str,
        resource: Option<String>,
        reason: String,
    },
}

impl Display for PermissionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProfile {
                requested,
                available,
                diagnostics,
            } => {
                write!(
                    formatter,
                    "unknown permission profile `{requested}`; available profiles: {}",
                    available.join(", ")
                )?;
                if !diagnostics.is_empty() {
                    write!(
                        formatter,
                        "; config diagnostics: {}",
                        diagnostics.join("; ")
                    )?;
                }
                Ok(())
            }
            Self::PathDenied {
                profile,
                action,
                path,
            } => write!(
                formatter,
                "permission denied: profile `{profile}` does not allow {action} `{path}`"
            ),
            Self::CapabilityDenied {
                profile,
                capability,
            } => write!(
                formatter,
                "permission denied: profile `{profile}` does not allow {capability}"
            ),
            Self::NetworkDenied {
                profile,
                target,
                domains,
            } => {
                if domains.is_empty() {
                    write!(
                        formatter,
                        "permission denied: profile `{profile}` does not allow network access to `{target}`"
                    )
                } else {
                    write!(
                        formatter,
                        "permission denied: profile `{profile}` only allows network access to {} (requested `{target}`)",
                        domains.join(", ")
                    )
                }
            }
            Self::PolicyHookDenied {
                profile,
                action,
                resource,
                reason,
            } => {
                if let Some(resource) = resource {
                    write!(
                        formatter,
                        "permission denied: profile `{profile}` policy hook rejected {action} `{resource}`: {reason}"
                    )
                } else {
                    write!(
                        formatter,
                        "permission denied: profile `{profile}` policy hook rejected {action}: {reason}"
                    )
                }
            }
        }
    }
}

impl std::error::Error for PermissionError {}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PermissionSql {
    pub cte: String,
    pub clause: String,
    pub params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PermissionFilter {
    path_permission: PathPermission,
}

pub trait PermissionGuard {
    fn profile_name(&self) -> &str;
    fn grant(&self) -> &PermissionGrant;

    fn has_policy_hook(&self) -> bool {
        false
    }

    fn check_policy_decision(
        &self,
        _action: &'static str,
        _resource: Option<&str>,
    ) -> Result<(), PermissionError> {
        Ok(())
    }

    fn check_read_path(&self, path: &str) -> Result<(), PermissionError> {
        if self.read_filter().is_allowed(path) {
            self.check_policy_decision("read", Some(path))
        } else {
            Err(PermissionError::PathDenied {
                profile: self.profile_name().to_string(),
                action: "read",
                path: normalize_permission_path(path),
            })
        }
    }

    fn check_write_path(&self, path: &str) -> Result<(), PermissionError> {
        if self.write_filter().is_allowed(path) {
            self.check_policy_decision("write", Some(path))
        } else {
            Err(PermissionError::PathDenied {
                profile: self.profile_name().to_string(),
                action: "write",
                path: normalize_permission_path(path),
            })
        }
    }

    fn check_refactor_path(&self, path: &str) -> Result<(), PermissionError> {
        if self.refactor_filter().is_allowed(path) {
            self.check_policy_decision("refactor", Some(path))
        } else {
            Err(PermissionError::PathDenied {
                profile: self.profile_name().to_string(),
                action: "refactor",
                path: normalize_permission_path(path),
            })
        }
    }

    fn check_network(&self, target: &str) -> Result<(), PermissionError> {
        if !self.grant().network {
            return Err(PermissionError::NetworkDenied {
                profile: self.profile_name().to_string(),
                target: target.to_string(),
                domains: self.grant().network_domains.clone(),
            });
        }
        if self.grant().network_domains.is_empty()
            || self
                .grant()
                .network_domains
                .iter()
                .any(|domain| network_target_matches(domain, target))
        {
            self.check_policy_decision("network", Some(target))
        } else {
            Err(PermissionError::NetworkDenied {
                profile: self.profile_name().to_string(),
                target: target.to_string(),
                domains: self.grant().network_domains.clone(),
            })
        }
    }

    fn check_git(&self) -> Result<(), PermissionError> {
        if self.grant().git {
            self.check_policy_decision("git", None)
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "git access",
            })
        }
    }

    fn check_shell(&self) -> Result<(), PermissionError> {
        if self.grant().shell {
            self.check_policy_decision("shell", None)
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "shell access",
            })
        }
    }

    fn check_index(&self) -> Result<(), PermissionError> {
        if self.grant().index {
            self.check_policy_decision("index", None)
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "index access",
            })
        }
    }

    fn check_execute(&self) -> Result<(), PermissionError> {
        if self.grant().execute {
            self.check_policy_decision("execute", None)
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "execute access",
            })
        }
    }

    fn check_config_read(&self) -> Result<(), PermissionError> {
        if self.grant().config_read {
            self.check_policy_decision("config_read", None)
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "config read access",
            })
        }
    }

    fn check_config_write(&self) -> Result<(), PermissionError> {
        if self.grant().config_write {
            self.check_policy_decision("config_write", None)
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "config write access",
            })
        }
    }

    fn resource_limits(&self) -> ResourceLimits {
        self.grant().limits.clone()
    }

    fn read_filter(&self) -> PermissionFilter {
        PermissionFilter::new(self.grant().read.clone())
    }

    fn write_filter(&self) -> PermissionFilter {
        PermissionFilter::new(self.grant().write.clone())
    }

    fn refactor_filter(&self) -> PermissionFilter {
        PermissionFilter::new(self.grant().refactor.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePermissionGuard {
    paths: VaultPaths,
    selection: ResolvedPermissionProfile,
    enable_policy_hooks: bool,
}

impl ProfilePermissionGuard {
    #[must_use]
    pub fn new(paths: &VaultPaths, selection: ResolvedPermissionProfile) -> Self {
        Self {
            paths: paths.clone(),
            selection,
            enable_policy_hooks: true,
        }
    }

    #[must_use]
    pub fn without_policy_hooks(paths: &VaultPaths, selection: ResolvedPermissionProfile) -> Self {
        Self {
            paths: paths.clone(),
            selection,
            enable_policy_hooks: false,
        }
    }

    #[must_use]
    pub fn selection(&self) -> &ResolvedPermissionProfile {
        &self.selection
    }

    fn apply_policy_hook(
        &self,
        action: &'static str,
        resource: Option<&str>,
    ) -> Result<(), PermissionError> {
        if !self.enable_policy_hooks {
            return Ok(());
        }

        let Some(policy_hook) = self.selection.profile.policy_hook.as_ref() else {
            return Ok(());
        };
        if !is_trusted_vault(self.paths.vault_root()) {
            return Err(PermissionError::PolicyHookDenied {
                profile: self.profile_name().to_string(),
                action,
                resource: resource.map(normalize_permission_path),
                reason: "policy hooks require a trusted vault".to_string(),
            });
        }

        let hook_path = resolve_policy_hook_path(self.paths.vault_root(), policy_hook);
        let hook_source =
            fs::read_to_string(&hook_path).map_err(|error| PermissionError::PolicyHookDenied {
                profile: self.profile_name().to_string(),
                action,
                resource: resource.map(normalize_permission_path),
                reason: format!(
                    "failed to read policy hook {}: {error}",
                    hook_path.display()
                ),
            })?;

        let input = serde_json::json!({
            "principal": null,
            "action": action,
            "resource": resource.map(normalize_permission_path),
            "profile_decision": "allow",
            "profile": self.profile_name(),
        });
        let source = format!(
            "const __vulcanPolicyInput = {};\n\
{}\n\
const __vulcanPolicyHandler = globalThis.policy_hook ?? globalThis.main;\n\
if (typeof __vulcanPolicyHandler !== 'function') {{\n\
  throw new Error('policy hook must export `policy_hook(input)` or `main(input)`');\n\
}}\n\
__vulcanPolicyHandler(__vulcanPolicyInput);\n",
            input,
            strip_shebang_line(&hook_source)
        );

        let profile = policy_hook_profile();
        let result = evaluate_dataview_js_with_options(
            &self.paths,
            &source,
            None,
            DataviewJsEvalOptions {
                timeout: Some(Duration::from_millis(100)),
                sandbox: Some(JsRuntimeSandbox::Strict),
                permission_profile: None,
                resolved_permissions: Some(ResolvedPermissionProfile {
                    name: format!("{}:policy_hook", self.profile_name()),
                    grant: PermissionGrant::from_profile(&profile),
                    profile,
                }),
                disable_policy_hooks: true,
                tool_registry: None,
            },
        )
        .map_err(|error| PermissionError::PolicyHookDenied {
            profile: self.profile_name().to_string(),
            action,
            resource: resource.map(normalize_permission_path),
            reason: error.to_string(),
        })?;

        interpret_policy_hook_result(self.profile_name(), action, resource, result.value)
    }
}

impl PermissionGuard for ProfilePermissionGuard {
    fn profile_name(&self) -> &str {
        &self.selection.name
    }

    fn grant(&self) -> &PermissionGrant {
        &self.selection.grant
    }

    fn check_policy_decision(
        &self,
        action: &'static str,
        resource: Option<&str>,
    ) -> Result<(), PermissionError> {
        self.apply_policy_hook(action, resource)
    }

    fn has_policy_hook(&self) -> bool {
        self.enable_policy_hooks && self.selection.profile.policy_hook.is_some()
    }
}

impl PermissionGrant {
    #[must_use]
    pub fn from_profile(profile: &PermissionProfile) -> Self {
        Self {
            read: PathPermission::from_config(&profile.read),
            write: PathPermission::from_config(&profile.write),
            refactor: PathPermission::from_config(&profile.refactor),
            git: matches!(profile.git, PermissionMode::Allow),
            network: profile.network.is_allowed(),
            network_domains: profile.network.domain_allowlist().to_vec(),
            index: matches!(profile.index, PermissionMode::Allow),
            config_read: !matches!(profile.config, ConfigPermissionMode::None),
            config_write: matches!(profile.config, ConfigPermissionMode::Write),
            execute: matches!(profile.execute, PermissionMode::Allow),
            shell: matches!(profile.shell, PermissionMode::Allow),
            limits: ResourceLimits {
                cpu_limit_ms: permission_limit_value(&profile.cpu_limit_ms),
                memory_limit_mb: permission_limit_value(&profile.memory_limit_mb),
                stack_limit_kb: permission_limit_value(&profile.stack_limit_kb),
            },
        }
    }

    #[must_use]
    pub fn is_subset_of(&self, active: &Self) -> bool {
        self.read.is_subset_of(&active.read)
            && self.write.is_subset_of(&active.write)
            && self.refactor.is_subset_of(&active.refactor)
            && capability_is_subset(self.git, active.git)
            && network_is_subset(
                self.network,
                &self.network_domains,
                active.network,
                &active.network_domains,
            )
            && capability_is_subset(self.index, active.index)
            && capability_is_subset(self.config_read, active.config_read)
            && capability_is_subset(self.config_write, active.config_write)
            && capability_is_subset(self.execute, active.execute)
            && capability_is_subset(self.shell, active.shell)
            && limit_is_subset(self.limits.cpu_limit_ms, active.limits.cpu_limit_ms)
            && limit_is_subset(self.limits.memory_limit_mb, active.limits.memory_limit_mb)
            && limit_is_subset(self.limits.stack_limit_kb, active.limits.stack_limit_kb)
    }
}

impl PathPermission {
    #[must_use]
    pub fn from_config(config: &PathPermissionConfig) -> Self {
        match config {
            PathPermissionConfig::Keyword(PathPermissionKeyword::All) => Self {
                allow: vec![ResourceSpecifier::All],
                deny: Vec::new(),
            },
            PathPermissionConfig::Keyword(PathPermissionKeyword::None) => Self::default(),
            PathPermissionConfig::Rules(PathPermissionRules { allow, deny }) => Self {
                allow: allow
                    .iter()
                    .map(|entry| parse_resource_specifier(entry))
                    .collect(),
                deny: deny
                    .iter()
                    .map(|entry| parse_resource_specifier(entry))
                    .collect(),
            },
        }
    }

    #[must_use]
    pub fn is_unrestricted(&self) -> bool {
        self.allow == [ResourceSpecifier::All] && self.deny.is_empty()
    }

    #[must_use]
    pub fn is_allowed(&self, path: &str) -> bool {
        self.is_allowed_with_tags(path, &[])
    }

    #[must_use]
    pub fn is_allowed_with_tags(&self, path: &str, tags: &[String]) -> bool {
        if self.is_unrestricted() {
            return true;
        }
        let normalized = normalize_permission_path(path);
        let allowed = self
            .allow
            .iter()
            .any(|specifier| specifier_matches_path(specifier, &normalized, tags));
        allowed
            && !self
                .deny
                .iter()
                .any(|specifier| specifier_matches_path(specifier, &normalized, tags))
    }

    #[must_use]
    pub fn is_subset_of(&self, active: &Self) -> bool {
        if self.allow.is_empty() {
            return true;
        }
        if active.is_unrestricted() {
            return true;
        }
        if self.is_unrestricted() {
            return false;
        }

        self.allow.iter().all(|requested| {
            let covered = active
                .allow
                .iter()
                .any(|allowed| resource_specifier_covers(allowed, requested));
            if !covered {
                return false;
            }

            let active_overlap = active
                .deny
                .iter()
                .filter(|deny| resource_specifiers_overlap(requested, deny))
                .collect::<Vec<_>>();
            active_overlap.into_iter().all(|active_deny| {
                self.deny
                    .iter()
                    .any(|requested_deny| resource_specifier_covers(requested_deny, active_deny))
            })
        })
    }
}

impl PermissionFilter {
    #[must_use]
    pub fn new(path_permission: PathPermission) -> Self {
        Self { path_permission }
    }

    #[must_use]
    pub fn is_allowed(&self, path: &str) -> bool {
        self.path_permission.is_allowed(path)
    }

    #[must_use]
    pub fn path_permission(&self) -> &PathPermission {
        &self.path_permission
    }

    #[must_use]
    pub fn document_scope_sql(&self, cte_name: &str) -> PermissionSql {
        if self.path_permission.is_unrestricted() {
            return PermissionSql::default();
        }
        if self.path_permission.allow.is_empty() {
            return PermissionSql {
                cte: String::new(),
                clause: " AND 1 = 0".to_string(),
                params: Vec::new(),
            };
        }

        let mut params = Vec::new();
        let allow_sql = specifier_group_sql(&self.path_permission.allow, "documents", &mut params);
        let deny_sql = specifier_group_sql(&self.path_permission.deny, "documents", &mut params);

        let mut cte = format!(
            "WITH {cte_name} AS (SELECT documents.id FROM documents WHERE documents.extension = 'md' AND ({allow_sql})"
        );
        if !deny_sql.is_empty() {
            cte.push_str(" AND NOT (");
            cte.push_str(&deny_sql);
            cte.push(')');
        }
        cte.push_str(") ");

        PermissionSql {
            cte,
            clause: format!(" AND documents.id IN (SELECT id FROM {cte_name})"),
            params,
        }
    }
}

#[must_use]
pub fn combine_cte_fragments<I>(fragments: I) -> String
where
    I: IntoIterator<Item = String>,
{
    let parts = fragments
        .into_iter()
        .filter_map(|fragment| {
            let trimmed = fragment.trim();
            (!trimmed.is_empty())
                .then(|| trimmed.trim_start_matches("WITH ").trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        String::new()
    } else {
        format!("WITH {} ", parts.join(", "))
    }
}

pub fn resolve_permission_profile(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
) -> Result<ResolvedPermissionProfile, PermissionError> {
    let requested_name = requested_profile.unwrap_or("unrestricted");
    let loaded = load_permission_profiles(paths);
    let Some(profile) = loaded.profiles.get(requested_name) else {
        return Err(PermissionError::UnknownProfile {
            requested: requested_name.to_string(),
            available: loaded.profiles.keys().cloned().collect(),
            diagnostics: loaded
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.path.display(), diagnostic.message))
                .collect(),
        });
    };

    Ok(ResolvedPermissionProfile {
        name: requested_name.to_string(),
        profile: profile.clone(),
        grant: PermissionGrant::from_profile(profile),
    })
}

fn parse_resource_specifier(value: &str) -> ResourceSpecifier {
    let normalized = normalize_permission_path(value);
    if normalized == "*" || normalized == "**" {
        return ResourceSpecifier::All;
    }
    if let Some(folder) = normalized.strip_prefix("folder:") {
        let folder = normalize_permission_path(folder);
        if folder == "*" || folder == "**" {
            return ResourceSpecifier::All;
        }
        return ResourceSpecifier::Folder(folder);
    }
    if let Some(tag) = normalized.strip_prefix("tag:") {
        return ResourceSpecifier::Tag(tag.trim_start_matches('#').to_string());
    }
    if let Some(tag) = normalized.strip_prefix('#') {
        return ResourceSpecifier::Tag(tag.to_string());
    }
    if let Some(note) = normalized.strip_prefix("note:") {
        return ResourceSpecifier::Note(normalize_permission_path(note));
    }
    if normalized.contains('*') || normalized.contains('?') {
        return ResourceSpecifier::Folder(normalized);
    }
    ResourceSpecifier::Note(normalized)
}

fn permission_limit_value(limit: &PermissionLimit) -> Option<usize> {
    match limit {
        PermissionLimit::Value(value) => Some(*value),
        PermissionLimit::Keyword(PermissionLimitKeyword::Unlimited) => None,
    }
}

fn capability_is_subset(requested: bool, active: bool) -> bool {
    !requested || active
}

fn limit_is_subset(requested: Option<usize>, active: Option<usize>) -> bool {
    match (requested, active) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(requested), Some(active)) => requested <= active,
    }
}

fn network_is_subset(
    requested_allow: bool,
    requested_domains: &[String],
    active_allow: bool,
    active_domains: &[String],
) -> bool {
    if !requested_allow {
        return true;
    }
    if !active_allow {
        return false;
    }
    if active_domains.is_empty() {
        return true;
    }
    if requested_domains.is_empty() {
        return false;
    }
    requested_domains.iter().all(|requested| {
        active_domains
            .iter()
            .any(|active| network_target_matches(active, requested))
    })
}

fn specifier_matches_path(specifier: &ResourceSpecifier, path: &str, tags: &[String]) -> bool {
    match specifier {
        ResourceSpecifier::All => true,
        ResourceSpecifier::Folder(pattern) => glob_matches(pattern, path),
        ResourceSpecifier::Tag(tag) => tags.iter().any(|candidate| tag_matches(tag, candidate)),
        ResourceSpecifier::Note(note) => normalize_permission_path(note) == path,
    }
}

fn resource_specifier_covers(active: &ResourceSpecifier, requested: &ResourceSpecifier) -> bool {
    match (active, requested) {
        (ResourceSpecifier::All, _) => true,
        (ResourceSpecifier::Tag(active), ResourceSpecifier::Tag(requested)) => {
            tag_matches(active, requested)
        }
        (ResourceSpecifier::Note(active), ResourceSpecifier::Note(requested)) => {
            normalize_permission_path(active) == normalize_permission_path(requested)
        }
        (ResourceSpecifier::Folder(active), ResourceSpecifier::Note(requested)) => {
            glob_matches(active, requested)
        }
        (ResourceSpecifier::Folder(active), ResourceSpecifier::Folder(requested)) => {
            active == requested || glob_pattern_covers_pattern(active, requested)
        }
        _ => false,
    }
}

fn resource_specifiers_overlap(left: &ResourceSpecifier, right: &ResourceSpecifier) -> bool {
    resource_specifier_covers(left, right)
        || resource_specifier_covers(right, left)
        || match (left, right) {
            (ResourceSpecifier::Folder(left), ResourceSpecifier::Note(right))
            | (ResourceSpecifier::Note(right), ResourceSpecifier::Folder(left)) => {
                glob_matches(left, right)
            }
            (ResourceSpecifier::Tag(left), ResourceSpecifier::Tag(right)) => {
                tag_matches(left, right) || tag_matches(right, left)
            }
            _ => false,
        }
}

fn specifier_group_sql(
    specifiers: &[ResourceSpecifier],
    document_alias: &str,
    params: &mut Vec<String>,
) -> String {
    specifiers
        .iter()
        .map(|specifier| specifier_sql(specifier, document_alias, params))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn specifier_sql(
    specifier: &ResourceSpecifier,
    document_alias: &str,
    params: &mut Vec<String>,
) -> String {
    match specifier {
        ResourceSpecifier::All => "1 = 1".to_string(),
        ResourceSpecifier::Folder(pattern) => {
            params.push(sqlite_glob_pattern(pattern));
            format!("{document_alias}.path GLOB ?")
        }
        ResourceSpecifier::Note(note) => {
            params.push(normalize_permission_path(note));
            format!("{document_alias}.path = ?")
        }
        ResourceSpecifier::Tag(tag) => {
            params.push(tag.trim_start_matches('#').to_string());
            params.push(format!("{}/{}", tag.trim_start_matches('#'), "*"));
            format!(
                "EXISTS (SELECT 1 FROM tags WHERE tags.document_id = {document_alias}.id AND (tags.tag_text = ? OR tags.tag_text GLOB ?))"
            )
        }
    }
}

fn sqlite_glob_pattern(pattern: &str) -> String {
    normalize_permission_path(pattern).replace("**", "*")
}

fn normalize_permission_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern = sqlite_glob_pattern(pattern);
    let path = normalize_permission_path(path);
    glob_matches_bytes(pattern.as_bytes(), path.as_bytes())
}

fn glob_pattern_covers_pattern(active: &str, requested: &str) -> bool {
    if active == requested {
        return true;
    }
    if matches!(active, "*" | "**") {
        return true;
    }

    let active_prefix = glob_static_prefix(active);
    let requested_prefix = glob_static_prefix(requested);
    !active_prefix.is_empty()
        && requested_prefix.starts_with(&active_prefix)
        && active.ends_with('*')
}

fn glob_matches_bytes(pattern: &[u8], path: &[u8]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }

    match pattern[0] {
        b'*' => {
            for index in 0..=path.len() {
                if glob_matches_bytes(&pattern[1..], &path[index..]) {
                    return true;
                }
            }
            false
        }
        b'?' => !path.is_empty() && glob_matches_bytes(&pattern[1..], &path[1..]),
        byte => {
            path.first().is_some_and(|candidate| *candidate == byte)
                && glob_matches_bytes(&pattern[1..], &path[1..])
        }
    }
}

fn glob_static_prefix(pattern: &str) -> String {
    pattern
        .split(['*', '?'])
        .next()
        .unwrap_or_default()
        .to_string()
}

fn tag_matches(expected: &str, candidate: &str) -> bool {
    let expected = expected.trim_start_matches('#');
    let candidate = candidate.trim_start_matches('#');
    candidate == expected || candidate.starts_with(&format!("{expected}/"))
}

fn resolve_policy_hook_path(vault_root: &Path, hook_path: &Path) -> PathBuf {
    if hook_path.is_absolute() {
        hook_path.to_path_buf()
    } else {
        vault_root.join(hook_path)
    }
}

fn strip_shebang_line(source: &str) -> &str {
    if let Some(stripped) = source.strip_prefix("#!") {
        stripped
            .split_once('\n')
            .map_or("", |(_, remainder)| remainder)
    } else {
        source
    }
}

fn policy_hook_profile() -> PermissionProfile {
    PermissionProfile {
        read: PathPermissionConfig::Keyword(PathPermissionKeyword::All),
        write: PathPermissionConfig::Keyword(PathPermissionKeyword::None),
        refactor: PathPermissionConfig::Keyword(PathPermissionKeyword::None),
        git: PermissionMode::Deny,
        network: crate::config::NetworkPermissionConfig::Mode(PermissionMode::Deny),
        index: PermissionMode::Deny,
        config: ConfigPermissionMode::None,
        execute: PermissionMode::Allow,
        shell: PermissionMode::Deny,
        cpu_limit_ms: PermissionLimit::Value(100),
        memory_limit_mb: PermissionLimit::Value(32),
        stack_limit_kb: PermissionLimit::Value(128),
        policy_hook: None,
    }
}

fn interpret_policy_hook_result(
    profile: &str,
    action: &'static str,
    resource: Option<&str>,
    value: Option<serde_json::Value>,
) -> Result<(), PermissionError> {
    let resource = resource.map(normalize_permission_path);
    match value {
        Some(serde_json::Value::String(decision)) if decision == "pass" => Ok(()),
        Some(serde_json::Value::String(decision)) if decision == "deny" => {
            Err(PermissionError::PolicyHookDenied {
                profile: profile.to_string(),
                action,
                resource,
                reason: "denied by policy hook".to_string(),
            })
        }
        Some(serde_json::Value::Object(object)) => {
            let decision = object
                .get("decision")
                .or_else(|| object.get("status"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let reason = object
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("denied by policy hook")
                .to_string();
            match decision {
                "pass" => Ok(()),
                "deny" => Err(PermissionError::PolicyHookDenied {
                    profile: profile.to_string(),
                    action,
                    resource,
                    reason,
                }),
                _ => Err(PermissionError::PolicyHookDenied {
                    profile: profile.to_string(),
                    action,
                    resource,
                    reason: "policy hook must return `pass` or `deny`".to_string(),
                }),
            }
        }
        _ => Err(PermissionError::PolicyHookDenied {
            profile: profile.to_string(),
            action,
            resource,
            reason: "policy hook must return `pass` or `deny`".to_string(),
        }),
    }
}

fn trusted_vaults_file() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|path| path.join("vulcan").join("trusted_vaults.json"))
}

fn is_trusted_vault(vault_root: &Path) -> bool {
    let Some(path) = trusted_vaults_file() else {
        return false;
    };
    let Ok(canonical_root) = vault_root.canonicalize() else {
        return false;
    };
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    value
        .get("vaults")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|vaults| {
            vaults
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|entry| {
                    PathBuf::from(entry)
                        .canonicalize()
                        .is_ok_and(|candidate| candidate == canonical_root)
                })
        })
}

fn network_target_matches(domain: &str, target: &str) -> bool {
    let normalized_domain = domain.trim().trim_start_matches('.');
    if normalized_domain.is_empty() {
        return false;
    }
    let host = extract_network_host(target)
        .unwrap_or(target)
        .trim()
        .trim_matches('/');
    host == normalized_domain || host.ends_with(&format!(".{normalized_domain}"))
}

fn extract_network_host(target: &str) -> Option<&str> {
    let without_scheme = target.split_once("://").map_or(target, |(_, rest)| rest);
    let host_port = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let host = host_port
        .split_once('@')
        .map_or(host_port, |(_, rest)| rest)
        .split(':')
        .next()
        .unwrap_or(host_port)
        .trim();
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::{
        combine_cte_fragments, glob_matches, network_target_matches, parse_resource_specifier,
        PathPermission, PermissionFilter, PermissionGrant, ResourceLimits, ResourceSpecifier,
    };
    use crate::config::{
        ConfigPermissionMode, NetworkPermissionConfig, NetworkPermissionDetails,
        PathPermissionConfig, PathPermissionRules, PermissionLimit, PermissionMode,
        PermissionProfile,
    };
    use proptest::prelude::*;

    fn path_segment_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[A-Za-z0-9_-]{1,8}")
            .expect("path segment regex should be valid")
    }

    #[test]
    fn path_permission_deny_rules_override_allow_rules() {
        let permission =
            PathPermission::from_config(&PathPermissionConfig::Rules(PathPermissionRules {
                allow: vec!["Projects/**".to_string()],
                deny: vec!["Projects/Secret.md".to_string()],
            }));

        assert!(permission.is_allowed("Projects/Alpha.md"));
        assert!(permission.is_allowed("Projects/Nested/Beta.md"));
        assert!(!permission.is_allowed("Projects/Secret.md"));
        assert!(!permission.is_allowed("Archive/Alpha.md"));
    }

    #[test]
    fn permission_filter_generates_scoped_document_sql() {
        let filter = PermissionFilter::new(PathPermission::from_config(
            &PathPermissionConfig::Rules(PathPermissionRules {
                allow: vec!["Projects/**".to_string()],
                deny: vec!["Projects/Secret.md".to_string()],
            }),
        ));

        let sql = filter.document_scope_sql("_allowed_documents");
        assert!(sql.cte.starts_with("WITH _allowed_documents AS"));
        assert!(sql.clause.contains("_allowed_documents"));
        assert_eq!(
            sql.params,
            vec!["Projects/*".to_string(), "Projects/Secret.md".to_string()]
        );
    }

    #[test]
    fn combine_cte_fragments_merges_multiple_with_clauses() {
        let combined = combine_cte_fragments([
            "WITH a AS (SELECT 1) ".to_string(),
            String::new(),
            "WITH b AS (SELECT 2) ".to_string(),
        ]);
        assert_eq!(combined, "WITH a AS (SELECT 1), b AS (SELECT 2) ");
    }

    #[test]
    fn permission_grant_maps_profile_capabilities_and_limits() {
        let profile = PermissionProfile {
            read: PathPermissionConfig::Rules(PathPermissionRules {
                allow: vec!["Projects/**".to_string()],
                deny: vec![],
            }),
            write: PathPermissionConfig::Keyword(crate::PathPermissionKeyword::All),
            refactor: PathPermissionConfig::default(),
            git: PermissionMode::Allow,
            network: NetworkPermissionConfig::Details(NetworkPermissionDetails {
                allow: true,
                domains: vec!["example.com".to_string()],
            }),
            index: PermissionMode::Deny,
            config: ConfigPermissionMode::Read,
            execute: PermissionMode::Allow,
            shell: PermissionMode::Deny,
            cpu_limit_ms: PermissionLimit::Value(2500),
            memory_limit_mb: PermissionLimit::Value(64),
            stack_limit_kb: PermissionLimit::Value(512),
            policy_hook: None,
        };

        let grant = PermissionGrant::from_profile(&profile);
        assert!(!grant.read.is_unrestricted());
        assert!(grant.write.is_unrestricted());
        assert!(grant.git);
        assert!(grant.network);
        assert_eq!(grant.network_domains, vec!["example.com".to_string()]);
        assert!(!grant.index);
        assert!(grant.config_read);
        assert!(!grant.config_write);
        assert!(grant.execute);
        assert!(!grant.shell);
        assert_eq!(
            grant.limits,
            ResourceLimits {
                cpu_limit_ms: Some(2500),
                memory_limit_mb: Some(64),
                stack_limit_kb: Some(512),
            }
        );
    }

    #[test]
    fn resource_specifier_parser_supports_tags_and_notes() {
        assert_eq!(
            parse_resource_specifier("Projects/**"),
            ResourceSpecifier::Folder("Projects/**".to_string())
        );
        assert_eq!(
            parse_resource_specifier("folder:Projects/**"),
            ResourceSpecifier::Folder("Projects/**".to_string())
        );
        assert_eq!(
            parse_resource_specifier("tag:project"),
            ResourceSpecifier::Tag("project".to_string())
        );
        assert_eq!(
            parse_resource_specifier("#project"),
            ResourceSpecifier::Tag("project".to_string())
        );
        assert_eq!(
            parse_resource_specifier("Projects/Alpha.md"),
            ResourceSpecifier::Note("Projects/Alpha.md".to_string())
        );
        assert_eq!(
            parse_resource_specifier("note:Projects/Alpha.md"),
            ResourceSpecifier::Note("Projects/Alpha.md".to_string())
        );
    }

    #[test]
    fn glob_matching_uses_sqlite_style_wildcards() {
        assert!(glob_matches("Projects/*", "Projects/Nested/Alpha.md"));
        assert!(glob_matches("Projects/???.md", "Projects/abc.md"));
        assert!(!glob_matches("Projects/???.md", "Projects/abcd.md"));
    }

    #[test]
    fn network_domain_matching_accepts_hosts_and_urls() {
        assert!(network_target_matches(
            "example.com",
            "https://api.example.com/search"
        ));
        assert!(network_target_matches("example.com", "api.example.com"));
        assert!(!network_target_matches("example.com", "example.org"));
    }

    #[test]
    fn path_permissions_accept_narrower_note_scopes() {
        let active =
            PathPermission::from_config(&PathPermissionConfig::Rules(PathPermissionRules {
                allow: vec!["Projects/**".to_string()],
                deny: vec!["Projects/Secret.md".to_string()],
            }));
        let requested =
            PathPermission::from_config(&PathPermissionConfig::Rules(PathPermissionRules {
                allow: vec!["note:Projects/Alpha.md".to_string()],
                deny: vec![],
            }));

        assert!(requested.is_subset_of(&active));
    }

    #[test]
    fn path_permissions_reject_requested_scopes_that_reenable_denied_paths() {
        let active =
            PathPermission::from_config(&PathPermissionConfig::Rules(PathPermissionRules {
                allow: vec!["Projects/**".to_string()],
                deny: vec!["Projects/Secret.md".to_string()],
            }));
        let requested =
            PathPermission::from_config(&PathPermissionConfig::Rules(PathPermissionRules {
                allow: vec!["Projects/**".to_string()],
                deny: vec![],
            }));

        assert!(!requested.is_subset_of(&active));
    }

    #[test]
    fn permission_grants_compare_capabilities_domains_and_limits() {
        let active = PermissionGrant {
            read: PathPermission::from_config(&PathPermissionConfig::Keyword(
                crate::PathPermissionKeyword::All,
            )),
            write: PathPermission::from_config(&PathPermissionConfig::Rules(PathPermissionRules {
                allow: vec!["Projects/**".to_string()],
                deny: vec![],
            })),
            refactor: PathPermission::default(),
            git: true,
            network: true,
            network_domains: vec!["example.com".to_string()],
            index: false,
            config_read: true,
            config_write: false,
            execute: true,
            shell: false,
            limits: ResourceLimits {
                cpu_limit_ms: Some(5_000),
                memory_limit_mb: Some(64),
                stack_limit_kb: Some(256),
            },
        };
        let requested = PermissionGrant {
            read: PathPermission::from_config(&PathPermissionConfig::Rules(PathPermissionRules {
                allow: vec!["note:Projects/Alpha.md".to_string()],
                deny: vec![],
            })),
            write: PathPermission::default(),
            refactor: PathPermission::default(),
            git: false,
            network: false,
            network_domains: vec![],
            index: false,
            config_read: false,
            config_write: false,
            execute: true,
            shell: false,
            limits: ResourceLimits {
                cpu_limit_ms: Some(100),
                memory_limit_mb: Some(32),
                stack_limit_kb: Some(128),
            },
        };

        assert!(requested.is_subset_of(&active));

        let broader_network = PermissionGrant {
            network: true,
            network_domains: vec![],
            ..requested.clone()
        };
        assert!(!broader_network.is_subset_of(&active));
    }

    proptest! {
        #[test]
        fn generated_allow_rules_still_respect_explicit_denies(
            folder in path_segment_strategy(),
            allowed_name in path_segment_strategy(),
            denied_name in path_segment_strategy(),
        ) {
            prop_assume!(allowed_name != denied_name);

            let allowed_path = format!("{folder}/{allowed_name}.md");
            let denied_path = format!("{folder}/{denied_name}.md");
            let outside_path = format!("Outside/{allowed_name}.md");
            let permission =
                PathPermission::from_config(&PathPermissionConfig::Rules(PathPermissionRules {
                    allow: vec![format!("folder:{folder}/**")],
                    deny: vec![format!("note:{denied_path}")],
                }));

            prop_assert!(permission.is_allowed(&allowed_path));
            prop_assert!(!permission.is_allowed(&denied_path));
            prop_assert!(!permission.is_allowed(&outside_path));

            let allowed_scope =
                PathPermission::from_config(&PathPermissionConfig::Rules(PathPermissionRules {
                    allow: vec![format!("note:{allowed_path}")],
                    deny: vec![],
                }));
            prop_assert!(allowed_scope.is_subset_of(&permission));

            let denied_scope =
                PathPermission::from_config(&PathPermissionConfig::Rules(PathPermissionRules {
                    allow: vec![format!("note:{denied_path}")],
                    deny: vec![],
                }));
            prop_assert!(!denied_scope.is_subset_of(&permission));
        }
    }
}
