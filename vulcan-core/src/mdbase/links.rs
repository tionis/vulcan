use super::{
    compose_mdbase_type_behavior, resolve_match_field, MdbaseCollection, MdbaseRecordDiagnostic,
    MdbaseRecordDiagnosticSeverity, MdbaseRecordDocument, MdbaseTypeRegistry,
    MdbaseValidationLevel,
};
use crate::config::VaultConfig;
use crate::parser::{parse_document, LinkKind, RawLink};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MdbaseLinkFormat {
    Wikilink,
    Markdown,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MdbaseLinkSource {
    Frontmatter,
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MdbaseLinkResolution {
    Resolved,
    NotFound,
    Ambiguous,
    Invalid,
    TargetTypeMismatch,
}

/// A loss-preserving mdbase link occurrence. `raw` is suitable for future
/// round-trip rewrites; the remaining members are parsed or derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseLink {
    pub raw: String,
    pub target: String,
    pub alias: Option<String>,
    pub anchor: Option<String>,
    pub format: MdbaseLinkFormat,
    pub is_relative: bool,
    pub source: MdbaseLinkSource,
    pub field: Option<String>,
    pub target_type: Option<String>,
    pub validate_exists: bool,
    pub embed: bool,
    pub resolved_path: Option<String>,
    pub resolution: MdbaseLinkResolution,
}

#[derive(Debug, Clone)]
struct ParsedLink {
    raw: String,
    target: String,
    alias: Option<String>,
    anchor: Option<String>,
    format: MdbaseLinkFormat,
    embed: bool,
}

#[derive(Debug, Clone, Copy)]
struct LinkRule<'a> {
    field: &'a str,
    target_type: Option<&'a str>,
    validate_exists: bool,
}

pub(crate) fn resolve_collection_links(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    records: &mut [MdbaseRecordDocument],
) {
    let id_field = collection.config.settings.id_field.as_str();
    let ids = id_index(records, id_field);
    let snapshot = records.to_vec();
    for record in records {
        let behavior = compose_mdbase_type_behavior(types, &record.types);
        let mut links = Vec::new();
        if let Some(frontmatter) = record.effective_frontmatter.as_object() {
            for (field, value) in &behavior.links {
                let rule = link_rule(field, value);
                let selected = resolve_match_field(frontmatter, field);
                for value in selected.values {
                    for value in link_strings(value) {
                        if let Some(parsed) = parse_link_value(value) {
                            links.push(resolve_link(parsed, record, Some(rule), &snapshot, &ids));
                        }
                    }
                }
            }
        }
        let parsed = parse_document(&record.body, &VaultConfig::default());
        links.extend(parsed.links.iter().filter_map(|link| {
            parsed_body_link(link).map(|parsed| resolve_link(parsed, record, None, &snapshot, &ids))
        }));
        links.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.field.cmp(&right.field))
                .then_with(|| left.raw.cmp(&right.raw))
        });
        add_link_diagnostics(collection, record, &links);
        record.links = links;
        record.tags = collect_record_tags(record, parsed.tags.into_iter().map(|tag| tag.tag_text));
    }
}

fn link_rule<'a>(field: &'a str, value: &'a serde_json::Value) -> LinkRule<'a> {
    LinkRule {
        field,
        target_type: value.get("target_type").and_then(serde_json::Value::as_str),
        validate_exists: value
            .get("validate_exists")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    }
}

