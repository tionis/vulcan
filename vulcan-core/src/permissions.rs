use crate::config::{
    load_permission_profiles, ConfigPermissionMode, PathPermissionConfig, PathPermissionKeyword,
    PathPermissionRules, PermissionLimit, PermissionLimitKeyword, PermissionMode,
    PermissionProfile,
};
use crate::paths::VaultPaths;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

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

    fn check_read_path(&self, path: &str) -> Result<(), PermissionError> {
        if self.read_filter().is_allowed(path) {
            Ok(())
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
            Ok(())
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
            Ok(())
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
            Ok(())
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
            Ok(())
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "git access",
            })
        }
    }

    fn check_shell(&self) -> Result<(), PermissionError> {
        if self.grant().shell {
            Ok(())
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "shell access",
            })
        }
    }

    fn check_index(&self) -> Result<(), PermissionError> {
        if self.grant().index {
            Ok(())
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "index access",
            })
        }
    }

    fn check_execute(&self) -> Result<(), PermissionError> {
        if self.grant().execute {
            Ok(())
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "execute access",
            })
        }
    }

    fn check_config_read(&self) -> Result<(), PermissionError> {
        if self.grant().config_read {
            Ok(())
        } else {
            Err(PermissionError::CapabilityDenied {
                profile: self.profile_name().to_string(),
                capability: "config read access",
            })
        }
    }

    fn check_config_write(&self) -> Result<(), PermissionError> {
        if self.grant().config_write {
            Ok(())
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
    selection: ResolvedPermissionProfile,
}

impl ProfilePermissionGuard {
    #[must_use]
    pub fn new(selection: ResolvedPermissionProfile) -> Self {
        Self { selection }
    }

    #[must_use]
    pub fn selection(&self) -> &ResolvedPermissionProfile {
        &self.selection
    }
}

impl PermissionGuard for ProfilePermissionGuard {
    fn profile_name(&self) -> &str {
        &self.selection.name
    }

    fn grant(&self) -> &PermissionGrant {
        &self.selection.grant
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

fn specifier_matches_path(specifier: &ResourceSpecifier, path: &str, tags: &[String]) -> bool {
    match specifier {
        ResourceSpecifier::All => true,
        ResourceSpecifier::Folder(pattern) => glob_matches(pattern, path),
        ResourceSpecifier::Tag(tag) => tags.iter().any(|candidate| tag_matches(tag, candidate)),
        ResourceSpecifier::Note(note) => normalize_permission_path(note) == path,
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

fn tag_matches(expected: &str, candidate: &str) -> bool {
    let expected = expected.trim_start_matches('#');
    let candidate = candidate.trim_start_matches('#');
    candidate == expected || candidate.starts_with(&format!("{expected}/"))
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
}
