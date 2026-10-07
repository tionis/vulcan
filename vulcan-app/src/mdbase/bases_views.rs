//! Obsidian `.base` files as saved-view sources (mdbase Chapter 15).
//!
//! Sources are selected by `x-obsidian.bases.include` and stay authoritative:
//! nothing is converted to a view record, and execution uses Vulcan's Bases
//! evaluator (the Bases expression language, not mdbase CEL). The adapter only
//! maps discovery, revisions, and results into the saved-view envelopes.

use super::{AppError, LoadedCollection, MdbaseViewSourceDeletion, MdbaseViewSourceDocument};
use super::{MdbaseViewSourceOptions, VaultPaths};
use crate::commit::AutoCommitPolicy;
use std::path::Path;
use vulcan_core::bases::{BasesEvaluatedView, BasesEvaluator};
use vulcan_core::mdbase::{
    derive_mdbase_view_ids, discover_mdbase_obsidian_base_sources, mdbase_content_revision,
    MdbaseDiagnostic, MdbaseDiagnosticLevel, MdbaseNamedViewDescriptor, MdbaseQueryGroup,
    MdbaseQueryMeta, MdbaseQueryResult, MdbaseQueryRow, MdbaseQueryViewMeta, MdbaseViewContextArg,
    MdbaseViewInvocation, MdbaseViewProperty, MdbaseViewSource, MdbaseViewSourceRef,
    MDBASE_OBSIDIAN_BASE_SOURCE_FORMAT,
};
use vulcan_core::paths::{
    secure_create_atomic, secure_read_to_string, secure_remove, secure_write_atomic,
};
use vulcan_core::{PermissionFilter, PermissionGuard, ProfilePermissionGuard};

/// Visible `.base` sources selected by the collection's include globs.
pub(super) fn base_source_paths(
    loaded: &LoadedCollection,
    filter: Option<&PermissionFilter>,
) -> Result<Vec<String>, AppError> {
    Ok(discover_mdbase_obsidian_base_sources(&loaded.collection)
        .map_err(AppError::operation)?
        .into_iter()
        .filter(|path| filter.is_none_or(|filter| filter.is_allowed(path)))
        .collect())
}

/// Describe every visible source; malformed ones become warnings.
pub(super) fn describe_base_sources(
    loaded: &LoadedCollection,
    filter: Option<&PermissionFilter>,
) -> Result<(Vec<MdbaseViewSource>, Vec<MdbaseDiagnostic>), AppError> {
    let mut sources = Vec::new();
    let mut diagnostics = Vec::new();
    for path in base_source_paths(loaded, filter)? {
        let document = secure_read_to_string(&loaded.collection.root, Path::new(&path))
            .map_err(AppError::operation)?;
        match describe_base_source(&path, &document) {
            Ok(source) => sources.push(source),
            Err(message) => diagnostics.push(MdbaseDiagnostic {
                severity: MdbaseDiagnosticLevel::Warning,
                code: "invalid_view".to_string(),
                message,
                path: Some(path),
                field: None,
                type_name: None,
                schema_location: None,
                details: None,
            }),
        }
    }
    Ok((sources, diagnostics))
}