fn link_strings(value: &serde_json::Value) -> Vec<&str> {
    match value {
        serde_json::Value::String(value) => vec![value],
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn parse_mdbase_link_value(value: &str) -> Option<MdbaseLink> {
    let parsed = parse_link_value(value)?;
    Some(MdbaseLink {
        is_relative: !parsed.target.starts_with('/'),
        raw: parsed.raw,
        target: parsed.target,
        alias: parsed.alias,
        anchor: parsed.anchor,
        format: parsed.format,
        source: MdbaseLinkSource::Body,
        field: None,
        target_type: None,
        validate_exists: false,
        embed: parsed.embed,
        resolved_path: None,
        resolution: MdbaseLinkResolution::NotFound,
    })
}

fn parse_link_value(value: &str) -> Option<ParsedLink> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = parse_document(trimmed, &VaultConfig::default());
    if let Some(link) = parsed.links.first() {
        if let Some(link) = parsed_body_link(link) {
            return Some(link);
        }
    }
    let (target, anchor) = split_anchor(trimmed);
    Some(ParsedLink {
        raw: value.to_string(),
        target: target.to_string(),
        alias: None,
        anchor: anchor.map(ToString::to_string),
        format: MdbaseLinkFormat::Path,
        embed: false,
    })
}

fn parsed_body_link(link: &RawLink) -> Option<ParsedLink> {
    if link.link_kind == LinkKind::External {
        return None;
    }
    let target = link.target_path_candidate.as_deref()?;
    let anchor = link
        .target_heading
        .as_deref()
        .map(ToString::to_string)
        .or_else(|| {
            link.target_block
                .as_deref()
                .map(|block| format!("^{block}"))
        });
    Some(ParsedLink {
        raw: link.raw_text.clone(),
        target: target.to_string(),
        alias: link.display_text.clone(),
        anchor,
        format: match link.link_kind {
            LinkKind::Wikilink | LinkKind::Embed => MdbaseLinkFormat::Wikilink,
            LinkKind::Markdown => MdbaseLinkFormat::Markdown,
            LinkKind::External => return None,
        },
        embed: link.link_kind == LinkKind::Embed,
    })
}

fn split_anchor(value: &str) -> (&str, Option<&str>) {
    value
        .split_once('#')
        .map_or((value, None), |(target, anchor)| (target, Some(anchor)))
}

fn resolve_link(
    parsed: ParsedLink,
    source: &MdbaseRecordDocument,
    rule: Option<LinkRule<'_>>,
    records: &[MdbaseRecordDocument],
    ids: &BTreeMap<String, Vec<String>>,
) -> MdbaseLink {
    let is_relative = !parsed.target.starts_with('/');
    let (resolved_path, mut resolution) = resolve_target(&parsed, &source.path, records, ids);
    if let (Some(target_type), Some(path)) =
        (rule.and_then(|rule| rule.target_type), &resolved_path)
    {
        let matches = records
            .iter()
            .find(|record| &record.path == path)
            .is_some_and(|record| {
                record
                    .types
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(target_type))
            });
        if !matches {
            resolution = MdbaseLinkResolution::TargetTypeMismatch;
        }
    }
    MdbaseLink {
        raw: parsed.raw,
        target: parsed.target,
        alias: parsed.alias,
        anchor: parsed.anchor,
        format: parsed.format,
        is_relative,
        source: if rule.is_some() {
            MdbaseLinkSource::Frontmatter
        } else {
            MdbaseLinkSource::Body
        },
        field: rule.map(|rule| rule.field.to_string()),
        target_type: rule.and_then(|rule| rule.target_type.map(ToString::to_string)),
        validate_exists: rule.is_some_and(|rule| rule.validate_exists),
        embed: parsed.embed,
        resolved_path,
        resolution,
    }
}

fn resolve_target(
    parsed: &ParsedLink,
    source_path: &str,
    records: &[MdbaseRecordDocument],
    ids: &BTreeMap<String, Vec<String>>,
) -> (Option<String>, MdbaseLinkResolution) {
    let simple_wikilink = parsed.format == MdbaseLinkFormat::Wikilink
        && !parsed.target.contains('/')
        && !parsed.target.starts_with('.');
    if simple_wikilink {
        if let Some(paths) = ids.get(&parsed.target) {
            return match paths.as_slice() {
                [path] => (Some(path.clone()), MdbaseLinkResolution::Resolved),
                [] => unreachable!("ID index never stores empty entries"),
                _ => (None, MdbaseLinkResolution::Ambiguous),
            };
        }
        return resolve_filename(&parsed.target, source_path, records);
    }
    let Some(candidate) = normalize_target_path(source_path, &parsed.target) else {
        return (None, MdbaseLinkResolution::Invalid);
    };
    resolve_exact(candidate, records)
}

fn resolve_exact(
    candidate: String,
    records: &[MdbaseRecordDocument],
) -> (Option<String>, MdbaseLinkResolution) {
    let candidates = if std::path::Path::new(&candidate)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        vec![candidate]
    } else {
        vec![candidate.clone(), format!("{candidate}.md")]
    };
    candidates
        .into_iter()
        .find(|candidate| records.iter().any(|record| record.path == *candidate))
        .map_or((None, MdbaseLinkResolution::NotFound), |path| {
            (Some(path), MdbaseLinkResolution::Resolved)
        })
}

fn resolve_filename(
    target: &str,
    source_path: &str,
    records: &[MdbaseRecordDocument],
) -> (Option<String>, MdbaseLinkResolution) {
    let wanted = target.strip_suffix(".md").unwrap_or(target);
    let source_folder = source_path
        .rsplit_once('/')
        .map_or("", |(folder, _)| folder);
    let mut candidates = records
        .iter()
        .filter(|record| record.file.basename == wanted)
        .map(|record| record.path.clone())
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let left_folder = left.rsplit_once('/').map_or("", |(folder, _)| folder);
        let right_folder = right.rsplit_once('/').map_or("", |(folder, _)| folder);
        (left_folder != source_folder)
            .cmp(&(right_folder != source_folder))
            .then_with(|| left.len().cmp(&right.len()))
            .then_with(|| left.cmp(right))
    });
    candidates
        .into_iter()
        .next()
        .map_or((None, MdbaseLinkResolution::NotFound), |path| {
            (Some(path), MdbaseLinkResolution::Resolved)
        })
}

