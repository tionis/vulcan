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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ParsedLink {
    raw: String,
    target: String,
    alias: Option<String>,
    anchor: Option<String>,
    format: MdbaseLinkFormat,
    embed: bool,
}

/// Source-local parse facts only: no target resolution or visible-scope answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct BodyLinkFacts {
    links: Vec<ParsedLink>,
    tags: Vec<String>,
}

#[cfg(test)]
thread_local! {
    static BODY_FACT_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn take_body_fact_parse_count() -> usize {
    BODY_FACT_PARSES.with(|count| count.replace(0))
}

impl BodyLinkFacts {
    pub(super) fn parse(body: &str) -> Self {
        #[cfg(test)]
        BODY_FACT_PARSES.with(|count| count.set(count.get() + 1));
        let parsed = parse_document(body, &VaultConfig::default());
        Self {
            links: parsed.links.iter().filter_map(parsed_body_link).collect(),
            tags: parsed.tags.into_iter().map(|tag| tag.tag_text).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct LinkRule<'a> {
    field: &'a str,
    target_type: Option<&'a str>,
    validate_exists: bool,
}

/// Resolution-only data from the caller's visible snapshot. Never retain note
/// bodies, exact source, arbitrary fields, diagnostics, or contract views here.
struct LinkTargetIndex {
    types_by_path: BTreeMap<String, Vec<String>>,
    paths_by_basename: BTreeMap<String, Vec<String>>,
    paths_by_id: BTreeMap<String, Vec<String>>,
}

impl LinkTargetIndex {
    fn new(records: &[MdbaseRecordDocument], id_field: &str) -> Self {
        let mut index = Self {
            types_by_path: BTreeMap::new(),
            paths_by_basename: BTreeMap::new(),
            paths_by_id: BTreeMap::new(),
        };
        for record in records {
            index
                .types_by_path
                .entry(record.path.clone())
                .or_insert_with(|| record.types.clone());
            index
                .paths_by_basename
                .entry(record.file.basename.clone())
                .or_default()
                .push(record.path.clone());
            // IDs intentionally come from authored values, never read defaults.
            if let Some(id) = record
                .frontmatter
                .get(id_field)
                .and_then(serde_json::Value::as_str)
            {
                index
                    .paths_by_id
                    .entry(id.to_string())
                    .or_default()
                    .push(record.path.clone());
            }
        }
        index
    }
}

pub(crate) fn resolve_collection_links(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    records: &mut [MdbaseRecordDocument],
) {
    resolve_collection_links_with_body_facts(collection, types, records, &BTreeMap::new());
}

pub(super) fn resolve_collection_links_with_body_facts(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    records: &mut [MdbaseRecordDocument],
    body_facts: &BTreeMap<String, BodyLinkFacts>,
) {
    let id_field = collection.config.settings.id_field.as_str();
    let index = LinkTargetIndex::new(records, id_field);
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
                            links.push(resolve_link(parsed, record, Some(rule), &index));
                        }
                    }
                }
            }
        }
        let fallback;
        let facts = if let Some(facts) = body_facts.get(&record.path) {
            facts
        } else {
            fallback = BodyLinkFacts::parse(&record.body);
            &fallback
        };
        links.extend(
            facts
                .links
                .iter()
                .cloned()
                .map(|parsed| resolve_link(parsed, record, None, &index)),
        );
        links.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.field.cmp(&right.field))
                .then_with(|| left.raw.cmp(&right.raw))
        });
        add_link_diagnostics(collection, record, &links);
        record.links = links;
        record.tags = collect_record_tags(record, facts.tags.iter().cloned());
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
    index: &LinkTargetIndex,
) -> MdbaseLink {
    let is_relative = !parsed.target.starts_with('/');
    let (resolved_path, mut resolution) = resolve_target(&parsed, &source.path, index);
    if let (Some(target_type), Some(path)) =
        (rule.and_then(|rule| rule.target_type), &resolved_path)
    {
        let matches = target_type == "any"
            || index.types_by_path.get(path).is_some_and(|types| {
                types
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
    index: &LinkTargetIndex,
) -> (Option<String>, MdbaseLinkResolution) {
    let simple_wikilink = parsed.format == MdbaseLinkFormat::Wikilink
        && !parsed.target.contains('/')
        && !parsed.target.starts_with('.');
    if simple_wikilink {
        if let Some(paths) = index.paths_by_id.get(&parsed.target) {
            return match paths.as_slice() {
                [path] => (Some(path.clone()), MdbaseLinkResolution::Resolved),
                [] => unreachable!("ID index never stores empty entries"),
                _ => (None, MdbaseLinkResolution::Ambiguous),
            };
        }
        return resolve_filename(&parsed.target, source_path, index);
    }
    let Some(candidate) = normalize_target_path(source_path, &parsed.target) else {
        return (None, MdbaseLinkResolution::Invalid);
    };
    resolve_exact(candidate, index)
}

fn resolve_exact(
    candidate: String,
    index: &LinkTargetIndex,
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
        .find(|candidate| index.types_by_path.contains_key(candidate))
        .map_or((None, MdbaseLinkResolution::NotFound), |path| {
            (Some(path), MdbaseLinkResolution::Resolved)
        })
}

fn resolve_filename(
    target: &str,
    source_path: &str,
    index: &LinkTargetIndex,
) -> (Option<String>, MdbaseLinkResolution) {
    let wanted = target.strip_suffix(".md").unwrap_or(target);
    let source_folder = source_path
        .rsplit_once('/')
        .map_or("", |(folder, _)| folder);
    index
        .paths_by_basename
        .get(wanted)
        .into_iter()
        .flatten()
        .min_by(|left, right| {
            let left_folder = left.rsplit_once('/').map_or("", |(folder, _)| folder);
            let right_folder = right.rsplit_once('/').map_or("", |(folder, _)| folder);
            (left_folder != source_folder)
                .cmp(&(right_folder != source_folder))
                .then_with(|| left.len().cmp(&right.len()))
                .then_with(|| left.cmp(right))
        })
        .map_or((None, MdbaseLinkResolution::NotFound), |path| {
            (Some(path.clone()), MdbaseLinkResolution::Resolved)
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

    fn target(path: &str, id: serde_json::Value) -> MdbaseRecordDocument {
        MdbaseRecordDocument {
            path: path.to_string(),
            revision: String::new(),
            types: vec!["task".to_string()],
            frontmatter: serde_json::Value::Object(serde_json::Map::from_iter([(
                "key".to_string(),
                id,
            )])),
            effective_frontmatter: serde_json::json!({"key": "default-id"}),
            body: String::new(),
            document: None,
            file: super::super::records::file_metadata(path, 0, None),
            links: vec![],
            tags: vec![],
            display: None,
            contract_views: vec![],
            diagnostics: vec![],
        }
    }

    // Independent scan-based oracle matching the pre-index resolution policy.
    fn scan_target(
        parsed: &ParsedLink,
        source: &str,
        records: &[MdbaseRecordDocument],
    ) -> (Option<String>, MdbaseLinkResolution) {
        if parsed.format == MdbaseLinkFormat::Wikilink
            && !parsed.target.contains('/')
            && !parsed.target.starts_with('.')
        {
            let ids = records
                .iter()
                .filter(|record| {
                    record
                        .frontmatter
                        .get("key")
                        .and_then(serde_json::Value::as_str)
                        == Some(parsed.target.as_str())
                })
                .collect::<Vec<_>>();
            if ids.len() > 1 {
                return (None, MdbaseLinkResolution::Ambiguous);
            }
            if let Some(record) = ids.first() {
                return (Some(record.path.clone()), MdbaseLinkResolution::Resolved);
            }
            let wanted = parsed.target.strip_suffix(".md").unwrap_or(&parsed.target);
            let folder = source.rsplit_once('/').map_or("", |(folder, _)| folder);
            let mut candidates = records
                .iter()
                .filter(|record| record.file.basename == wanted)
                .map(|record| record.path.clone())
                .collect::<Vec<_>>();
            candidates.sort_by_key(|path| {
                (
                    path.rsplit_once('/').map_or("", |(parent, _)| parent) != folder,
                    path.len(),
                    path.clone(),
                )
            });
            return candidates
                .into_iter()
                .next()
                .map_or((None, MdbaseLinkResolution::NotFound), |path| {
                    (Some(path), MdbaseLinkResolution::Resolved)
                });
        }
        let Some(candidate) = normalize_target_path(source, &parsed.target) else {
            return (None, MdbaseLinkResolution::Invalid);
        };
        let candidates = if std::path::Path::new(&candidate)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            vec![candidate]
        } else {
            vec![candidate.clone(), format!("{candidate}.md")]
        };
        candidates
            .into_iter()
            .find(|path| records.iter().any(|record| &record.path == path))
            .map_or((None, MdbaseLinkResolution::NotFound), |path| {
                (Some(path), MdbaseLinkResolution::Resolved)
            })
    }

    #[test]
    fn indexed_targets_match_scan_precedence_ties_scope_and_type_rules() {
        let records = vec![
            target("long-folder/item.md", serde_json::json!("unique")),
            target("a/item.md", serde_json::json!("duplicate")),
            target("b/item.md", serde_json::json!("duplicate")),
            target("item.md", serde_json::Value::Null),
            target("unique.md", serde_json::json!(42)),
            target("Case.MD", serde_json::Value::Null),
            target("other/α.md", serde_json::json!("unicode")),
            target("a/direct", serde_json::Value::Null),
            target("a/direct.md", serde_json::Value::Null),
        ];
        for visible in [records.as_slice(), &records[1..], &records[..0]] {
            let index = LinkTargetIndex::new(visible, "key");
            for source in [
                "a/source.md",
                "b/source.md",
                "source.md",
                "long-folder/source.md",
            ] {
                for raw in [
                    "[[unique|Alias]]",
                    "[[duplicate]]",
                    "[[item]]",
                    "[[item.md#Anchor]]",
                    "[[Case.MD]]",
                    "[[case]]",
                    "[[unicode]]",
                    "[[default-id]]",
                    "[[42]]",
                    "[[missing]]",
                    "[[../item]]",
                    "[x](/a/direct)",
                    "[x](direct)",
                    "[x](/Case.MD)",
                    "[x](../../escape.md)",
                    "[x](/other/α.md)",
                ] {
                    let parsed = parse_link_value(raw).unwrap();
                    let expected = scan_target(&parsed, source, visible);
                    assert_eq!(
                        resolve_target(&parsed, source, &index),
                        expected,
                        "{source} {raw}"
                    );
                    for wanted_type in ["task", "TASK", "contact", "any"] {
                        let link = resolve_link(
                            parsed.clone(),
                            &target(source, serde_json::Value::Null),
                            Some(LinkRule {
                                field: "parent",
                                target_type: Some(wanted_type),
                                validate_exists: true,
                            }),
                            &index,
                        );
                        let status = if expected.0.is_some() && wanted_type == "contact" {
                            MdbaseLinkResolution::TargetTypeMismatch
                        } else {
                            expected.1
                        };
                        assert_eq!(
                            (link.resolved_path, link.resolution),
                            (expected.0.clone(), status)
                        );
                        assert_eq!(link.raw, parsed.raw);
                        assert_eq!(link.alias, parsed.alias);
                        assert_eq!(link.anchor, parsed.anchor);
                    }
                }
            }
        }
    }

    #[test]
    fn ten_thousand_targets_index_only_matching_basename_candidates() {
        let mut records = (0..10_000)
            .map(|i| {
                target(
                    &format!("notes/n{i:05}.md"),
                    serde_json::json!(format!("id-{i}")),
                )
            })
            .collect::<Vec<_>>();
        records[9999].body = "body must not be retained".repeat(4096);
        let index = LinkTargetIndex::new(&records, "key");
        assert_eq!(index.types_by_path.len(), 10_000);
        assert_eq!(
            index.paths_by_basename.get("n09999").unwrap(),
            &["notes/n09999.md"]
        );
        assert_eq!(
            index.paths_by_id.get("id-9999").unwrap(),
            &["notes/n09999.md"]
        );
        // The index owns only resolution keys; record lifetimes/body size do not
        // constrain it. A new visible snapshot rebuilds its own index.
        drop(records);
        assert_eq!(
            resolve_filename("n09999", "source.md", &index).0.as_deref(),
            Some("notes/n09999.md")
        );
        assert!(
            resolve_filename("n09999", "source.md", &LinkTargetIndex::new(&[], "key"))
                .0
                .is_none()
        );
    }

    #[test]
    #[ignore = "release component benchmark; run serialized with --nocapture"]
    fn link_target_index_component_benchmark() {
        let records = (0..10_000)
            .map(|i| {
                let mut record = target(&format!("notes/n{i:05}.md"), serde_json::Value::Null);
                record.body = "x".repeat(4096);
                record
            })
            .collect::<Vec<_>>();
        let index = LinkTargetIndex::new(&records, "key");
        let targets = (9990..10_000)
            .map(|i| parse_link_value(&format!("[Target](/notes/n{i:05}.md)")).unwrap())
            .collect::<Vec<_>>();
        for indexed in [false, true] {
            let mut samples = Vec::new();
            for iteration in 0..1020 {
                let start = std::time::Instant::now();
                for parsed in &targets {
                    let result = if indexed {
                        resolve_target(std::hint::black_box(parsed), "source.md", &index)
                    } else {
                        scan_target(std::hint::black_box(parsed), "source.md", &records)
                    };
                    assert_eq!(result.1, MdbaseLinkResolution::Resolved);
                    std::hint::black_box(result);
                }
                if iteration >= 20 {
                    samples.push(start.elapsed().as_secs_f64() * 1_000_000.0);
                }
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "{}",
                serde_json::json!({"mode": if indexed {"indexed"} else {"linear_scan"}, "records": 10000, "targets_per_sample": 10, "samples": 1000, "warmup": 20, "unit": "microseconds", "p50": samples[499], "p95": samples[949], "p99": samples[989]})
            );
        }
    }

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