/// The saved-view descriptor of one `.base` document, or why it is invalid.
fn describe_base_source(path: &str, document: &str) -> Result<MdbaseViewSource, String> {
    BasesEvaluator::new()
        .inspect_yaml(path, document)
        .map_err(|error| format!("`{path}` is not a valid Obsidian base: {error}"))?;
    let yaml = serde_yaml::from_str::<serde_yaml::Value>(document)
        .map_err(|error| format!("`{path}` is not valid YAML: {error}"))?;
    let views = yaml
        .get("views")
        .and_then(serde_yaml::Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    let names = views
        .iter()
        .map(|view| view.get("name").and_then(serde_yaml::Value::as_str))
        .collect::<Vec<_>>();
    let ids = derive_mdbase_view_ids(&names);
    let display_name = |key: &str| {
        yaml.get("properties")
            .and_then(|properties| properties.get(key))
            .and_then(|property| property.get("displayName"))
            .and_then(serde_yaml::Value::as_str)
            .map(ToString::to_string)
    };
    let stem = Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(path)
        .to_string();
    Ok(MdbaseViewSource {
        id: path.to_string(),
        name: stem,
        description: None,
        source: MdbaseViewSourceRef {
            path: path.to_string(),
            format: MDBASE_OBSIDIAN_BASE_SOURCE_FORMAT.to_string(),
            revision: mdbase_content_revision(document),
            writable: true,
        },
        views: views
            .iter()
            .zip(ids)
            .zip(&names)
            .map(|((view, id), name)| MdbaseNamedViewDescriptor {
                name: name.map_or_else(|| id.clone(), ToString::to_string),
                id,
                description: None,
                properties: view
                    .get("order")
                    .and_then(serde_yaml::Value::as_sequence)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_yaml::Value::as_str)
                    .map(|key| MdbaseViewProperty {
                        key: key.to_string(),
                        label: display_name(key),
                        description: None,
                        format: None,
                        hidden: None,
                    })
                    .collect(),
                presentation: view
                    .get("type")
                    .and_then(serde_yaml::Value::as_str)
                    .map(|kind| serde_json::json!({"type": kind})),
            })
            .collect(),
    })
}

