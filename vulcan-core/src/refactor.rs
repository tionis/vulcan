use crate::graph::{resolve_note_reference, GraphQueryError};
use crate::parser::{parse_document, RawBlockRef, RawHeading, RawLink};
use crate::scan::{discover_relative_paths, scan_vault_unlocked, ScanError, ScanMode};
use crate::write_lock::acquire_write_lock;
use crate::{load_vault_config, VaultPaths};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_yaml::Value as YamlValue;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;

#[derive(Debug)]
pub enum RefactorError {
    AmbiguousTarget {
        path: String,
        kind: &'static str,
        value: String,
    },
    DuplicateTarget {
        path: String,
        kind: &'static str,
        value: String,
    },
    Graph(GraphQueryError),
    InvalidFrontmatterRoot {
        path: String,
    },
    Io(std::io::Error),
    MissingLinkSpan {
        path: String,
        byte_offset: usize,
    },
    MissingTarget {
        path: String,
        kind: &'static str,
        value: String,
    },
    Scan(ScanError),
    Sqlite(rusqlite::Error),
    Yaml(serde_yaml::Error),
}

impl Display for RefactorError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AmbiguousTarget { path, kind, value } => {
                write!(
                    formatter,
                    "multiple {kind} entries named '{value}' in {path}"
                )
            }
            Self::DuplicateTarget { path, kind, value } => {
                write!(formatter, "{path} already contains {kind} '{value}'")
            }
            Self::Graph(error) => write!(formatter, "{error}"),
            Self::InvalidFrontmatterRoot { path } => {
                write!(
                    formatter,
                    "{path} has non-mapping frontmatter; cannot edit properties"
                )
            }
            Self::Io(error) => write!(formatter, "{error}"),
            Self::MissingLinkSpan { path, byte_offset } => write!(
                formatter,
                "failed to locate cached link at byte offset {byte_offset} in {path}"
            ),
            Self::MissingTarget { path, kind, value } => {
                write!(formatter, "no {kind} named '{value}' found in {path}")
            }
            Self::Scan(error) => write!(formatter, "{error}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
            Self::Yaml(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for RefactorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Graph(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Scan(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Yaml(error) => Some(error),
            Self::InvalidFrontmatterRoot { .. }
            | Self::AmbiguousTarget { .. }
            | Self::DuplicateTarget { .. }
            | Self::MissingLinkSpan { .. }
            | Self::MissingTarget { .. } => None,
        }
    }
}

impl From<GraphQueryError> for RefactorError {
    fn from(error: GraphQueryError) -> Self {
        Self::Graph(error)
    }
}

impl From<std::io::Error> for RefactorError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ScanError> for RefactorError {
    fn from(error: ScanError) -> Self {
        Self::Scan(error)
    }
}

impl From<rusqlite::Error> for RefactorError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<serde_yaml::Error> for RefactorError {
    fn from(error: serde_yaml::Error) -> Self {
        Self::Yaml(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefactorReport {
    pub dry_run: bool,
    pub action: String,
    pub files: Vec<RefactorFileReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefactorFileReport {
    pub path: String,
    pub changes: Vec<RefactorChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefactorChange {
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextEdit {
    start: usize,
    end: usize,
    replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FilePlan {
    path: String,
    updated_contents: String,
    changes: Vec<RefactorChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrontmatterBlock {
    full_start: usize,
    full_end: usize,
    yaml_start: usize,
    yaml_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedSubpathLink {
    source_path: String,
    raw_text: String,
    byte_offset: usize,
}

pub fn rename_property(
    paths: &VaultPaths,
    old_key: &str,
    new_key: &str,
    dry_run: bool,
) -> Result<RefactorReport, RefactorError> {
    let _lock = acquire_write_lock(paths)?;
    let mut plans = Vec::new();

    for path in markdown_note_paths(paths)? {
        let source = fs::read_to_string(paths.vault_root().join(&path))?;
        let mut edits = Vec::new();
        let mut changes = Vec::new();

        // Try surgical rename (preserves formatting) before falling back to full round-trip.
        let surgical_result = plan_surgical_frontmatter_edit(&source, |yaml| {
            // Abort surgical path if new_key already exists (fall back for proper error handling).
            if old_key != new_key && find_yaml_key_span(yaml, new_key).is_some() {
                return None;
            }
            let new_yaml = surgical_rename_key(yaml, old_key, new_key)?;
            Some((
                new_yaml,
                vec![RefactorChange {
                    before: old_key.to_string(),
                    after: new_key.to_string(),
                }],
            ))
        });

        let result = if surgical_result.is_some() {
            surgical_result
        } else {
            plan_frontmatter_replacement(&source, |frontmatter| {
                rename_frontmatter_property(frontmatter, &path, old_key, new_key)
            })?
        };

        if let Some((edit, edit_changes)) = result {
            edits.push(edit);
            changes.extend(edit_changes);
        }

        if let Some(plan) = build_file_plan(&path, &source, &edits, changes) {
            plans.push(plan);
        }
    }

    finalize_refactor(paths, dry_run, "rename_property", plans)
}

pub fn merge_tags(
    paths: &VaultPaths,
    source_tag: &str,
    destination_tag: &str,
    dry_run: bool,
) -> Result<RefactorReport, RefactorError> {
    let _lock = acquire_write_lock(paths)?;
    let source_tag = normalize_tag_name(source_tag);
    let destination_tag = normalize_tag_name(destination_tag);
    let config = load_vault_config(paths).config;
    let mut plans = Vec::new();

    for path in markdown_note_paths(paths)? {
        let source = fs::read_to_string(paths.vault_root().join(&path))?;
        let parsed = parse_document(&source, &config);
        let mut edits = Vec::new();
        let mut changes = Vec::new();

        // Try surgical tag edit (only rewrites the `tags:` block, preserves everything else).
        let surgical_result = plan_surgical_frontmatter_edit(&source, |yaml| {
            surgical_edit_key_value_block(yaml, "tags", |tags_value| {
                merge_frontmatter_tags_value(tags_value, &source_tag, &destination_tag)
            })
        });

        let fm_result = if surgical_result.is_some() {
            surgical_result
        } else {
            plan_frontmatter_replacement(&source, |frontmatter| {
                Ok(merge_frontmatter_tags(
                    frontmatter,
                    &source_tag,
                    &destination_tag,
                ))
            })?
        };

        if let Some((edit, edit_changes)) = fm_result {
            edits.push(edit);
            changes.extend(edit_changes);
        }

        for tag in parsed
            .tags
            .iter()
            .filter(|tag| tag.byte_offset > 0 && normalize_tag_name(&tag.tag_text) == source_tag)
        {
            let before = format!("#{}", tag.tag_text);
            let after = format!("#{destination_tag}");
            edits.push(TextEdit {
                start: tag.byte_offset,
                end: tag.byte_offset + before.len(),
                replacement: after.clone(),
            });
            changes.push(RefactorChange { before, after });
        }

        if let Some(plan) = build_file_plan(&path, &source, &edits, changes) {
            plans.push(plan);
        }
    }

    finalize_refactor(paths, dry_run, "merge_tags", plans)
}

pub fn rename_alias(
    paths: &VaultPaths,
    note_identifier: &str,
    old_alias: &str,
    new_alias: &str,
    dry_run: bool,
) -> Result<RefactorReport, RefactorError> {
    let _lock = acquire_write_lock(paths)?;
    let note = resolve_note_reference(paths, note_identifier)?;
    let path = note.path;
    let source = fs::read_to_string(paths.vault_root().join(&path))?;
    let mut edits = Vec::new();
    let mut changes = Vec::new();

    // Try surgical alias value edit first; fall back to full round-trip.
    let surgical = plan_surgical_frontmatter_edit(&source, |yaml| {
        surgical_edit_key_value_block(yaml, "aliases", |aliases_value| {
            rename_frontmatter_alias_value(aliases_value, &path, old_alias, new_alias)
                .unwrap_or_default()
        })
    });

    let alias_result = if surgical.is_some() {
        surgical
    } else {
        plan_frontmatter_replacement(&source, |frontmatter| {
            rename_frontmatter_alias(frontmatter, &path, old_alias, new_alias)
        })?
    };

    if let Some((edit, edit_changes)) = alias_result {
        edits.push(edit);
        changes.extend(edit_changes);
    }

    let Some(plan) = build_file_plan(&path, &source, &edits, changes) else {
        return Err(RefactorError::MissingTarget {
            path,
            kind: "alias",
            value: old_alias.to_string(),
        });
    };

    finalize_refactor(paths, dry_run, "rename_alias", vec![plan])
}

pub fn set_note_property(
    paths: &VaultPaths,
    note_identifier: &str,
    key: &str,
    value: Option<&str>,
    dry_run: bool,
) -> Result<RefactorReport, RefactorError> {
    let _lock = acquire_write_lock(paths)?;
    let note = resolve_note_reference(paths, note_identifier)?;
    let path = note.path;
    let source = fs::read_to_string(paths.vault_root().join(&path))?;
    let desired_value = parse_property_value(value)?;

    let Some((edit, changes)) =
        plan_set_note_property_replacement(&source, &path, key, desired_value.as_ref())?
    else {
        return finalize_refactor(paths, dry_run, "set_note_property", Vec::new());
    };

    let Some(plan) = build_file_plan(&path, &source, &[edit], changes) else {
        return finalize_refactor(paths, dry_run, "set_note_property", Vec::new());
    };

    finalize_refactor(paths, dry_run, "set_note_property", vec![plan])
}

/// Apply a property set or unset to all notes that match the given query filters.
///
/// - `key`: the frontmatter property key to write
/// - `value`: the new value as a YAML-compatible string; `None` removes the property
/// - `dry_run`: if true, compute and return planned changes without writing files
///
/// Acquires the write lock once and runs one incremental reindex after all edits.
pub fn bulk_set_property(
    paths: &VaultPaths,
    filters: &[String],
    key: &str,
    value: Option<&str>,
    dry_run: bool,
) -> Result<BulkMutationReport, RefactorError> {
    let _lock = acquire_write_lock(paths)?;

    let matching_paths = query_matching_paths(paths, filters)?;
    let desired_value = parse_property_value(value)?;
    let mut plans = Vec::new();

    for path in &matching_paths {
        let source = fs::read_to_string(paths.vault_root().join(path))?;
        let Some((edit, changes)) =
            plan_set_note_property_replacement(&source, path, key, desired_value.as_ref())?
        else {
            continue;
        };
        if let Some(plan) = build_file_plan(path, &source, &[edit], changes) {
            plans.push(plan);
        }
    }

    let action = if value.is_some() {
        "bulk_update"
    } else {
        "bulk_unset"
    };
    let inner = finalize_refactor(paths, dry_run, action, plans)?;
    Ok(BulkMutationReport {
        dry_run,
        action: inner.action.clone(),
        filters: filters.to_vec(),
        key: key.to_string(),
        value: value.map(str::to_string),
        files: inner.files,
    })
}

pub fn bulk_set_property_on_paths(
    paths: &VaultPaths,
    note_paths: &[String],
    key: &str,
    value: Option<&str>,
    dry_run: bool,
) -> Result<BulkMutationReport, RefactorError> {
    let _lock = acquire_write_lock(paths)?;

    let desired_value = parse_property_value(value)?;
    let mut plans = Vec::new();

    for path in note_paths {
        let source = fs::read_to_string(paths.vault_root().join(path))?;
        let Some((edit, changes)) =
            plan_set_note_property_replacement(&source, path, key, desired_value.as_ref())?
        else {
            continue;
        };
        if let Some(plan) = build_file_plan(path, &source, &[edit], changes) {
            plans.push(plan);
        }
    }

    let action = if value.is_some() {
        "bulk_update"
    } else {
        "bulk_unset"
    };
    let inner = finalize_refactor(paths, dry_run, action, plans)?;
    Ok(BulkMutationReport {
        dry_run,
        action: inner.action.clone(),
        filters: Vec::new(),
        key: key.to_string(),
        value: value.map(str::to_string),
        files: inner.files,
    })
}

/// Fetch the vault-relative paths of all notes matching the given `--where` filters.
fn query_matching_paths(
    paths: &VaultPaths,
    filters: &[String],
) -> Result<Vec<String>, RefactorError> {
    use crate::properties::{query_notes, NoteQuery, PropertyError};

    let report = query_notes(
        paths,
        &NoteQuery {
            filters: filters.to_vec(),
            sort_by: None,
            sort_descending: false,
        },
    )
    .map_err(|e| match e {
        PropertyError::CacheMissing => RefactorError::Io(std::io::Error::other(
            "cache is missing; run `vulcan scan` first",
        )),
        other => RefactorError::Io(std::io::Error::other(other.to_string())),
    })?;
    Ok(report.notes.into_iter().map(|n| n.document_path).collect())
}

/// Result of a query-driven bulk property mutation.
#[derive(Debug, Clone, Serialize)]
pub struct BulkMutationReport {
    pub dry_run: bool,
    pub action: String,
    /// Filters that selected the affected notes.
    pub filters: Vec<String>,
    /// The property key that was set or unset.
    pub key: String,
    /// The new value (`None` means the property was removed).
    pub value: Option<String>,
    /// Per-file change details.
    pub files: Vec<crate::refactor::RefactorFileReport>,
}

pub fn rename_heading(
    paths: &VaultPaths,
    note_identifier: &str,
    old_heading: &str,
    new_heading: &str,
    dry_run: bool,
) -> Result<RefactorReport, RefactorError> {
    let _lock = acquire_write_lock(paths)?;
    let config = load_vault_config(paths).config;
    let note = resolve_note_reference(paths, note_identifier)?;
    let note_source = fs::read_to_string(paths.vault_root().join(&note.path))?;
    let parsed = parse_document(&note_source, &config);
    let heading = find_unique_heading(&parsed.headings, &note.path, old_heading)?;
    let mut edits_by_file = BTreeMap::<String, Vec<TextEdit>>::new();
    let mut changes_by_file = BTreeMap::<String, Vec<RefactorChange>>::new();

    let (heading_edit, heading_change) = plan_heading_edit(&note_source, heading, new_heading);
    edits_by_file
        .entry(note.path.clone())
        .or_default()
        .push(heading_edit);
    changes_by_file
        .entry(note.path.clone())
        .or_default()
        .push(heading_change);

    let connection = open_existing_cache(paths)?;
    let inbound_links = load_subpath_links(&connection, &note.id, "heading", old_heading)?;
    plan_subpath_link_rewrites(
        paths,
        &config,
        inbound_links,
        Some(new_heading),
        None,
        &mut edits_by_file,
        &mut changes_by_file,
    )?;

    let plans = build_plans_from_edits(paths, edits_by_file, changes_by_file)?;
    finalize_refactor(paths, dry_run, "rename_heading", plans)
}

pub fn rename_block_ref(
    paths: &VaultPaths,
    note_identifier: &str,
    old_block_ref: &str,
    new_block_ref: &str,
    dry_run: bool,
) -> Result<RefactorReport, RefactorError> {
    let _lock = acquire_write_lock(paths)?;
    let config = load_vault_config(paths).config;
    let note = resolve_note_reference(paths, note_identifier)?;
    let note_source = fs::read_to_string(paths.vault_root().join(&note.path))?;
    let parsed = parse_document(&note_source, &config);
    let block_ref = find_unique_block_ref(&parsed.block_refs, &note.path, old_block_ref)?;
    let mut edits_by_file = BTreeMap::<String, Vec<TextEdit>>::new();
    let mut changes_by_file = BTreeMap::<String, Vec<RefactorChange>>::new();

    let (block_edit, block_change) = plan_block_ref_edit(&note_source, block_ref, new_block_ref);
    edits_by_file
        .entry(note.path.clone())
        .or_default()
        .push(block_edit);
    changes_by_file
        .entry(note.path.clone())
        .or_default()
        .push(block_change);

    let connection = open_existing_cache(paths)?;
    let inbound_links = load_subpath_links(&connection, &note.id, "block", old_block_ref)?;
    plan_subpath_link_rewrites(
        paths,
        &config,
        inbound_links,
        None,
        Some(new_block_ref),
        &mut edits_by_file,
        &mut changes_by_file,
    )?;

    let plans = build_plans_from_edits(paths, edits_by_file, changes_by_file)?;
    finalize_refactor(paths, dry_run, "rename_block_ref", plans)
}

fn finalize_refactor(
    paths: &VaultPaths,
    dry_run: bool,
    action: &str,
    plans: Vec<FilePlan>,
) -> Result<RefactorReport, RefactorError> {
    if !dry_run {
        for plan in &plans {
            fs::write(paths.vault_root().join(&plan.path), &plan.updated_contents)?;
        }
        if !plans.is_empty() {
            scan_vault_unlocked(paths, ScanMode::Incremental)?;
        }
    }

    Ok(RefactorReport {
        dry_run,
        action: action.to_string(),
        files: plans
            .into_iter()
            .map(|plan| RefactorFileReport {
                path: plan.path,
                changes: plan.changes,
            })
            .collect(),
    })
}

fn markdown_note_paths(paths: &VaultPaths) -> Result<Vec<String>, RefactorError> {
    Ok(discover_relative_paths(paths.vault_root())?
        .into_iter()
        .filter(|path| {
            std::path::Path::new(path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        })
        .collect())
}

fn plan_frontmatter_replacement<F>(
    source: &str,
    mutate: F,
) -> Result<Option<(TextEdit, Vec<RefactorChange>)>, RefactorError>
where
    F: FnOnce(&mut YamlValue) -> Result<Vec<RefactorChange>, RefactorError>,
{
    let Some(block) = find_frontmatter_block(source) else {
        return Ok(None);
    };
    let raw_yaml = &source[block.yaml_start..block.yaml_end];
    let mut frontmatter = serde_yaml::from_str::<YamlValue>(raw_yaml)?;
    let changes = mutate(&mut frontmatter)?;
    if changes.is_empty() {
        return Ok(None);
    }

    let replacement = format_frontmatter_block(&frontmatter)?;
    if replacement == source[block.full_start..block.full_end] {
        return Ok(None);
    }

    Ok(Some((
        TextEdit {
            start: block.full_start,
            end: block.full_end,
            replacement,
        },
        changes,
    )))
}

fn plan_set_note_property_replacement(
    source: &str,
    path: &str,
    key: &str,
    value: Option<&YamlValue>,
) -> Result<Option<(TextEdit, Vec<RefactorChange>)>, RefactorError> {
    if let Some(block) = find_frontmatter_block(source) {
        let raw_yaml = &source[block.yaml_start..block.yaml_end];

        // Attempt surgical set/remove first: only touches the target key.
        let surgical = try_surgical_set_in_yaml(block, raw_yaml, source, key, value);
        if surgical.is_some() {
            return Ok(surgical);
        }

        // Fall back to full round-trip (handles complex types, new-key append, etc.).
        let mut frontmatter = serde_yaml::from_str::<YamlValue>(raw_yaml)?;
        let changes = set_frontmatter_property(&mut frontmatter, path, key, value)?;
        if changes.is_empty() {
            return Ok(None);
        }

        let replacement = format_frontmatter_block_or_remove(&frontmatter)?;
        return Ok(Some((
            TextEdit {
                start: block.full_start,
                end: block.full_end,
                replacement,
            },
            changes,
        )));
    }

    let Some(value) = value else {
        return Ok(None);
    };

    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(YamlValue::String(key.to_string()), value.clone());
    let replacement = format_frontmatter_block(&YamlValue::Mapping(mapping))?;
    Ok(Some((
        TextEdit {
            start: 0,
            end: 0,
            replacement,
        },
        vec![RefactorChange {
            before: format!("{key}: <missing>"),
            after: format!("{key}: {}", summarize_yaml_value(value)),
        }],
    )))
}

/// Try a surgical set/remove within a frontmatter block's raw YAML.
///
/// Returns `None` (fall back to full round-trip) if:
/// - the key is not present in the YAML,
/// - the previous value can't be deserialized for change tracking, or
/// - `surgical_set_key` / `format_yaml_kv_block` cannot handle the value type.
fn try_surgical_set_in_yaml(
    block: FrontmatterBlock,
    raw_yaml: &str,
    source: &str,
    key: &str,
    value: Option<&YamlValue>,
) -> Option<(TextEdit, Vec<RefactorChange>)> {
    // We need the previous value for the change record.
    let (key_start, _key_end) = find_yaml_key_span(raw_yaml, key)?;
    let key_block = &raw_yaml[key_start..];
    let prev_mapping: YamlValue = serde_yaml::from_str(key_block).ok()?;
    let prev_value = prev_mapping
        .as_mapping()?
        .get(YamlValue::String(key.to_string()))?
        .clone();

    if let Some(new_value) = value {
        if prev_value == *new_value {
            return None; // No change needed; fall through to full round-trip (will also no-op).
        }
    }

    let new_yaml = surgical_set_key(raw_yaml, key, value)?;
    let new_frontmatter = if new_yaml.trim().is_empty() {
        String::new() // whole block removed
    } else {
        format!("---\n{new_yaml}---\n")
    };

    if new_frontmatter == source[block.full_start..block.full_end] {
        return None;
    }

    let change = match value {
        None => RefactorChange {
            before: format!("{key}: {}", summarize_yaml_value(&prev_value)),
            after: format!("{key}: <removed>"),
        },
        Some(v) => RefactorChange {
            before: format!("{key}: {}", summarize_yaml_value(&prev_value)),
            after: format!("{key}: {}", summarize_yaml_value(v)),
        },
    };

    Some((
        TextEdit {
            start: block.full_start,
            end: block.full_end,
            replacement: new_frontmatter,
        },
        vec![change],
    ))
}

fn rename_frontmatter_property(
    frontmatter: &mut YamlValue,
    path: &str,
    old_key: &str,
    new_key: &str,
) -> Result<Vec<RefactorChange>, RefactorError> {
    if old_key == new_key {
        return Ok(Vec::new());
    }

    let Some(mapping) = frontmatter.as_mapping_mut() else {
        return Ok(Vec::new());
    };
    let old_key_value = YamlValue::String(old_key.to_string());
    let new_key_value = YamlValue::String(new_key.to_string());
    if !mapping.contains_key(&old_key_value) {
        return Ok(Vec::new());
    }
    if mapping.contains_key(&new_key_value) {
        return Err(RefactorError::DuplicateTarget {
            path: path.to_string(),
            kind: "property",
            value: new_key.to_string(),
        });
    }

    let entries = mapping
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    mapping.clear();
    for (key, value) in entries {
        if key.as_str() == Some(old_key) {
            mapping.insert(new_key_value.clone(), value);
        } else {
            mapping.insert(key, value);
        }
    }

    Ok(vec![RefactorChange {
        before: old_key.to_string(),
        after: new_key.to_string(),
    }])
}

/// Merge tags directly within the `tags` *value* (not the whole frontmatter mapping).
/// Used by the surgical path; the full-round-trip path uses `merge_frontmatter_tags`.
fn merge_frontmatter_tags_value(
    tags: &mut YamlValue,
    source_tag: &str,
    destination_tag: &str,
) -> Vec<RefactorChange> {
    let mut changes = Vec::new();
    match tags {
        YamlValue::String(text) => {
            let uses_hash_prefix = split_tag_tokens(text)
                .iter()
                .any(|value| value.starts_with('#'));
            let updated = split_tag_tokens(text)
                .into_iter()
                .map(|value| {
                    let normalized = normalize_tag_name(value);
                    if normalized == source_tag {
                        changes.push(RefactorChange {
                            before: value.to_string(),
                            after: if uses_hash_prefix {
                                format!("#{destination_tag}")
                            } else {
                                destination_tag.to_string()
                            },
                        });
                        if uses_hash_prefix {
                            format!("#{destination_tag}")
                        } else {
                            destination_tag.to_string()
                        }
                    } else {
                        value.to_string()
                    }
                })
                .collect::<Vec<_>>();
            if !changes.is_empty() {
                *text = if text.contains(',') {
                    updated.join(", ")
                } else {
                    updated.join(" ")
                };
            }
        }
        YamlValue::Sequence(values) => {
            for value in values.iter_mut() {
                let Some(text) = value.as_str() else {
                    continue;
                };
                if normalize_tag_name(text) == source_tag {
                    changes.push(RefactorChange {
                        before: text.to_string(),
                        after: destination_tag.to_string(),
                    });
                    *value = YamlValue::String(destination_tag.to_string());
                }
            }
        }
        _ => {}
    }
    changes
}

fn merge_frontmatter_tags(
    frontmatter: &mut YamlValue,
    source_tag: &str,
    destination_tag: &str,
) -> Vec<RefactorChange> {
    let Some(mapping) = frontmatter.as_mapping_mut() else {
        return Vec::new();
    };
    let Some(tags) = mapping.get_mut(YamlValue::String("tags".to_string())) else {
        return Vec::new();
    };

    merge_frontmatter_tags_value(tags, source_tag, destination_tag)
}

/// Rename an alias within the `aliases` *value* (not the whole frontmatter mapping).
/// Used by the surgical path.  Returns `Err` for duplicates so the surgical path can abort.
fn rename_frontmatter_alias_value(
    aliases: &mut YamlValue,
    path: &str,
    old_alias: &str,
    new_alias: &str,
) -> Result<Vec<RefactorChange>, RefactorError> {
    let mut changes = Vec::new();
    match aliases {
        YamlValue::String(text) => {
            if text == new_alias && old_alias != new_alias {
                return Err(RefactorError::DuplicateTarget {
                    path: path.to_string(),
                    kind: "alias",
                    value: new_alias.to_string(),
                });
            }
            if text == old_alias {
                changes.push(RefactorChange {
                    before: text.clone(),
                    after: new_alias.to_string(),
                });
                *text = new_alias.to_string();
            }
        }
        YamlValue::Sequence(values) => {
            if old_alias != new_alias
                && values
                    .iter()
                    .any(|value| value.as_str().is_some_and(|alias| alias == new_alias))
            {
                return Err(RefactorError::DuplicateTarget {
                    path: path.to_string(),
                    kind: "alias",
                    value: new_alias.to_string(),
                });
            }
            for value in values.iter_mut() {
                let Some(alias) = value.as_str() else {
                    continue;
                };
                if alias == old_alias {
                    changes.push(RefactorChange {
                        before: alias.to_string(),
                        after: new_alias.to_string(),
                    });
                    *value = YamlValue::String(new_alias.to_string());
                }
            }
        }
        _ => {}
    }
    Ok(changes)
}

fn rename_frontmatter_alias(
    frontmatter: &mut YamlValue,
    path: &str,
    old_alias: &str,
    new_alias: &str,
) -> Result<Vec<RefactorChange>, RefactorError> {
    let Some(mapping) = frontmatter.as_mapping_mut() else {
        return Ok(Vec::new());
    };
    let Some(aliases) = mapping.get_mut(YamlValue::String("aliases".to_string())) else {
        return Ok(Vec::new());
    };
    rename_frontmatter_alias_value(aliases, path, old_alias, new_alias)
}

fn set_frontmatter_property(
    frontmatter: &mut YamlValue,
    path: &str,
    key: &str,
    value: Option<&YamlValue>,
) -> Result<Vec<RefactorChange>, RefactorError> {
    let Some(mapping) = frontmatter.as_mapping_mut() else {
        return Err(RefactorError::InvalidFrontmatterRoot {
            path: path.to_string(),
        });
    };

    let key_value = YamlValue::String(key.to_string());
    let previous = mapping.get(&key_value).cloned();
    if let Some(value) = value {
        if previous.as_ref() == Some(value) {
            return Ok(Vec::new());
        }
        mapping.insert(key_value, value.clone());
        Ok(vec![RefactorChange {
            before: previous.as_ref().map_or_else(
                || format!("{key}: <missing>"),
                |existing| format!("{key}: {}", summarize_yaml_value(existing)),
            ),
            after: format!("{key}: {}", summarize_yaml_value(value)),
        }])
    } else {
        let Some(previous) = previous else {
            return Ok(Vec::new());
        };
        mapping.remove(YamlValue::String(key.to_string()));
        Ok(vec![RefactorChange {
            before: format!("{key}: {}", summarize_yaml_value(&previous)),
            after: format!("{key}: <removed>"),
        }])
    }
}

fn find_unique_heading<'a>(
    headings: &'a [RawHeading],
    path: &str,
    heading: &str,
) -> Result<&'a RawHeading, RefactorError> {
    let matches = headings
        .iter()
        .filter(|candidate| candidate.text == heading)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(RefactorError::MissingTarget {
            path: path.to_string(),
            kind: "heading",
            value: heading.to_string(),
        }),
        [single] => Ok(*single),
        _ => Err(RefactorError::AmbiguousTarget {
            path: path.to_string(),
            kind: "heading",
            value: heading.to_string(),
        }),
    }
}

fn find_unique_block_ref<'a>(
    block_refs: &'a [RawBlockRef],
    path: &str,
    block_ref: &str,
) -> Result<&'a RawBlockRef, RefactorError> {
    let matches = block_refs
        .iter()
        .filter(|candidate| candidate.block_id_text == block_ref)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(RefactorError::MissingTarget {
            path: path.to_string(),
            kind: "block ref",
            value: block_ref.to_string(),
        }),
        [single] => Ok(*single),
        _ => Err(RefactorError::AmbiguousTarget {
            path: path.to_string(),
            kind: "block ref",
            value: block_ref.to_string(),
        }),
    }
}

fn plan_heading_edit(
    source: &str,
    heading: &RawHeading,
    new_heading: &str,
) -> (TextEdit, RefactorChange) {
    let line_end = line_end(source, heading.byte_offset);
    let line = &source[heading.byte_offset..line_end];
    let prefix_end = line
        .char_indices()
        .find_map(|(index, character)| (!matches!(character, '#' | ' ' | '\t')).then_some(index))
        .unwrap_or(line.len());
    let replacement = format!("{}{new_heading}", &line[..prefix_end]);

    (
        TextEdit {
            start: heading.byte_offset,
            end: line_end,
            replacement: replacement.clone(),
        },
        RefactorChange {
            before: line.to_string(),
            after: replacement,
        },
    )
}

fn plan_block_ref_edit(
    source: &str,
    block_ref: &RawBlockRef,
    new_block_ref: &str,
) -> (TextEdit, RefactorChange) {
    let line_end = line_end(source, block_ref.block_id_byte_offset);
    let line = &source[block_ref.block_id_byte_offset..line_end];
    let replacement = format!("^{new_block_ref}");

    (
        TextEdit {
            start: block_ref.block_id_byte_offset,
            end: line_end,
            replacement: replacement.clone(),
        },
        RefactorChange {
            before: line.to_string(),
            after: replacement,
        },
    )
}

fn open_existing_cache(paths: &VaultPaths) -> Result<Connection, RefactorError> {
    if !paths.cache_db().exists() {
        return Err(RefactorError::Graph(GraphQueryError::CacheMissing));
    }

    Ok(Connection::open(paths.cache_db())?)
}

fn load_subpath_links(
    connection: &Connection,
    document_id: &str,
    subpath_kind: &str,
    value: &str,
) -> Result<Vec<CachedSubpathLink>, RefactorError> {
    let sql = match subpath_kind {
        "heading" => {
            "
            SELECT source.path, links.raw_text, links.byte_offset
            FROM links
            JOIN documents AS source ON source.id = links.source_document_id
            WHERE links.resolved_target_id = ?1 AND links.target_heading = ?2
            ORDER BY source.path, links.byte_offset
            "
        }
        "block" => {
            "
            SELECT source.path, links.raw_text, links.byte_offset
            FROM links
            JOIN documents AS source ON source.id = links.source_document_id
            WHERE links.resolved_target_id = ?1 AND links.target_block = ?2
            ORDER BY source.path, links.byte_offset
            "
        }
        _ => unreachable!(),
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params![document_id, value], |row| {
        Ok(CachedSubpathLink {
            source_path: row.get(0)?,
            raw_text: row.get(1)?,
            byte_offset: row.get(2)?,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(RefactorError::from)
}

fn plan_subpath_link_rewrites(
    paths: &VaultPaths,
    config: &crate::VaultConfig,
    inbound_links: Vec<CachedSubpathLink>,
    new_heading: Option<&str>,
    new_block_ref: Option<&str>,
    edits_by_file: &mut BTreeMap<String, Vec<TextEdit>>,
    changes_by_file: &mut BTreeMap<String, Vec<RefactorChange>>,
) -> Result<(), RefactorError> {
    let mut inbound_by_file = BTreeMap::<String, Vec<CachedSubpathLink>>::new();
    for link in inbound_links {
        inbound_by_file
            .entry(link.source_path.clone())
            .or_default()
            .push(link);
    }

    for (path, links) in inbound_by_file {
        let source = fs::read_to_string(paths.vault_root().join(&path))?;
        let parsed = parse_document(&source, config);

        for link in links {
            let raw_link = parsed
                .links
                .iter()
                .find(|candidate| {
                    candidate.byte_offset == link.byte_offset && candidate.raw_text == link.raw_text
                })
                .ok_or_else(|| RefactorError::MissingLinkSpan {
                    path: path.clone(),
                    byte_offset: link.byte_offset,
                })?;
            let replacement = rewrite_link_subpath(raw_link, new_heading, new_block_ref);
            if replacement == raw_link.raw_text {
                continue;
            }

            edits_by_file
                .entry(path.clone())
                .or_default()
                .push(TextEdit {
                    start: raw_link.byte_offset,
                    end: raw_link.byte_offset + raw_link.raw_text.len(),
                    replacement: replacement.clone(),
                });
            changes_by_file
                .entry(path.clone())
                .or_default()
                .push(RefactorChange {
                    before: raw_link.raw_text.clone(),
                    after: replacement,
                });
        }
    }

    Ok(())
}

fn build_plans_from_edits(
    paths: &VaultPaths,
    edits_by_file: BTreeMap<String, Vec<TextEdit>>,
    mut changes_by_file: BTreeMap<String, Vec<RefactorChange>>,
) -> Result<Vec<FilePlan>, RefactorError> {
    let mut plans = Vec::new();
    for (path, edits) in edits_by_file {
        let source = fs::read_to_string(paths.vault_root().join(&path))?;
        let changes = changes_by_file.remove(&path).unwrap_or_default();
        if let Some(plan) = build_file_plan(&path, &source, &edits, changes) {
            plans.push(plan);
        }
    }
    Ok(plans)
}

fn build_file_plan(
    path: &str,
    source: &str,
    edits: &[TextEdit],
    changes: Vec<RefactorChange>,
) -> Option<FilePlan> {
    if edits.is_empty() || changes.is_empty() {
        return None;
    }
    let updated_contents = apply_edits(source, edits);
    if updated_contents == source {
        return None;
    }

    Some(FilePlan {
        path: path.to_string(),
        updated_contents,
        changes,
    })
}

fn find_frontmatter_block(source: &str) -> Option<FrontmatterBlock> {
    let mut lines = source.split_inclusive('\n');
    let first_line = lines.next()?;
    if trim_line(first_line) != "---" {
        return None;
    }

    let yaml_start = first_line.len();
    let mut offset = yaml_start;
    for line in lines {
        if trim_line(line) == "---" {
            return Some(FrontmatterBlock {
                full_start: 0,
                full_end: offset + line.len(),
                yaml_start,
                yaml_end: offset,
            });
        }
        offset += line.len();
    }

    None
}

fn format_frontmatter_block(frontmatter: &YamlValue) -> Result<String, RefactorError> {
    let mut yaml = serde_yaml::to_string(frontmatter)?;
    if let Some(stripped) = yaml.strip_prefix("---\n") {
        yaml = stripped.to_string();
    }
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }
    Ok(format!("---\n{yaml}---\n"))
}

fn format_frontmatter_block_or_remove(frontmatter: &YamlValue) -> Result<String, RefactorError> {
    if frontmatter
        .as_mapping()
        .is_some_and(serde_yaml::Mapping::is_empty)
    {
        Ok(String::new())
    } else {
        format_frontmatter_block(frontmatter)
    }
}

// === Surgical YAML frontmatter editing ===
//
// These functions edit only the targeted key within a frontmatter block, preserving
// all other keys byte-for-byte (ordering, comments, quoting style, list indentation).

/// Returns true if `line` (possibly with a trailing newline) is a top-level YAML mapping entry
/// for `key` — i.e., it begins exactly with `key:` followed by a space, tab, newline, or nothing.
fn is_top_level_key_line(line: &str, key: &str) -> bool {
    let Some(rest) = line.strip_prefix(key) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(':') else {
        return false;
    };
    rest.is_empty()
        || rest.starts_with(' ')
        || rest.starts_with('\t')
        || rest == "\n"
        || rest == "\r\n"
}

/// Find the byte range `[start, end)` of a top-level key's block in raw YAML frontmatter.
///
/// The span covers the key line plus all continuation lines (indented lines and bare
/// sequence entries `- item` at column 0).  Blank lines and new top-level keys end the span.
/// Returns `None` if the key is not present.
fn find_yaml_key_span(yaml: &str, key: &str) -> Option<(usize, usize)> {
    let mut pos = 0usize;
    let mut span_start: Option<usize> = None;

    for line in yaml.split_inclusive('\n') {
        let line_start = pos;
        pos += line.len();
        let line_body = line.trim_end_matches('\n').trim_end_matches('\r');

        if let Some(start) = span_start {
            if line_body.is_empty() {
                return Some((start, line_start));
            }
            let first_char = line.chars().next().unwrap_or(' ');
            if first_char == ' ' || first_char == '\t' {
                // Indented continuation.
            } else if line_body.starts_with("- ") || line_body == "-" {
                // Bare sequence entry at column 0.
            } else {
                // New top-level key, comment, or end of frontmatter.
                return Some((start, line_start));
            }
        } else if is_top_level_key_line(line_body, key) {
            span_start = Some(line_start);
            let after_colon = line_body[key.len() + 1..].trim();
            if !after_colon.is_empty() {
                // Inline scalar value: span is this single line.
                return Some((line_start, pos));
            }
            // Block value: accumulate continuation lines below.
        }
    }

    span_start.map(|start| (start, pos))
}

/// Rename a top-level key in raw YAML frontmatter, preserving all other formatting byte-for-byte.
/// Returns `None` if the key was not found.
fn surgical_rename_key(yaml: &str, old_key: &str, new_key: &str) -> Option<String> {
    let mut result = String::with_capacity(yaml.len() + new_key.len());
    let mut found = false;

    for line in yaml.split_inclusive('\n') {
        if !found {
            let line_body = line.trim_end_matches('\n').trim_end_matches('\r');
            if is_top_level_key_line(line_body, old_key) {
                // Replace just the key name; keep the colon and everything after.
                let after_key = &line[old_key.len()..];
                result.push_str(new_key);
                result.push_str(after_key);
                found = true;
                continue;
            }
        }
        result.push_str(line);
    }

    found.then_some(result)
}

/// Serialize a YAML value as a compact inline form suitable for `key: <result>`.
/// Returns `None` for complex types (nested mappings) that need full serialization.
fn format_scalar_yaml_value(value: &YamlValue) -> Option<String> {
    match value {
        YamlValue::Null => Some("null".to_string()),
        YamlValue::Bool(b) => Some(b.to_string()),
        YamlValue::Number(_) | YamlValue::String(_) => serde_yaml::to_string(value)
            .ok()
            .map(|s| s.trim_end().to_string()),
        _ => None,
    }
}

/// Format a key-value block for insertion into raw frontmatter YAML.
/// Returns `None` for nested mapping values (caller should fall back to full round-trip).
fn format_yaml_kv_block(key: &str, value: &YamlValue) -> Option<String> {
    use std::fmt::Write as _;
    match value {
        YamlValue::Null | YamlValue::Bool(_) | YamlValue::Number(_) | YamlValue::String(_) => {
            let v = format_scalar_yaml_value(value)?;
            Some(format!("{key}: {v}\n"))
        }
        YamlValue::Sequence(seq) => {
            if seq.is_empty() {
                return Some(format!("{key}: []\n"));
            }
            let mut block = format!("{key}:\n");
            for item in seq {
                let item_str = format_scalar_yaml_value(item).or_else(|| {
                    serde_yaml::to_string(item)
                        .ok()
                        .map(|s| s.trim_end().to_string())
                })?;
                writeln!(block, "- {item_str}").ok();
            }
            Some(block)
        }
        _ => None,
    }
}

/// Surgically set or remove a key in raw YAML frontmatter.
/// Preserves all other keys byte-for-byte.  Returns `None` when:
/// - the key is not present (for removal),
/// - the key is not present and the value is being set (caller should append),
/// - or the value type is too complex for surgical formatting.
fn surgical_set_key(yaml: &str, key: &str, value: Option<&YamlValue>) -> Option<String> {
    let (span_start, span_end) = find_yaml_key_span(yaml, key)?;
    let before = &yaml[..span_start];
    let after = &yaml[span_end..];

    match value {
        None => Some(format!("{before}{after}")),
        Some(v) => {
            let new_block = format_yaml_kv_block(key, v)?;
            Some(format!("{before}{new_block}{after}"))
        }
    }
}

/// Apply a surgical edit to a specific property key's value block in raw frontmatter YAML.
///
/// `edit_fn` receives a mutable reference to the deserialized *value* of `key_property` and
/// should return the `RefactorChange` list.  The value is re-formatted surgically and only
/// that block is replaced; all other keys are preserved byte-for-byte.
///
/// Returns `None` (fall back to full round-trip) if:
/// - the key is absent,
/// - YAML parsing fails,
/// - or `format_yaml_kv_block` cannot handle the resulting value type.
fn surgical_edit_key_value_block<F>(
    yaml: &str,
    key_property: &str,
    edit_fn: F,
) -> Option<(String, Vec<RefactorChange>)>
where
    F: FnOnce(&mut YamlValue) -> Vec<RefactorChange>,
{
    let (key_start, key_end) = find_yaml_key_span(yaml, key_property)?;
    let key_block = &yaml[key_start..key_end];

    // Parse just this key's block as a mini YAML mapping.
    let mut mini: YamlValue = serde_yaml::from_str(key_block).ok()?;
    let key_yaml = YamlValue::String(key_property.to_string());
    let value = mini.as_mapping_mut()?.get_mut(&key_yaml)?;

    let changes = edit_fn(value);
    if changes.is_empty() {
        return None;
    }

    let new_block = format_yaml_kv_block(key_property, value)?;
    let new_yaml = format!("{}{}{}", &yaml[..key_start], new_block, &yaml[key_end..]);
    Some((new_yaml, changes))
}

/// Generic surgical frontmatter edit driver.
///
/// Calls `edit_fn` with the raw YAML string extracted from the frontmatter block.
/// `edit_fn` should return `(new_yaml, changes)` or `None` to abort the surgical path.
/// On success returns a `TextEdit` spanning the full frontmatter block and the change list.
fn plan_surgical_frontmatter_edit<F>(
    source: &str,
    edit_fn: F,
) -> Option<(TextEdit, Vec<RefactorChange>)>
where
    F: FnOnce(&str) -> Option<(String, Vec<RefactorChange>)>,
{
    let block = find_frontmatter_block(source)?;
    let raw_yaml = &source[block.yaml_start..block.yaml_end];

    let (new_yaml, changes) = edit_fn(raw_yaml)?;
    if changes.is_empty() {
        return None;
    }

    let new_frontmatter = format!("---\n{new_yaml}---\n");
    if new_frontmatter == source[block.full_start..block.full_end] {
        return None;
    }

    Some((
        TextEdit {
            start: block.full_start,
            end: block.full_end,
            replacement: new_frontmatter,
        },
        changes,
    ))
}

fn parse_property_value(value: Option<&str>) -> Result<Option<YamlValue>, RefactorError> {
    let Some(value) = value.map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_yaml::from_str::<YamlValue>(value)?))
}

fn summarize_yaml_value(value: &YamlValue) -> String {
    let rendered = serde_yaml::to_string(value).unwrap_or_else(|_| format!("{value:?}"));
    rendered.trim().replace('\n', " ")
}

fn rewrite_link_subpath(
    link: &RawLink,
    new_heading: Option<&str>,
    new_block_ref: Option<&str>,
) -> String {
    let target_path = link.target_path_candidate.as_deref().unwrap_or("");
    let suffix = if let Some(heading) = new_heading {
        format!("#{heading}")
    } else if let Some(block_ref) = new_block_ref {
        format!("#^{block_ref}")
    } else {
        String::new()
    };
    let target = format!("{target_path}{suffix}");

    if link.raw_text.starts_with("![[") {
        if let Some(display_text) = link.display_text.as_deref() {
            format!("![[{target}|{display_text}]]")
        } else {
            format!("![[{target}]]")
        }
    } else if link.raw_text.starts_with("[[") {
        if let Some(display_text) = link.display_text.as_deref() {
            format!("[[{target}|{display_text}]]")
        } else {
            format!("[[{target}]]")
        }
    } else if link.raw_text.starts_with("![") {
        let label = link
            .display_text
            .clone()
            .unwrap_or_else(|| extract_markdown_label(&link.raw_text));
        format!("![{label}]({target})")
    } else if link.raw_text.starts_with('[') {
        let label = link
            .display_text
            .clone()
            .unwrap_or_else(|| extract_markdown_label(&link.raw_text));
        format!("[{label}]({target})")
    } else {
        link.raw_text.clone()
    }
}

fn extract_markdown_label(raw_text: &str) -> String {
    let start = if raw_text.starts_with("![") { 2 } else { 1 };
    raw_text[start..]
        .split("](")
        .next()
        .unwrap_or_default()
        .to_string()
}

fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
    let mut updated = source.to_string();
    let mut sorted = edits.to_vec();
    sorted.sort_by(|left, right| right.start.cmp(&left.start));
    for edit in sorted {
        updated.replace_range(edit.start..edit.end, &edit.replacement);
    }
    updated
}

fn line_end(source: &str, start: usize) -> usize {
    source[start..]
        .find('\n')
        .map_or(source.len(), |offset| start + offset)
}

fn trim_line(line: &str) -> &str {
    line.trim_end_matches('\n').trim_end_matches('\r')
}

fn split_tag_tokens(text: &str) -> Vec<&str> {
    text.split(|character: char| character == ',' || character.is_whitespace())
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalize_tag_name(tag: &str) -> String {
    tag.trim().trim_start_matches('#').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{doctor_vault, query_notes, resolve_note_reference, scan_vault, NoteQuery};
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn rename_property_updates_frontmatter_and_reindexes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("refactors", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        let report =
            rename_property(&paths, "status", "phase", false).expect("rename should succeed");

        assert_eq!(report.files.len(), 1);
        let home = fs::read_to_string(vault_root.join("Home.md")).expect("home should read");
        assert!(home.contains("phase: active"));
        assert!(!home.contains("status: active"));
        let notes = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["phase = active".to_string()],
                sort_by: None,
                sort_descending: false,
            },
        )
        .expect("notes query should succeed");
        assert_eq!(notes.notes.len(), 1);
        assert_eq!(notes.notes[0].document_path, "Home.md");
    }

    #[test]
    fn merge_tags_updates_frontmatter_and_inline_tags() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("refactors", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        let report =
            merge_tags(&paths, "project", "initiative", false).expect("merge should succeed");

        assert_eq!(report.files.len(), 2);
        let home = fs::read_to_string(vault_root.join("Home.md")).expect("home should read");
        let alpha =
            fs::read_to_string(vault_root.join("Projects/Alpha.md")).expect("alpha should read");
        assert!(home.contains("initiative"));
        assert!(home.contains("#initiative"));
        assert!(alpha.contains("tags: initiative"));
        assert!(!home.contains("#project"));
    }

    #[test]
    fn rename_alias_heading_and_block_ref_update_note_graph() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("refactors", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        rename_alias(&paths, "Home.md", "Start", "Landing", false)
            .expect("alias rename should succeed");
        assert_eq!(
            resolve_note_reference(&paths, "Landing")
                .expect("alias should resolve")
                .path,
            "Home.md"
        );

        rename_heading(&paths, "Projects/Alpha.md", "Status", "Progress", false)
            .expect("heading rename should succeed");
        rename_block_ref(
            &paths,
            "Projects/Alpha.md",
            "alpha-status",
            "alpha-progress",
            false,
        )
        .expect("block ref rename should succeed");

        let home = fs::read_to_string(vault_root.join("Home.md")).expect("home should read");
        let alpha =
            fs::read_to_string(vault_root.join("Projects/Alpha.md")).expect("alpha should read");
        assert!(home.contains("[[Projects/Alpha#Progress]]"));
        assert!(home.contains("[[Projects/Alpha#^alpha-progress]]"));
        assert!(alpha.contains("Self link [[#Progress]]."));
        assert!(alpha.contains("^alpha-progress"));
        assert_eq!(
            doctor_vault(&paths)
                .expect("doctor should succeed")
                .summary
                .unresolved_links,
            0
        );
    }

    #[test]
    fn rename_alias_dry_run_preserves_source_files() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("refactors", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let before = fs::read_to_string(vault_root.join("Home.md")).expect("home should read");

        let report = rename_alias(&paths, "Home.md", "Start", "Landing", true)
            .expect("dry run should succeed");

        assert!(report.dry_run);
        assert_eq!(
            fs::read_to_string(vault_root.join("Home.md")).expect("home should read"),
            before
        );
    }

    #[test]
    fn set_note_property_updates_frontmatter_and_reindexes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("mixed-properties", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = set_note_property(&paths, "Done", "status", Some("shipped"), false)
            .expect("property update should succeed");

        assert_eq!(report.files.len(), 1);
        let done = fs::read_to_string(vault_root.join("Done.md")).expect("done note should read");
        assert!(done.contains("status: shipped"));
        let notes = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["status = shipped".to_string()],
                sort_by: None,
                sort_descending: false,
            },
        )
        .expect("notes query should succeed");
        assert_eq!(notes.notes.len(), 1);
        assert_eq!(notes.notes[0].document_path, "Done.md");
    }

    #[test]
    fn set_note_property_creates_frontmatter_when_missing() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join("Scratch.md"),
            "# Scratch\n\nNo frontmatter yet.\n",
        )
        .expect("scratch note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = set_note_property(&paths, "Scratch", "owner", Some("alice"), false)
            .expect("property creation should succeed");
        assert_eq!(report.files.len(), 1);
        let scratch =
            fs::read_to_string(vault_root.join("Scratch.md")).expect("scratch should read");
        assert!(scratch.starts_with("---\nowner: alice\n---\n"));
        let notes = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["owner = alice".to_string()],
                sort_by: None,
                sort_descending: false,
            },
        )
        .expect("notes query should succeed");
        assert_eq!(notes.notes[0].document_path, "Scratch.md");
    }

    #[test]
    fn set_note_property_with_empty_value_removes_property() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("mixed-properties", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        set_note_property(&paths, "Done", "reviewed", Some(""), false)
            .expect("property removal should succeed");

        let done = fs::read_to_string(vault_root.join("Done.md")).expect("done note should read");
        assert!(!done.contains("reviewed: true"));
        let notes = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["reviewed = true".to_string()],
                sort_by: None,
                sort_descending: false,
            },
        )
        .expect("notes query should succeed");
        assert!(notes.notes.is_empty());
    }

    // === Surgical editing unit tests ===

    #[test]
    fn surgical_rename_preserves_all_other_formatting() {
        let yaml = "# comment\nstatus: active\n# another comment\nestimate: 8\nreviewed: false\nrelated:\n- item1\n- item2\n";
        let result = surgical_rename_key(yaml, "status", "phase").expect("should rename");
        // Key name changed
        assert!(result.contains("phase: active\n"));
        assert!(!result.contains("status: active\n"));
        // All other lines preserved byte-for-byte
        assert!(result.contains("# comment\n"));
        assert!(result.contains("# another comment\n"));
        assert!(result.contains("estimate: 8\n"));
        assert!(result.contains("reviewed: false\n"));
        assert!(result.contains("related:\n"));
        assert!(result.contains("- item1\n"));
        assert!(result.contains("- item2\n"));
    }

    #[test]
    fn surgical_rename_returns_none_when_key_absent() {
        let yaml = "status: active\nestimate: 8\n";
        assert!(surgical_rename_key(yaml, "nonexistent", "new").is_none());
    }

    #[test]
    fn surgical_set_scalar_preserves_other_keys() {
        let yaml = "status: backlog\nestimate: 8\n# note below\nreviewed: false\n";
        let new_value = YamlValue::String("done".to_string());
        let result = surgical_set_key(yaml, "status", Some(&new_value)).expect("should set");
        assert!(result.contains("status: done\n"));
        assert!(!result.contains("status: backlog\n"));
        // Other keys byte-for-byte
        assert!(result.contains("estimate: 8\n"));
        assert!(result.contains("# note below\n"));
        assert!(result.contains("reviewed: false\n"));
    }

    #[test]
    fn surgical_remove_key_leaves_others_intact() {
        let yaml = "status: backlog\nestimate: 8\nreviewed: false\n";
        let result = surgical_set_key(yaml, "estimate", None).expect("should remove");
        assert!(!result.contains("estimate:"));
        assert!(result.contains("status: backlog\n"));
        assert!(result.contains("reviewed: false\n"));
    }

    #[test]
    fn surgical_set_block_list_replaces_only_that_key() {
        let yaml = "status: backlog\nrelated:\n- item1\n- item2\nreviewed: false\n";
        let new_value = YamlValue::Sequence(vec![
            YamlValue::String("new1".to_string()),
            YamlValue::String("new2".to_string()),
        ]);
        let result = surgical_set_key(yaml, "related", Some(&new_value)).expect("should set");
        assert!(result.contains("related:\n"));
        assert!(result.contains("- new1\n"));
        assert!(result.contains("- new2\n"));
        assert!(!result.contains("- item1\n"));
        assert!(result.contains("status: backlog\n"));
        assert!(result.contains("reviewed: false\n"));
    }

    #[test]
    fn rename_property_diff_is_minimal() {
        // After surgical rename, only the key line should change; all other bytes are identical.
        let old_yaml = "# top comment\nstatus: active\nestimate: 8\nrelated:\n  - '[[Done]]'\n";
        let new_yaml = surgical_rename_key(old_yaml, "status", "phase").expect("should rename");
        // Only one line changed
        let changed_lines: Vec<_> = old_yaml
            .lines()
            .zip(new_yaml.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(
            changed_lines.len(),
            1,
            "surgical rename should change exactly one line"
        );
        assert_eq!(changed_lines[0].0, "status: active");
        assert_eq!(changed_lines[0].1, "phase: active");
    }

    #[test]
    fn set_note_property_produces_minimal_diff_for_scalar() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("mixed-properties", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let before =
            fs::read_to_string(vault_root.join("Backlog.md")).expect("backlog should read");
        set_note_property(&paths, "Backlog", "status", Some("done"), false)
            .expect("set should succeed");
        let after = fs::read_to_string(vault_root.join("Backlog.md")).expect("backlog should read");

        // Count changed lines (only the `status:` line should differ)
        let before_lines: Vec<_> = before.lines().collect();
        let after_lines: Vec<_> = after.lines().collect();
        assert_eq!(
            before_lines.len(),
            after_lines.len(),
            "surgical set should not change the number of lines"
        );
        let changed: Vec<_> = before_lines
            .iter()
            .zip(after_lines.iter())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(
            changed.len(),
            1,
            "only the status line should change: {changed:?}"
        );
    }

    #[test]
    fn repeated_set_property_is_stable() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("mixed-properties", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        set_note_property(&paths, "Backlog", "status", Some("done"), false)
            .expect("first set should succeed");
        let first = fs::read_to_string(vault_root.join("Backlog.md")).expect("backlog should read");

        // Second identical set should produce no changes
        let report = set_note_property(&paths, "Backlog", "status", Some("done"), false)
            .expect("second set should succeed");
        let second =
            fs::read_to_string(vault_root.join("Backlog.md")).expect("backlog should read");

        assert_eq!(report.files.len(), 0, "second set should be a no-op");
        assert_eq!(
            first, second,
            "file should be identical after idempotent set"
        );
    }

    #[test]
    fn rename_property_preserves_comments_and_list_formatting() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        let paths = VaultPaths::new(&vault_root);
        fs::create_dir_all(&vault_root).expect("vault dir should create");

        // Write a note with a comment and a list property
        fs::write(
            vault_root.join("Note.md"),
            "---\n# status metadata\nstatus: active\nrelated:\n  - '[[A]]'\n  - '[[B]]'\ncreated: 2026-01-01\n---\n# Body\n",
        )
        .expect("note should write");

        let before = fs::read_to_string(vault_root.join("Note.md")).expect("note should read");
        rename_property(&paths, "status", "phase", false).expect("rename should succeed");
        let after = fs::read_to_string(vault_root.join("Note.md")).expect("note should read");

        assert!(after.contains("phase: active\n"));
        assert!(!after.contains("status: active\n"));
        // Comments and list values are preserved byte-for-byte
        assert!(after.contains("# status metadata\n"));
        assert!(
            after.contains("  - '[[A]]'\n"),
            "list indent should be preserved"
        );
        assert!(
            after.contains("  - '[[B]]'\n"),
            "list indent should be preserved"
        );
        assert!(after.contains("created: 2026-01-01\n"));
        // Only the key line changed
        let before_lines: Vec<_> = before.lines().collect();
        let after_lines: Vec<_> = after.lines().collect();
        let changed: Vec<_> = before_lines
            .iter()
            .zip(after_lines.iter())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(changed.len(), 1, "only the key line should change");
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_recursive(&source, destination);
        fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination should exist");
        for entry in fs::read_dir(source).expect("source dir should read") {
            let entry = entry.expect("entry should read");
            let file_type = entry.file_type().expect("file type should read");
            let target = destination.join(entry.file_name());
            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).expect("parent should exist");
                }
                fs::copy(entry.path(), target).expect("file should copy");
            }
        }
    }
}