fn normalize_target_path(source_path: &str, target: &str) -> Option<String> {
    let mut components = Vec::new();
    if !target.starts_with('/') {
        if let Some((folder, _)) = source_path.rsplit_once('/') {
            components.extend(folder.split('/').filter(|value| !value.is_empty()));
        }
    }
    for component in target.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value if value.contains(['\\', '\0']) => return None,
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn id_index(records: &[MdbaseRecordDocument], id_field: &str) -> BTreeMap<String, Vec<String>> {
    let mut ids = BTreeMap::<String, Vec<String>>::new();
    for record in records {
        if let Some(id) = record
            .frontmatter
            .get(id_field)
            .and_then(serde_json::Value::as_str)
        {
            ids.entry(id.to_string())
                .or_default()
                .push(record.path.clone());
        }
    }
    ids
}

fn add_link_diagnostics(
    collection: &MdbaseCollection,
    record: &mut MdbaseRecordDocument,
    links: &[MdbaseLink],
) {
    if collection.config.settings.validation == MdbaseValidationLevel::Off {
        return;
    }
    let severity = if collection.config.settings.validation == MdbaseValidationLevel::Error {
        MdbaseRecordDiagnosticSeverity::Error
    } else {
        MdbaseRecordDiagnosticSeverity::Warning
    };
    for link in links
        .iter()
        .filter(|link| link.source == MdbaseLinkSource::Frontmatter)
    {
        let diagnostic = match link.resolution {
            MdbaseLinkResolution::NotFound if link.validate_exists => Some((
                "link_not_found",
                format!("link target was not found: {}", link.target),
            )),
            MdbaseLinkResolution::Ambiguous if link.validate_exists => Some((
                "link_ambiguous",
                format!("link target is ambiguous: {}", link.target),
            )),
            MdbaseLinkResolution::Invalid => Some((
                "link_invalid",
                format!("link target escapes the collection: {}", link.target),
            )),
            MdbaseLinkResolution::TargetTypeMismatch => Some((
                "link_target_type_mismatch",
                format!("link target has the wrong type: {}", link.target),
            )),
            _ => None,
        };
        if let Some((code, message)) = diagnostic {
            record.diagnostics.push(MdbaseRecordDiagnostic {
                severity,
                code: code.to_string(),
                message,
                path: record.path.clone(),
                field: link.field.clone().unwrap_or_default(),
                type_name: None,
                schema_path: None,
                related_paths: link.resolved_path.iter().cloned().collect(),
            });
        }
    }
}

fn collect_record_tags(
    record: &MdbaseRecordDocument,
    body_tags: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut tags = std::collections::BTreeSet::new();
    if let Some(value) = record.effective_frontmatter.get("tags") {
        match value {
            serde_json::Value::String(value) => {
                tags.extend(value.split([',', ' ']).filter_map(normalize_tag));
            }
            serde_json::Value::Array(values) => tags.extend(
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter_map(normalize_tag),
            ),
            _ => {}
        }
    }
    tags.extend(body_tags.into_iter().filter_map(|tag| normalize_tag(&tag)));
    tags.into_iter().collect()
}

fn normalize_tag(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches('#');
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_portable_link_value_forms() {
        let wiki = parse_link_value("[[task-1|Parent]]").expect("wikilink");
        assert_eq!(wiki.target, "task-1");
        assert_eq!(wiki.alias.as_deref(), Some("Parent"));
        assert_eq!(wiki.format, MdbaseLinkFormat::Wikilink);

        let markdown = parse_link_value("[Parent](../tasks/parent.md#Details)").expect("markdown");
        assert_eq!(markdown.target, "../tasks/parent.md");
        assert_eq!(markdown.anchor.as_deref(), Some("Details"));
        assert_eq!(markdown.format, MdbaseLinkFormat::Markdown);

        let path = parse_link_value("../tasks/parent.md").expect("path");
        assert_eq!(path.format, MdbaseLinkFormat::Path);
    }

    #[test]
    fn path_normalization_is_file_relative_root_relative_and_escape_safe() {
        assert_eq!(
            normalize_target_path("projects/child.md", "../tasks/parent.md").as_deref(),
            Some("tasks/parent.md")
        );
        assert_eq!(
            normalize_target_path("projects/child.md", "/tasks/parent.md").as_deref(),
            Some("tasks/parent.md")
        );
        assert!(normalize_target_path("child.md", "../secret.md").is_none());
    }
}