/// Execute one named view of a visible `.base` source with the Bases
/// evaluator and return the headless saved-view envelope.
pub(super) fn execute_base_view(
    paths: &VaultPaths,
    loaded: &LoadedCollection,
    invocation: &MdbaseViewInvocation,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseQueryResult, AppError> {
    if !base_source_paths(loaded, filter)?.contains(&invocation.source) {
        return Err(not_found(&invocation.source));
    }
    if invocation.render {
        return Err(AppError::operation_with_code(
            "unsupported_presentation",
            "rendered view output is not supported; request the headless result",
        ));
    }
    if matches!(invocation.context, MdbaseViewContextArg::Path(_)) {
        // The Bases evaluator binds `this` to the current row; it has no
        // separate invocation context yet, so an explicit one cannot be honored.
        return Err(AppError::operation_with_code(
            "unsupported_context",
            "Obsidian base views do not accept an explicit invocation context yet",
        ));
    }
    let document = secure_read_to_string(&loaded.collection.root, Path::new(&invocation.source))
        .map_err(AppError::operation)?;
    let descriptor = describe_base_source(&invocation.source, &document)
        .map_err(|message| AppError::operation_with_code("invalid_view", message))?;
    let index = descriptor
        .views
        .iter()
        .position(|view| view.id == invocation.view)
        .ok_or_else(|| {
            AppError::operation_with_code(
                "view_not_found",
                format!(
                    "base `{}` has no view `{}`",
                    invocation.source, invocation.view
                ),
            )
        })?;
    let report = BasesEvaluator::new()
        .evaluate_yaml_with_filter(
            paths,
            &invocation.source,
            &headless_view_yaml(&document, index)?,
            filter,
        )
        .map_err(AppError::operation)?;
    let view = report.views.first().ok_or_else(|| {
        AppError::operation(format!(
            "base `{}` view `{}` did not evaluate",
            invocation.source, invocation.view
        ))
    })?;
    let diagnostics = report
        .diagnostics
        .iter()
        .map(|diagnostic| MdbaseDiagnostic {
            severity: MdbaseDiagnosticLevel::Warning,
            code: "bases_evaluation".to_string(),
            message: diagnostic.message.clone(),
            path: Some(invocation.source.clone()),
            field: diagnostic.path.clone(),
            type_name: None,
            schema_location: None,
            details: None,
        })
        .collect();
    Ok(base_result(
        view,
        invocation,
        MdbaseQueryViewMeta {
            path: invocation.source.clone(),
            id: invocation.view.clone(),
        },
        diagnostics,
    ))
}

/// The source reduced to one view for headless evaluation. A view's `type`
/// only selects a renderer, so types the evaluator does not render evaluate
/// as tables; `TaskNotes` types keep their own source semantics. Nothing is
/// written back: the `.base` file stays authoritative.
fn headless_view_yaml(document: &str, index: usize) -> Result<String, AppError> {
    let mut yaml = serde_yaml::from_str::<serde_yaml::Value>(document)
        .map_err(|error| AppError::operation_with_code("invalid_view", error))?;
    let views = yaml
        .get_mut("views")
        .and_then(serde_yaml::Value::as_sequence_mut)
        .ok_or_else(|| AppError::operation_with_code("invalid_view", "base has no views"))?;
    let mut view = views.swap_remove(index);
    let rendered = view
        .get("type")
        .and_then(serde_yaml::Value::as_str)
        .is_some_and(|kind| {
            kind.eq_ignore_ascii_case("table") || kind.to_ascii_lowercase().starts_with("tasknotes")
        });
    if !rendered {
        if let Some(view) = view.as_mapping_mut() {
            view.insert("type".into(), "table".into());
        }
    }
    *views = vec![view];
    serde_yaml::to_string(&yaml).map_err(AppError::operation)
}

fn base_result(
    view: &BasesEvaluatedView,
    invocation: &MdbaseViewInvocation,
    meta_view: MdbaseQueryViewMeta,
    diagnostics: Vec<MdbaseDiagnostic>,
) -> MdbaseQueryResult {
    let total_count = view.rows.len();
    let start = invocation.offset.unwrap_or(0).min(total_count);
    let end = invocation.limit.map_or(total_count, |limit| {
        start.saturating_add(limit).min(total_count)
    });
    // Groups describe the complete match set, in first-appearance order.
    let groups = view.group_by.as_ref().map(|group_by| {
        let mut groups = Vec::<MdbaseQueryGroup>::new();
        for row in &view.rows {
            let value = row.group_value.clone().unwrap_or(serde_json::Value::Null);
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.values.get(&group_by.property) == Some(&value))
            {
                group.count += 1;
            } else {
                groups.push(MdbaseQueryGroup {
                    values: serde_json::Map::from_iter([(group_by.property.clone(), value)]),
                    count: 1,
                    summaries: serde_json::Map::new(),
                });
            }
        }
        groups
    });
    MdbaseQueryResult {
        results: view.rows[start..end]
            .iter()
            .map(|row| MdbaseQueryRow {
                file: serde_json::json!({"path": row.document_path}),
                frontmatter: None,
                effective_frontmatter: None,
                values: Some(serde_json::Value::Object(
                    view.columns
                        .iter()
                        .map(|column| {
                            (
                                column.key.clone(),
                                row.cells
                                    .get(&column.key)
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null),
                            )
                        })
                        .collect(),
                )),
                body: None,
            })
            .collect(),
        meta: MdbaseQueryMeta {
            total_count,
            has_more: end < total_count,
            context: None,
            groups,
            view: Some(meta_view),
        },
        diagnostics,
    }
}

