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
            Self::AmbiguousTarget { .. }
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

        if let Some((edit, edit_changes)) = plan_frontmatter_replacement(&source, |frontmatter| {
            rename_frontmatter_property(frontmatter, &path, old_key, new_key)
        })? {
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

        if let Some((edit, edit_changes)) = plan_frontmatter_replacement(&source, |frontmatter| {
            Ok(merge_frontmatter_tags(
                frontmatter,
                &source_tag,
                &destination_tag,
            ))
        })? {
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

    if let Some((edit, edit_changes)) = plan_frontmatter_replacement(&source, |frontmatter| {
        rename_frontmatter_alias(frontmatter, &path, old_alias, new_alias)
    })? {
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

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_recursive(&source, destination);
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
