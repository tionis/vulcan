use super::{MdbaseCollection, MdbaseTypeRegistry};
use crate::permissions::PermissionGuard;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

/// Policy inputs known before an mdbase write inspects record-dependent
/// constraints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MdbaseWriteAuthorizationRequest {
    /// Vault-relative existing paths whose source state participates.
    pub read_paths: Vec<String>,
    /// Vault-relative paths the operation may create, replace, rename, or delete.
    pub write_paths: Vec<String>,
    /// Type membership computed from the proposed draft.
    pub matched_types: Vec<String>,
}

/// Scope proven visible before global validation may inspect collection data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseAuthorizedValidationScope {
    pub read_paths: Vec<String>,
    pub write_paths: Vec<String>,
    /// Vault-relative glob namespaces that must be completely readable.
    pub record_namespaces: Vec<String>,
    /// Equivalent collection-relative namespaces for preview state binding.
    pub collection_record_namespaces: Vec<String>,
}

/// A deliberately non-oracular authorization failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWriteAuthorizationError {
    pub code: String,
    pub message: String,
}

impl MdbaseWriteAuthorizationError {
    fn permission_denied() -> Self {
        Self {
            code: "permission_denied".to_string(),
            message: "permission denied: mdbase write requires complete validation-scope visibility and affected-path authority".to_string(),
        }
    }
}

impl Display for MdbaseWriteAuthorizationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MdbaseWriteAuthorizationError {}

/// Prove the authority required to evaluate record-dependent mdbase rules.
///
/// This preflight intentionally accepts no discovered record list and performs
/// no filesystem reads. Its result therefore cannot vary with a hidden record,
/// hidden conflict, or phantom record. Callers must run it before uniqueness or
/// link-existence validation and must not substitute a permission-filtered read
/// result for this proof. `collection_permission_prefix` locates the collection
/// root in the guard's vault-relative permission namespace; use an empty string
/// for a collection at the vault root.
pub fn authorize_mdbase_write_validation_scope(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    collection_permission_prefix: &str,
    request: &MdbaseWriteAuthorizationRequest,
    guard: &dyn PermissionGuard,
) -> Result<MdbaseAuthorizedValidationScope, MdbaseWriteAuthorizationError> {
    let read_paths = normalized_paths(&request.read_paths);
    let write_paths = normalized_paths(&request.write_paths);

    for path in &read_paths {
        guard
            .check_read_path(path)
            .map_err(|_| MdbaseWriteAuthorizationError::permission_denied())?;
    }
    for path in &write_paths {
        guard
            .check_write_path(path)
            .map_err(|_| MdbaseWriteAuthorizationError::permission_denied())?;
    }

    let collection_record_namespaces =
        required_record_namespaces(collection, types, &request.matched_types);
    let record_namespaces = collection_record_namespaces
        .iter()
        .map(|namespace| prefixed_namespace(collection_permission_prefix, namespace))
        .collect::<Vec<_>>();
    if !record_namespaces.is_empty()
        && (guard.has_policy_hook()
            || record_namespaces
                .iter()
                .any(|pattern| !guard.grant().read.covers_path_namespace(pattern)))
    {
        return Err(MdbaseWriteAuthorizationError::permission_denied());
    }

    Ok(MdbaseAuthorizedValidationScope {
        read_paths,
        write_paths,
        record_namespaces,
        collection_record_namespaces,
    })
}

fn required_record_namespaces(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    matched_types: &[String],
) -> Vec<String> {
    let mut namespaces = BTreeSet::new();
    let mut collection_wide = false;
    for type_name in matched_types {
        let Some(definition) = types.get(type_name) else {
            continue;
        };
        if definition
            .frontmatter
            .pointer("/collection/links")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|links| {
                links.values().any(|rule| {
                    let validates_existence = rule
                        .get("validate_exists")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let constrains_type = rule
                        .get("target_type")
                        .is_some_and(|target| target.as_str() != Some("any"));
                    validates_existence || constrains_type
                })
            })
        {
            collection_wide = true;
        }
        let unique = definition
            .frontmatter
            .pointer("/collection/unique")
            .and_then(serde_json::Value::as_array);
        for rule in unique.into_iter().flatten() {
            match rule
                .get("scope")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("collection")
            {
                "path_glob" => {
                    if let Some(pattern) = rule.get("path_glob").and_then(serde_json::Value::as_str)
                    {
                        namespaces.insert(pattern.replace('\\', "/"));
                    }
                }
                // Explicit type declarations may occur in any candidate record,
                // so type scope requires the same namespace proof as collection
                // scope. Inspecting current membership to narrow this would be
                // a hidden-record oracle.
                _ => collection_wide = true,
            }
        }
    }
    if collection_wide {
        namespaces.extend(collection_record_namespaces(collection));
    }
    namespaces.into_iter().collect()
}