/// `read_view_source` for a visible `.base` source.
pub(super) fn read_base_source(
    loaded: &LoadedCollection,
    path: &str,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseViewSourceDocument, AppError> {
    if !base_source_paths(loaded, filter)?
        .iter()
        .any(|source| source == path)
    {
        return Err(not_found(path));
    }
    let document = secure_read_to_string(&loaded.collection.root, Path::new(path))
        .map_err(AppError::operation)?;
    Ok(base_document(path, document))
}

/// Create, replace, or delete a `.base` source under the vault write lock.
/// A candidate document must parse as a base and the path must be selected
/// by the include globs, so the result is always a listed source.
pub(super) fn write_base_source(
    paths: &VaultPaths,
    path: &str,
    document: Option<&str>,
    mode: BaseWrite<'_>,
    options: &MdbaseViewSourceOptions,
) -> Result<Option<MdbaseViewSourceDocument>, AppError> {
    let guard = ProfilePermissionGuard::new(
        paths,
        vulcan_core::resolve_permission_profile(paths, options.permission_profile.as_deref())
            .map_err(AppError::operation)?,
    );
    super::authorize_affected_paths(&guard, &[path.to_string()])?;
    let loaded = super::load_collection_authorized(paths, Some(&guard.read_filter()))?;
    let root = loaded.collection.root.clone();
    let selected = vulcan_core::mdbase::discover_mdbase_obsidian_base_sources(&loaded.collection)
        .map_err(AppError::operation)?
        .iter()
        .any(|source| source == path);
    if let Some(document) = document {
        describe_base_source(path, document)
            .map_err(|message| AppError::operation_with_code("invalid_view", message))?;
        let included = loaded
            .collection
            .config
            .obsidian_bases
            .as_ref()
            .is_some_and(|bases| base_path_included(&bases.include, path));
        if !included {
            return Err(AppError::operation_with_code(
                "invalid_view",
                format!("`{path}` is not selected by x-obsidian.bases.include"),
            ));
        }
    } else if !selected {
        return Err(not_found(path));
    }
    drop(loaded);
    let lock = vulcan_core::write_lock::acquire_write_lock(paths).map_err(AppError::operation)?;
    let current = secure_read_to_string(&root, Path::new(path)).ok();
    match mode {
        BaseWrite::Create => {
            if current.is_some() || root.join(path).exists() {
                return Err(AppError::operation_with_code(
                    "path_conflict",
                    format!("a file already exists at `{path}`"),
                ));
            }
        }
        BaseWrite::Replace { if_revision } => {
            let Some(current) = current.as_deref() else {
                return Err(not_found(path));
            };
            if !selected {
                return Err(not_found(path));
            }
            if if_revision.is_some_and(|expected| expected != mdbase_content_revision(current)) {
                return Err(AppError::operation_with_code(
                    "concurrent_modification",
                    "the view source no longer matches if_revision; read it and retry",
                ));
            }
        }
    }
    if options.dry_run {
        return Ok(document.map(|document| base_document(path, document.to_string())));
    }
    match (mode, document) {
        (BaseWrite::Create, Some(document)) => {
            secure_create_atomic(&root, Path::new(path), document).map_err(AppError::operation)?;
        }
        (BaseWrite::Replace { .. }, Some(document)) => {
            secure_write_atomic(&root, Path::new(path), document).map_err(AppError::operation)?;
        }
        (_, None) => secure_remove(&root, Path::new(path)).map_err(AppError::operation)?,
    }
    drop(lock);
    AutoCommitPolicy::for_mutation(paths, options.no_commit)
        .commit(
            paths,
            "view-source",
            &[path.to_string()],
            options.permission_profile.as_deref(),
            options.quiet,
        )
        .map_err(AppError::operation)?;
    Ok(document.map(|document| base_document(path, document.to_string())))
}

#[derive(Debug, Clone, Copy)]
pub(super) enum BaseWrite<'a> {
    Create,
    Replace { if_revision: Option<&'a str> },
}

pub(super) fn base_deletion(
    path: &str,
    options: &MdbaseViewSourceOptions,
) -> MdbaseViewSourceDeletion {
    MdbaseViewSourceDeletion {
        path: path.to_string(),
        deleted: !options.dry_run,
        dry_run: options.dry_run,
    }
}

fn base_path_included(include: &[String], path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("base"))
        && include.iter().any(|pattern| {
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .backslash_escape(false)
                .build()
                .is_ok_and(|glob| glob.compile_matcher().is_match(path))
        })
}

fn base_document(path: &str, document: String) -> MdbaseViewSourceDocument {
    MdbaseViewSourceDocument {
        path: path.to_string(),
        format: MDBASE_OBSIDIAN_BASE_SOURCE_FORMAT.to_string(),
        revision: mdbase_content_revision(&document),
        document,
    }
}

fn not_found(path: &str) -> AppError {
    AppError::operation_with_code(
        "view_not_found",
        format!("no saved-view source has path `{path}`"),
    )
}

#[cfg(test)]
mod tests;
