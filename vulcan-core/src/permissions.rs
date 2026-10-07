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
use std::sync::Arc;
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
    /// The policy could not produce a valid decision; unlike an explicit deny,
    /// this must not be interpreted as a successfully filtered read result.
    PolicyHookFailed {
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
            }
            | Self::PolicyHookFailed {
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
struct PolicyHookSnapshot {
    path: PathBuf,
    source: String,
}

impl PolicyHookSnapshot {
    fn capture(path: PathBuf) -> Result<Self, std::io::Error> {
        let source = fs::read_to_string(&path)?;
        Ok(Self { path, source })
    }

    fn is_current(&self) -> Result<bool, std::io::Error> {
        Ok(fs::read_to_string(&self.path)? == self.source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePermissionGuard {
    paths: VaultPaths,
    selection: ResolvedPermissionProfile,
    enable_policy_hooks: bool,
    policy_snapshot: Option<Arc<PolicyHookSnapshot>>,
}

impl ProfilePermissionGuard {
    #[must_use]
    pub fn new(paths: &VaultPaths, selection: ResolvedPermissionProfile) -> Self {
        Self {
            paths: paths.clone(),
            selection,
            enable_policy_hooks: true,
            policy_snapshot: None,
        }
    }

    #[must_use]
    pub fn without_policy_hooks(paths: &VaultPaths, selection: ResolvedPermissionProfile) -> Self {
        Self {
            paths: paths.clone(),
            selection,
            enable_policy_hooks: false,
            policy_snapshot: None,
        }
    }

    #[must_use]
    pub fn selection(&self) -> &ResolvedPermissionProfile {
        &self.selection
    }

    /// Capture one hook revision for all decisions in a read operation. The
    /// caller must validate its retained grant before capturing and recheck
    /// both that grant and this snapshot before publishing the result.
    pub fn snapshot_read_policy(&self) -> Result<Self, PermissionError> {
        let mut guard = self.clone();
        if self.has_policy_hook() {
            self.check_snapshot_trust()?;
            let hook = self
                .selection
                .profile
                .policy_hook
                .as_ref()
                .expect("active hook");
            let path = resolve_policy_hook_path(self.paths.vault_root(), hook);
            let snapshot = PolicyHookSnapshot::capture(path)
                .map_err(|_| self.snapshot_failure("failed to read policy hook snapshot"))?;
            guard.policy_snapshot = Some(Arc::new(snapshot));
        }
        Ok(guard)
    }

    pub fn recheck_read_policy_snapshot(&self) -> Result<(), PermissionError> {
        if let Some(snapshot) = &self.policy_snapshot {
            self.check_snapshot_trust()?;
            if !snapshot.is_current().unwrap_or(false) {
                return Err(self.snapshot_failure(
                    "policy hook changed during task read; retry with current authority",
                ));
            }
        }
        Ok(())
    }

    fn check_snapshot_trust(&self) -> Result<(), PermissionError> {
        if !crate::paths::is_trusted_vault(self.paths.vault_root()) {
            return Err(self.snapshot_failure("policy hooks require a trusted vault"));
        }
        Ok(())
    }

    fn snapshot_failure(&self, reason: &str) -> PermissionError {
        PermissionError::PolicyHookFailed {
            profile: self.profile_name().to_string(),
            action: "read",
            resource: None,
            reason: reason.to_string(),
        }
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
        if !crate::paths::is_trusted_vault(self.paths.vault_root()) {
            return Err(PermissionError::PolicyHookFailed {
                profile: self.profile_name().to_string(),
                action,
                resource: resource.map(normalize_permission_path),
                reason: "policy hooks require a trusted vault".to_string(),
            });
        }

        let hook_path = resolve_policy_hook_path(self.paths.vault_root(), policy_hook);
        let fresh_source;
        let hook_source = if let Some(snapshot) = &self.policy_snapshot {
            snapshot.source.as_str()
        } else {
            fresh_source = fs::read_to_string(&hook_path).map_err(|error| {
                PermissionError::PolicyHookFailed {
                    profile: self.profile_name().to_string(),
                    action,
                    resource: resource.map(normalize_permission_path),
                    reason: format!(
                        "failed to read policy hook {}: {error}",
                        hook_path.display()
                    ),
                }
            })?;
            fresh_source.as_str()
        };

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
            strip_shebang_line(hook_source)
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
                deterministic_static: false,
                disable_policy_hooks: true,
                tool_registry: None,
                mutation_committer: None,
            },
        )
        .map_err(|error| PermissionError::PolicyHookFailed {
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

    /// Return whether this permission proves access to every path represented
    /// by `pattern` without consulting the current filesystem.
    ///
    /// This is deliberately stricter than checking the records that happen to
    /// exist. Callers use it for constraints whose correctness depends on the
    /// absence of hidden or not-yet-created records. Tag selectors cannot prove
    /// path-namespace coverage, and any overlapping deny must make the proof
    /// fail.
    #[must_use]
    pub fn covers_path_namespace(&self, pattern: &str) -> bool {
        let requested = ResourceSpecifier::Folder(normalize_permission_path(pattern));
        self.allow
            .iter()
            .any(|allowed| resource_specifier_covers(allowed, &requested))
            && !self
                .deny
                .iter()
                .any(|denied| namespace_deny_may_overlap(denied, pattern))
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
        self.scope_sql(cte_name, "documents", "documents.id", "documents.id")
    }

    /// [`Self::document_scope_sql`] over the narrow `note_query` table, whose
    /// clause restricts `id_column` (such as `note_query.document_id`); wide
    /// `documents` rows are never read.
    #[must_use]
    pub fn note_query_scope_sql(&self, cte_name: &str, id_column: &str) -> PermissionSql {
        self.scope_sql(cte_name, "note_query", "note_query.document_id", id_column)
    }

    /// The scope as a condition on the current `note_query` row (no CTE),
    /// so a query reading `note_query` alone can use a covering index.
    #[must_use]
    pub fn note_query_scope_condition(&self) -> PermissionSql {
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
        let allow_sql = specifier_group_sql(
            &self.path_permission.allow,
            "note_query",
            "note_query.document_id",
            &mut params,
        );
        let deny_sql = specifier_group_sql(
            &self.path_permission.deny,
            "note_query",
            "note_query.document_id",
            &mut params,
        );
        let mut clause = format!(" AND note_query.extension = 'md' AND ({allow_sql})");
        if !deny_sql.is_empty() {
            clause.push_str(" AND NOT (");
            clause.push_str(&deny_sql);
            clause.push(')');
        }
        PermissionSql {
            cte: String::new(),
            clause,
            params,
        }
    }

    /// The scope as a CTE of permitted ids selected from `table` (with
    /// `path`, `extension`, and the document id `table_id`), and a clause
    /// restricting `id_column` to them.
    fn scope_sql(
        &self,
        cte_name: &str,
        table: &str,
        table_id: &str,
        id_column: &str,
    ) -> PermissionSql {
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
        let allow_sql =
            specifier_group_sql(&self.path_permission.allow, table, table_id, &mut params);
        let deny_sql =
            specifier_group_sql(&self.path_permission.deny, table, table_id, &mut params);

        let mut cte = format!(
            "WITH {cte_name} AS (SELECT {table_id} AS id FROM {table} WHERE {table}.extension = 'md' AND ({allow_sql})"
        );
        if !deny_sql.is_empty() {
            cte.push_str(" AND NOT (");
            cte.push_str(&deny_sql);
            cte.push(')');
        }
        cte.push_str(") ");

        PermissionSql {
            cte,
            clause: format!(" AND {id_column} IN (SELECT id FROM {cte_name})"),
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

fn namespace_deny_may_overlap(denied: &ResourceSpecifier, namespace: &str) -> bool {
    match denied {
        ResourceSpecifier::All | ResourceSpecifier::Tag(_) => true,
        ResourceSpecifier::Note(path) => glob_matches(namespace, path),
        ResourceSpecifier::Folder(pattern) => {
            let denied_prefix = glob_static_prefix(pattern);
            let namespace_prefix = glob_static_prefix(namespace);
            denied_prefix.is_empty()
                || namespace_prefix.is_empty()
                || denied_prefix.starts_with(&namespace_prefix)
                || namespace_prefix.starts_with(&denied_prefix)
        }
    }
}

fn specifier_group_sql(
    specifiers: &[ResourceSpecifier],
    document_alias: &str,
    document_id: &str,
    params: &mut Vec<String>,
) -> String {
    specifiers
        .iter()
        .map(|specifier| specifier_sql(specifier, document_alias, document_id, params))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn specifier_sql(
    specifier: &ResourceSpecifier,
    document_alias: &str,
    document_id: &str,
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
                "EXISTS (SELECT 1 FROM tags WHERE tags.document_id = {document_id} AND (tags.tag_text = ? OR tags.tag_text GLOB ?))"
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

/// Whether `path` matches `pattern`, where `*` (and `**`, which is the
/// same) matches any run of characters including `/` and `?` matches one.
/// Literal and `prefix*` patterns, the common grant shapes, are decided
/// without backtracking; nothing is allocated unless a backslash needs
/// normalizing.
fn glob_matches(pattern: &str, path: &str) -> bool {
    fn normalize(value: &str) -> std::borrow::Cow<'_, str> {
        if value.contains('\\') {
            std::borrow::Cow::Owned(normalize_permission_path(value))
        } else {
            std::borrow::Cow::Borrowed(value)
        }
    }
    let (pattern, path) = (normalize(pattern), normalize(path));
    let (pattern, path) = (pattern.as_bytes(), path.as_bytes());
    match pattern.iter().position(|byte| matches!(byte, b'*' | b'?')) {
        None => pattern == path,
        Some(wildcard) if pattern[wildcard..].iter().all(|byte| *byte == b'*') => {
            path.starts_with(&pattern[..wildcard])
        }
        Some(_) => glob_matches_bytes(pattern, path),
    }
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
            // A run of stars matches like one.
            let rest = pattern
                .iter()
                .position(|byte| *byte != b'*')
                .map_or(&[][..], |at| &pattern[at..]);
            if rest.is_empty() {
                return true;
            }
            for index in 0..=path.len() {
                if glob_matches_bytes(rest, &path[index..]) {
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
                _ => Err(PermissionError::PolicyHookFailed {
                    profile: profile.to_string(),
                    action,
                    resource,
                    reason: "policy hook must return `pass` or `deny`".to_string(),
                }),
            }
        }
        _ => Err(PermissionError::PolicyHookFailed {
            profile: profile.to_string(),
            action,
            resource,
            reason: "policy hook must return `pass` or `deny`".to_string(),
        }),
    }
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

    #[test]
    fn glob_fast_paths_match_the_backtracking_matcher() {
        // The previous matcher: rewrite `**` to `*`, then backtrack.
        fn backtrack(pattern: &[u8], path: &[u8]) -> bool {
            match pattern.first() {
                None => path.is_empty(),
                Some(b'*') => (0..=path.len()).any(|at| backtrack(&pattern[1..], &path[at..])),
                Some(b'?') => !path.is_empty() && backtrack(&pattern[1..], &path[1..]),
                Some(byte) => path.first() == Some(byte) && backtrack(&pattern[1..], &path[1..]),
            }
        }
        fn oracle(pattern: &str, path: &str) -> bool {
            let pattern = pattern.replace('\\', "/").replace("**", "*");
            backtrack(pattern.as_bytes(), path.replace('\\', "/").as_bytes())
        }
        let patterns = [
            "",
            "*",
            "**",
            "***",
            "public/**",
            "public/*",
            "public",
            "public/",
            "pub*",
            "*.md",
            "**/*.md",
            "a?c",
            "a*c",
            "a**c",
            "dir\\**",
            "x/*/y",
            "?",
            "??*",
            "tasks/private/**",
            "_types/**",
        ];
        let paths = [
            "",
            "public",
            "public/",
            "public/a.md",
            "publicity.md",
            "abc",
            "ac",
            "a/b/c",
            "dir/x.md",
            "dir\\x.md",
            "x/a/y",
            "x//y",
            "tasks/private/secret.md",
            "tasks/public.md",
            "_types/task.md",
            "a.md",
            "z",
        ];
        for pattern in patterns {
            for path in paths {
                assert_eq!(
                    glob_matches(pattern, path),
                    oracle(pattern, path),
                    "{pattern:?} vs {path:?}"
                );
            }
        }
    }
    use super::{
        combine_cte_fragments, glob_matches, interpret_policy_hook_result, network_target_matches,
        parse_resource_specifier, PathPermission, PermissionError, PermissionFilter,
        PermissionGrant, ResourceLimits, ResourceSpecifier,
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
    #[cfg(feature = "js_runtime")]
    fn read_policy_snapshot_keeps_one_revision_and_detects_drift() {
        use super::{PermissionGuard, ProfilePermissionGuard};
        use std::fs;
        const CHILD_ROOT: &str = "VULCAN_POLICY_SNAPSHOT_TEST_ROOT";
        let Some(root) = std::env::var_os(CHILD_ROOT) else {
            // Isolate trust configuration without changing this test process's
            // environment while other tests may be reading it.
            let temp = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "permissions::tests::read_policy_snapshot_keeps_one_revision_and_detects_drift",
                    "--nocapture",
                ])
                .env(CHILD_ROOT, temp.path())
                .env("XDG_CONFIG_HOME", temp.path().join("xdg"))
                .env("XDG_STATE_HOME", temp.path().join("state"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = std::path::PathBuf::from(root).join("vault");
        fs::create_dir_all(root.join(".vulcan")).unwrap();
        fs::write(root.join(".vulcan/config.toml"), "[permissions.profiles.guarded]\nread = \"all\"\nwrite = \"none\"\npolicy_hook = \".vulcan/guard.js\"\n").unwrap();
        fs::write(root.join("Note.md"), "Note\n").unwrap();
        let hook = root.join(".vulcan/guard.js");
        let allow = "function policy_hook(input) { return 'pass'; }";
        fs::write(&hook, allow).unwrap();
        let trust = crate::paths::trusted_vaults_file().unwrap();
        fs::create_dir_all(trust.parent().unwrap()).unwrap();
        fs::write(
            &trust,
            serde_json::json!({"vaults": [root.canonicalize().unwrap()]}).to_string(),
        )
        .unwrap();
        let paths = crate::VaultPaths::new(&root);
        crate::scan_vault(&paths, crate::ScanMode::Full).unwrap();
        let selection = super::resolve_permission_profile(&paths, Some("guarded")).unwrap();
        let guard = ProfilePermissionGuard::new(&paths, selection.clone());
        let snapshot = guard.snapshot_read_policy().unwrap();
        assert!(snapshot.check_read_path("Note.md").is_ok());
        fs::write(&hook, "function policy_hook(input) { return 'deny'; }").unwrap();
        assert!(snapshot.check_read_path("Note.md").is_ok());
        assert!(guard.check_read_path("Note.md").is_err());
        assert!(snapshot.recheck_read_policy_snapshot().is_err());
        assert!(guard
            .snapshot_read_policy()
            .unwrap()
            .check_read_path("Note.md")
            .is_err());
        fs::write(&hook, allow).unwrap();
        assert!(snapshot.recheck_read_policy_snapshot().is_ok());
        fs::remove_file(&hook).unwrap();
        assert!(snapshot.check_read_path("Note.md").is_ok());
        assert!(snapshot.recheck_read_policy_snapshot().is_err());
        assert!(guard.snapshot_read_policy().is_err());
        let disabled = ProfilePermissionGuard::without_policy_hooks(&paths, selection);
        assert!(disabled
            .snapshot_read_policy()
            .unwrap()
            .check_read_path("Note.md")
            .is_ok());
        fs::write(&hook, allow).unwrap();
        fs::write(&trust, "{\"vaults\":[]}").unwrap();
        assert!(snapshot.check_read_path("Note.md").is_err());
        assert!(snapshot.recheck_read_policy_snapshot().is_err());
        assert!(guard.snapshot_read_policy().is_err());
    }

    #[test]
    fn policy_decision_distinguishes_explicit_denial_from_invalid_results() {
        for value in [
            serde_json::json!("deny"),
            serde_json::json!({"decision": "deny", "reason": "classified"}),
        ] {
            assert!(matches!(
                interpret_policy_hook_result("test", "read", Some("Note.md"), Some(value)),
                Err(PermissionError::PolicyHookDenied { .. })
            ));
        }
        for value in [
            None,
            Some(serde_json::json!(true)),
            Some(serde_json::json!("allow")),
            Some(serde_json::json!({"decision": "invalid"})),
        ] {
            assert!(matches!(
                interpret_policy_hook_result("test", "read", Some("Note.md"), value),
                Err(PermissionError::PolicyHookFailed { .. })
            ));
        }
        assert!(interpret_policy_hook_result(
            "test",
            "read",
            Some("Note.md"),
            Some(serde_json::json!("pass"))
        )
        .is_ok());
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
    fn namespace_coverage_is_policy_based_and_deny_aware() {
        let covered = PathPermission {
            allow: vec![ResourceSpecifier::Folder("tasks/**".to_string())],
            deny: Vec::new(),
        };
        assert!(covered.covers_path_namespace("tasks/published/**/*.md"));
        assert!(!covered.covers_path_namespace("projects/**/*.md"));

        let denied_subtree = PathPermission {
            allow: vec![ResourceSpecifier::Folder("tasks/**".to_string())],
            deny: vec![ResourceSpecifier::Folder("tasks/private/**".to_string())],
        };
        assert!(!denied_subtree.covers_path_namespace("tasks/**/*.md"));

        let tag_only = PathPermission {
            allow: vec![ResourceSpecifier::Tag("task".to_string())],
            deny: Vec::new(),
        };
        assert!(!tag_only.covers_path_namespace("tasks/**/*.md"));
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