fn collection_record_namespaces(collection: &MdbaseCollection) -> Vec<String> {
    let mut namespaces = Vec::new();
    for extension in &collection.config.settings.record_extensions {
        namespaces.push(format!("*.{extension}"));
        if collection.config.settings.include_subfolders {
            namespaces.push(format!("**/*.{extension}"));
        }
    }
    namespaces.sort();
    namespaces.dedup();
    namespaces
}

fn prefixed_namespace(prefix: &str, namespace: &str) -> String {
    let prefix = prefix.replace('\\', "/");
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        namespace.to_string()
    } else {
        format!("{prefix}/{namespace}")
    }
}

fn normalized_paths(paths: &[String]) -> Vec<String> {
    let mut paths = paths
        .iter()
        .map(|path| path.replace('\\', "/"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ConfigPermissionMode, NetworkPermissionConfig, PathPermissionConfig, PermissionLimit,
        PermissionMode, PermissionProfile,
    };
    use crate::mdbase::{load_mdbase_collection, load_mdbase_type_registry};
    use crate::paths::VaultPaths;
    use crate::permissions::{
        PathPermission, PermissionGrant, ProfilePermissionGuard, ResolvedPermissionProfile,
        ResourceLimits, ResourceSpecifier,
    };
    use std::fs;
    use tempfile::tempdir;

    fn fixture(
        unique: &str,
        links: &str,
    ) -> (tempfile::TempDir, MdbaseCollection, MdbaseTypeRegistry) {
        let directory = tempdir().expect("temp directory");
        fs::write(
            directory.path().join("mdbase.yaml"),
            "spec_version: \"0.3.0\"\n",
        )
        .expect("config");
        fs::create_dir_all(directory.path().join("_types")).expect("types directory");
        fs::write(
            directory.path().join("_types/task.md"),
            format!(
                "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  value: {{type: object}}\ncollection:\n{unique}{links}---\n"
            ),
        )
        .expect("type file");
        let collection = load_mdbase_collection(directory.path())
            .expect("collection loads")
            .expect("collection exists");
        let types = load_mdbase_type_registry(&collection).expect("types load");
        (directory, collection, types)
    }

    fn guard(
        root: &std::path::Path,
        read: PathPermission,
        write: PathPermission,
    ) -> ProfilePermissionGuard {
        let grant = PermissionGrant {
            read,
            write,
            refactor: PathPermission::default(),
            git: false,
            network: false,
            network_domains: Vec::new(),
            index: false,
            config_read: false,
            config_write: false,
            execute: false,
            shell: false,
            limits: ResourceLimits::default(),
        };
        let profile = PermissionProfile {
            read: PathPermissionConfig::default(),
            write: PathPermissionConfig::default(),
            refactor: PathPermissionConfig::default(),
            git: PermissionMode::Deny,
            network: NetworkPermissionConfig::default(),
            index: PermissionMode::Deny,
            config: ConfigPermissionMode::None,
            execute: PermissionMode::Deny,
            shell: PermissionMode::Deny,
            cpu_limit_ms: PermissionLimit::default(),
            memory_limit_mb: PermissionLimit::default(),
            stack_limit_kb: PermissionLimit::default(),
            policy_hook: None,
        };
        ProfilePermissionGuard::new(
            &VaultPaths::new(root),
            ResolvedPermissionProfile {
                name: "test".to_string(),
                profile,
                grant,
            },
        )
    }

    fn request() -> MdbaseWriteAuthorizationRequest {
        MdbaseWriteAuthorizationRequest {
            read_paths: vec!["tasks/public.md".to_string()],
            write_paths: vec!["tasks/public.md".to_string()],
            matched_types: vec!["task".to_string()],
        }
    }

    #[test]
    fn collection_uniqueness_requires_policy_coverage_not_discovered_records() {
        let (directory, collection, types) =
            fixture("  unique:\n    - {field: id, scope: collection}\n", "");
        let narrow = guard(
            directory.path(),
            PathPermission {
                allow: vec![ResourceSpecifier::Note("tasks/public.md".to_string())],
                deny: Vec::new(),
            },
            PathPermission {
                allow: vec![ResourceSpecifier::Note("tasks/public.md".to_string())],
                deny: Vec::new(),
            },
        );
        let first =
            authorize_mdbase_write_validation_scope(&collection, &types, "", &request(), &narrow)
                .expect_err("narrow visibility cannot prove uniqueness");
        fs::create_dir_all(directory.path().join("tasks/private")).expect("private directory");
        fs::write(
            directory.path().join("tasks/private/conflict.md"),
            "---\ntype: task\nid: duplicate\n---\n",
        )
        .expect("hidden conflict");
        let second =
            authorize_mdbase_write_validation_scope(&collection, &types, "", &request(), &narrow)
                .expect_err("hidden record cannot change denial");
        assert_eq!(first, second);
        assert_eq!(first.code, "permission_denied");
        assert!(!first.message.contains("private"));
        assert!(!first.message.contains("conflict"));
    }

    #[test]
    fn path_glob_uniqueness_accepts_complete_bounded_visibility() {
        let (directory, collection, types) = fixture(
            "  unique:\n    - {field: slug, scope: path_glob, path_glob: 'published/**'}\n",
            "",
        );
        let allowed = guard(
            directory.path(),
            PathPermission {
                allow: vec![
                    ResourceSpecifier::Note("tasks/public.md".to_string()),
                    ResourceSpecifier::Folder("published/**".to_string()),
                ],
                deny: Vec::new(),
            },
            PathPermission {
                allow: vec![ResourceSpecifier::Note("tasks/public.md".to_string())],
                deny: Vec::new(),
            },
        );
        let scope =
            authorize_mdbase_write_validation_scope(&collection, &types, "", &request(), &allowed)
                .expect("bounded namespace is visible");
        assert_eq!(scope.record_namespaces, ["published/**"]);
    }

    #[test]
    fn existence_checked_links_require_the_complete_candidate_namespace() {
        let (directory, collection, types) = fixture(
            "",
            "  links:\n    related:\n      target_type: any\n      validate_exists: true\n",
        );
        let unrestricted = PathPermission {
            allow: vec![ResourceSpecifier::All],
            deny: Vec::new(),
        };
        let allowed = guard(directory.path(), unrestricted.clone(), unrestricted);
        let scope =
            authorize_mdbase_write_validation_scope(&collection, &types, "", &request(), &allowed)
                .expect("unrestricted scope is authorized");
        assert_eq!(scope.record_namespaces, ["**/*.md", "*.md"]);
    }

    #[test]
    fn target_type_constraints_require_scope_even_when_missing_targets_are_allowed() {
        let (directory, collection, types) = fixture(
            "",
            "  links:\n    parent:\n      target_type: task\n      validate_exists: false\n",
        );
        let exact = PathPermission {
            allow: vec![ResourceSpecifier::Note("tasks/public.md".to_string())],
            deny: Vec::new(),
        };
        let narrow = guard(directory.path(), exact.clone(), exact);
        let error =
            authorize_mdbase_write_validation_scope(&collection, &types, "", &request(), &narrow)
                .expect_err("target type validation may not inspect hidden records");
        assert_eq!(error.code, "permission_denied");
    }

    #[test]
    fn local_rules_without_global_constraints_keep_narrow_writes_available() {
        let (directory, collection, types) = fixture("", "");
        let exact = PathPermission {
            allow: vec![ResourceSpecifier::Note("tasks/public.md".to_string())],
            deny: Vec::new(),
        };
        let allowed = guard(directory.path(), exact.clone(), exact);
        let scope =
            authorize_mdbase_write_validation_scope(&collection, &types, "", &request(), &allowed)
                .expect("local-only validation does not require collection visibility");
        assert!(scope.record_namespaces.is_empty());
    }

    #[test]
    fn collection_relative_constraints_are_proved_in_the_vault_permission_domain() {
        let (directory, collection, types) = fixture(
            "  unique:\n    - {field: slug, scope: path_glob, path_glob: 'published/**'}\n",
            "",
        );
        let read = PathPermission {
            allow: vec![
                ResourceSpecifier::Note("collections/work/tasks/public.md".to_string()),
                ResourceSpecifier::Folder("collections/work/published/**".to_string()),
            ],
            deny: Vec::new(),
        };
        let write = PathPermission {
            allow: vec![ResourceSpecifier::Note(
                "collections/work/tasks/public.md".to_string(),
            )],
            deny: Vec::new(),
        };
        let allowed = guard(directory.path(), read, write);
        let mut request = request();
        request.read_paths = vec!["collections/work/tasks/public.md".to_string()];
        request.write_paths = request.read_paths.clone();
        let scope = authorize_mdbase_write_validation_scope(
            &collection,
            &types,
            "collections/work",
            &request,
            &allowed,
        )
        .expect("prefixed collection namespace is visible");
        assert_eq!(scope.record_namespaces, ["collections/work/published/**"]);
    }
}
