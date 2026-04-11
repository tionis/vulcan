use crate::config::VaultConfig;
use crate::expression::eval::normalize_field_name;
use crate::parser::parse_document;
use crate::parser::types::InlineFieldKind;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContentTransformConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_callouts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_headings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_frontmatter_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_inline_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replace: Vec<ContentReplacementRuleConfig>,
}

impl ContentTransformConfig {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exclude_callouts.is_empty()
            && self.exclude_headings.is_empty()
            && self.exclude_frontmatter_keys.is_empty()
            && self.exclude_inline_fields.is_empty()
            && self
                .replace
                .iter()
                .all(ContentReplacementRuleConfig::is_empty)
    }

    pub fn merge_in(&mut self, other: &Self) {
        merge_string_lists(&mut self.exclude_callouts, &other.exclude_callouts);
        merge_string_lists(&mut self.exclude_headings, &other.exclude_headings);
        merge_string_lists(
            &mut self.exclude_frontmatter_keys,
            &other.exclude_frontmatter_keys,
        );
        merge_string_lists(
            &mut self.exclude_inline_fields,
            &other.exclude_inline_fields,
        );
        merge_replacement_rules(&mut self.replace, &other.replace);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContentReplacementRuleConfig {
    pub pattern: String,
    pub replacement: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub regex: bool,
}

impl ContentReplacementRuleConfig {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pattern.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContentTransformRuleConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_json: Option<String>,
    #[serde(flatten)]
    pub transforms: ContentTransformConfig,
}

impl ContentTransformRuleConfig {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.transforms.is_empty()
    }
}

#[must_use]
pub fn apply_content_transforms(source: &str, transforms: &ContentTransformConfig) -> String {
    let excluded_callouts = transforms
        .exclude_callouts
        .iter()
        .filter_map(|value| normalize_callout_name(value))
        .collect::<HashSet<_>>();
    let excluded_headings = transforms
        .exclude_headings
        .iter()
        .filter_map(|value| normalize_heading_name(value))
        .collect::<HashSet<_>>();
    let excluded_frontmatter_keys = transforms
        .exclude_frontmatter_keys
        .iter()
        .filter_map(|value| normalize_metadata_key(value))
        .collect::<HashSet<_>>();
    let excluded_inline_fields = transforms
        .exclude_inline_fields
        .iter()
        .filter_map(|value| normalize_metadata_key(value))
        .collect::<HashSet<_>>();

    let mut rendered = source.to_string();
    if !excluded_frontmatter_keys.is_empty() {
        rendered = strip_excluded_frontmatter_keys(&rendered, &excluded_frontmatter_keys);
    }
    if !excluded_headings.is_empty() {
        rendered = strip_excluded_headings(&rendered, &excluded_headings);
    }
    if !excluded_callouts.is_empty() {
        rendered = strip_excluded_callouts(&rendered, &excluded_callouts);
    }
    if !excluded_inline_fields.is_empty() {
        rendered = strip_excluded_inline_fields(&rendered, &excluded_inline_fields);
    }
    if !transforms.replace.is_empty() {
        rendered = apply_replacement_rules(&rendered, &transforms.replace);
    }
    rendered
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrontmatterBlock {
    full_start: usize,
    full_end: usize,
    yaml_start: usize,
    yaml_end: usize,
}

fn strip_excluded_frontmatter_keys(source: &str, excluded: &HashSet<String>) -> String {
    let Some(block) = find_frontmatter_block(source) else {
        return source.to_string();
    };

    let raw_yaml = &source[block.yaml_start..block.yaml_end];
    let Ok(frontmatter) = serde_yaml::from_str::<serde_yaml::Value>(raw_yaml) else {
        return source.to_string();
    };
    let Some(mapping) = frontmatter.as_mapping() else {
        return source.to_string();
    };

    let keys_to_remove = mapping
        .keys()
        .filter_map(serde_yaml::Value::as_str)
        .filter(|key| excluded.contains(&normalize_field_name(key)))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if keys_to_remove.is_empty() {
        return source.to_string();
    }

    let mut updated_yaml = raw_yaml.to_string();
    for key in keys_to_remove {
        let Some(next_yaml) = remove_yaml_key_block(&updated_yaml, &key) else {
            continue;
        };
        updated_yaml = next_yaml;
    }

    let replacement = if updated_yaml.trim().is_empty() {
        String::new()
    } else {
        format!("---\n{updated_yaml}---\n")
    };

    let mut rendered =
        String::with_capacity(source.len() + replacement.len().saturating_sub(block.full_end));
    rendered.push_str(&source[..block.full_start]);
    rendered.push_str(&replacement);
    rendered.push_str(&source[block.full_end..]);
    rendered
}

fn strip_excluded_headings(source: &str, excluded: &HashSet<String>) -> String {
    let parsed = parse_document(source, &VaultConfig::default());
    let mut ranges = Vec::<Range<usize>>::new();

    for (index, heading) in parsed.headings.iter().enumerate() {
        let Some(heading_name) = normalize_heading_name(&heading.text) else {
            continue;
        };
        if !excluded.contains(&heading_name) {
            continue;
        }

        let end = parsed
            .headings
            .iter()
            .skip(index + 1)
            .find(|candidate| candidate.level <= heading.level)
            .map_or(source.len(), |candidate| candidate.byte_offset);
        ranges.push(heading.byte_offset..end);
    }

    remove_byte_ranges(source, &ranges)
}

fn strip_excluded_inline_fields(source: &str, excluded: &HashSet<String>) -> String {
    let parsed = parse_document(source, &VaultConfig::default());
    let mut ranges = Vec::<Range<usize>>::new();

    for field in &parsed.inline_fields {
        if excluded.contains(&normalize_field_name(&field.key)) {
            ranges.push(expand_inline_field_removal_range(
                source,
                field.byte_range.clone(),
                field.kind,
            ));
        }
    }
    for task in &parsed.tasks {
        for field in &task.inline_fields {
            if excluded.contains(&normalize_field_name(&field.key)) {
                ranges.push(expand_inline_field_removal_range(
                    source,
                    field.byte_range.clone(),
                    field.kind,
                ));
            }
        }
    }

    remove_byte_ranges(source, &ranges)
}

fn strip_excluded_callouts(source: &str, excluded: &HashSet<String>) -> String {
    let mut rendered = String::with_capacity(source.len());
    let mut skipped_depth = None::<usize>;

    for line in source.split_inclusive('\n') {
        if let Some(depth) = skipped_depth {
            if blockquote_depth(line).is_some_and(|current| current >= depth) {
                continue;
            }
            skipped_depth = None;
        }

        let Some((depth, content)) = blockquote_depth_and_content(line) else {
            rendered.push_str(line);
            continue;
        };
        let Some(callout_name) = parse_callout_name(content) else {
            rendered.push_str(line);
            continue;
        };
        if excluded.contains(&callout_name) {
            skipped_depth = Some(depth);
            continue;
        }
        rendered.push_str(line);
    }

    rendered
}

fn blockquote_depth(line: &str) -> Option<usize> {
    blockquote_depth_and_content(line).map(|(depth, _)| depth)
}

fn blockquote_depth_and_content(line: &str) -> Option<(usize, &str)> {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }

    let mut depth = 0_usize;
    loop {
        if index >= bytes.len() || bytes[index] != b'>' {
            break;
        }
        depth += 1;
        index += 1;
        while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
            index += 1;
        }
    }

    (depth > 0).then_some((depth, &line[index..]))
}

fn parse_callout_name(content: &str) -> Option<String> {
    let trimmed = content.trim_start();
    let inner = trimmed.strip_prefix("[!")?;
    let end = inner.find(']')?;
    normalize_callout_name(&inner[..end])
}

fn normalize_callout_name(value: &str) -> Option<String> {
    let mut trimmed = value.trim();
    if let Some(inner) = trimmed
        .strip_prefix("[!")
        .and_then(|inner| inner.strip_suffix(']'))
    {
        trimmed = inner.trim();
    }
    if let Some(inner) = trimmed.strip_prefix('!') {
        trimmed = inner.trim();
    }
    if let Some(inner) = trimmed
        .strip_suffix('+')
        .or_else(|| trimmed.strip_suffix('-'))
    {
        trimmed = inner.trim_end();
    }

    let normalized = trimmed
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn normalize_heading_name(value: &str) -> Option<String> {
    let normalized = value
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn normalize_metadata_key(value: &str) -> Option<String> {
    let normalized = normalize_field_name(value);
    (!normalized.is_empty()).then_some(normalized)
}

fn merge_string_lists(target: &mut Vec<String>, values: &[String]) {
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() || target.iter().any(|existing| existing == trimmed) {
            continue;
        }
        target.push(trimmed.to_string());
    }
}

fn merge_replacement_rules(
    target: &mut Vec<ContentReplacementRuleConfig>,
    values: &[ContentReplacementRuleConfig],
) {
    target.extend(values.iter().filter(|rule| !rule.is_empty()).cloned());
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

fn expand_inline_field_removal_range(
    source: &str,
    range: Range<usize>,
    kind: InlineFieldKind,
) -> Range<usize> {
    if kind != InlineFieldKind::Bare {
        return range;
    }

    let line_start = source[..range.start]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line_end_without_newline = source[range.end..]
        .find('\n')
        .map_or(source.len(), |offset| range.end + offset);
    let line_end = if line_end_without_newline < source.len() {
        line_end_without_newline + 1
    } else {
        line_end_without_newline
    };

    if source[line_start..range.start].trim().is_empty()
        && source[range.end..line_end_without_newline]
            .trim()
            .is_empty()
    {
        line_start..line_end
    } else {
        range
    }
}

fn apply_replacement_rules(source: &str, rules: &[ContentReplacementRuleConfig]) -> String {
    let mut rendered = source.to_string();
    for rule in rules.iter().filter(|rule| !rule.is_empty()) {
        if rule.regex {
            let Ok(regex) = Regex::new(&rule.pattern) else {
                continue;
            };
            rendered = regex
                .replace_all(&rendered, rule.replacement.as_str())
                .into_owned();
        } else {
            rendered = rendered.replace(&rule.pattern, &rule.replacement);
        }
    }
    rendered
}

fn remove_byte_ranges(source: &str, ranges: &[Range<usize>]) -> String {
    if ranges.is_empty() {
        return source.to_string();
    }

    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|range| (range.start, range.end));

    let mut merged = Vec::<Range<usize>>::new();
    for range in sorted {
        match merged.last_mut() {
            Some(previous) if range.start <= previous.end => {
                previous.end = previous.end.max(range.end);
            }
            _ => merged.push(range),
        }
    }

    let mut rendered = String::with_capacity(source.len());
    let mut cursor = 0_usize;
    for range in merged {
        if cursor < range.start {
            rendered.push_str(&source[cursor..range.start]);
        }
        cursor = cursor.max(range.end);
    }
    if cursor < source.len() {
        rendered.push_str(&source[cursor..]);
    }
    rendered
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

fn trim_line(line: &str) -> &str {
    line.trim_end_matches('\n').trim_end_matches('\r')
}

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

fn find_yaml_key_span(yaml: &str, key: &str) -> Option<(usize, usize)> {
    let mut pos = 0usize;
    let mut span_start = None::<usize>;

    for line in yaml.split_inclusive('\n') {
        let line_start = pos;
        pos += line.len();
        let line_body = trim_line(line);

        if let Some(start) = span_start {
            if line_body.is_empty() {
                return Some((start, line_start));
            }
            let first_char = line.chars().next().unwrap_or(' ');
            if first_char == ' '
                || first_char == '\t'
                || line_body.starts_with("- ")
                || line_body == "-"
            {
            } else {
                return Some((start, line_start));
            }
        } else if is_top_level_key_line(line_body, key) {
            span_start = Some(line_start);
            let after_colon = line_body[key.len() + 1..].trim();
            if !after_colon.is_empty() {
                return Some((line_start, pos));
            }
        }
    }

    span_start.map(|start| (start, pos))
}

fn remove_yaml_key_block(yaml: &str, key: &str) -> Option<String> {
    let (span_start, span_end) = find_yaml_key_span(yaml, key)?;
    Some(format!("{}{}", &yaml[..span_start], &yaml[span_end..]))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_content_transforms, ContentReplacementRuleConfig, ContentTransformConfig,
        ContentTransformRuleConfig,
    };

    #[test]
    fn excludes_matching_callout_blocks() {
        let source = concat!(
            "# Home\n\n",
            "Visible paragraph.\n\n",
            "> [!secret gm]- Internal\n",
            "> Hidden details.\n",
            "> Hidden [[People/Bob]].\n\n",
            "After.\n",
        );

        let rendered = apply_content_transforms(
            source,
            &ContentTransformConfig {
                exclude_callouts: vec!["secret gm".to_string()],
                exclude_headings: Vec::new(),
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: Vec::new(),
                replace: Vec::new(),
            },
        );

        assert!(rendered.contains("Visible paragraph."));
        assert!(rendered.contains("After."));
        assert!(!rendered.contains("Hidden details."));
        assert!(!rendered.contains("[[People/Bob]]"));
    }

    #[test]
    fn nested_excluded_callout_resumes_outer_blockquote_content() {
        let source = concat!(
            "> [!note]\n",
            "> Keep this.\n",
            "> > [!secret gm]\n",
            "> > Hide this.\n",
            "> Back outside.\n",
            "After.\n",
        );

        let rendered = apply_content_transforms(
            source,
            &ContentTransformConfig {
                exclude_callouts: vec!["SECRET   GM".to_string()],
                exclude_headings: Vec::new(),
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: Vec::new(),
                replace: Vec::new(),
            },
        );

        assert!(rendered.contains("> [!note]"));
        assert!(rendered.contains("> Keep this."));
        assert!(rendered.contains("> Back outside."));
        assert!(rendered.contains("After."));
        assert!(!rendered.contains("Hide this."));
    }

    #[test]
    fn leaves_other_blockquotes_and_callouts_untouched() {
        let source = concat!("> plain quote\n\n", "> [!note]\n", "> keep\n",);
        let rendered = apply_content_transforms(
            source,
            &ContentTransformConfig {
                exclude_callouts: vec!["secret".to_string()],
                exclude_headings: Vec::new(),
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: Vec::new(),
                replace: Vec::new(),
            },
        );

        assert_eq!(rendered, source);
    }

    #[test]
    fn empty_configuration_is_a_no_op() {
        let source = "> [!secret]\n> Hidden.\n";
        let rendered = apply_content_transforms(source, &ContentTransformConfig::default());
        assert_eq!(rendered, source);
    }

    #[test]
    fn merge_in_unions_excluded_callouts_in_order() {
        let mut base = ContentTransformConfig {
            exclude_callouts: vec!["secret gm".to_string()],
            exclude_headings: vec!["scratch".to_string()],
            exclude_frontmatter_keys: vec!["api key".to_string()],
            exclude_inline_fields: vec!["owner".to_string()],
            replace: vec![ContentReplacementRuleConfig {
                pattern: "secret".to_string(),
                replacement: "[redacted]".to_string(),
                regex: false,
            }],
        };
        base.merge_in(&ContentTransformConfig {
            exclude_callouts: vec![
                "internal".to_string(),
                "secret gm".to_string(),
                "  ".to_string(),
            ],
            exclude_headings: vec!["private".to_string(), "scratch".to_string()],
            exclude_frontmatter_keys: vec!["email".to_string(), "api key".to_string()],
            exclude_inline_fields: vec!["budget".to_string(), "owner".to_string()],
            replace: vec![ContentReplacementRuleConfig {
                pattern: r"\b[A-Z0-9]{8}\b".to_string(),
                replacement: "[token]".to_string(),
                regex: true,
            }],
        });

        assert_eq!(
            base.exclude_callouts,
            vec!["secret gm".to_string(), "internal".to_string()]
        );
        assert_eq!(
            base.exclude_headings,
            vec!["scratch".to_string(), "private".to_string()]
        );
        assert_eq!(
            base.exclude_frontmatter_keys,
            vec!["api key".to_string(), "email".to_string()]
        );
        assert_eq!(
            base.exclude_inline_fields,
            vec!["owner".to_string(), "budget".to_string()]
        );
        assert_eq!(
            base.replace,
            vec![
                ContentReplacementRuleConfig {
                    pattern: "secret".to_string(),
                    replacement: "[redacted]".to_string(),
                    regex: false,
                },
                ContentReplacementRuleConfig {
                    pattern: r"\b[A-Z0-9]{8}\b".to_string(),
                    replacement: "[token]".to_string(),
                    regex: true,
                }
            ]
        );
    }

    #[test]
    fn rule_is_empty_only_when_it_has_no_effective_transforms() {
        assert!(ContentTransformRuleConfig::default().is_empty());
        assert!(!ContentTransformRuleConfig {
            query: Some("from notes".to_string()),
            query_json: None,
            transforms: ContentTransformConfig {
                exclude_callouts: vec!["secret".to_string()],
                exclude_headings: Vec::new(),
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: Vec::new(),
                replace: Vec::new(),
            },
        }
        .is_empty());
    }

    #[test]
    fn excludes_matching_heading_sections_and_nested_subsections() {
        let source = concat!(
            "# Home\n\n",
            "Visible paragraph.\n\n",
            "## Scratch\n\n",
            "Hidden [[People/Bob]].\n\n",
            "### Scratch child\n\n",
            "Still hidden.\n\n",
            "## Public\n\n",
            "Visible again.\n",
        );

        let rendered = apply_content_transforms(
            source,
            &ContentTransformConfig {
                exclude_callouts: Vec::new(),
                exclude_headings: vec!["scratch".to_string()],
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: Vec::new(),
                replace: Vec::new(),
            },
        );

        assert!(rendered.contains("# Home"));
        assert!(rendered.contains("Visible paragraph."));
        assert!(rendered.contains("## Public"));
        assert!(rendered.contains("Visible again."));
        assert!(!rendered.contains("## Scratch"));
        assert!(!rendered.contains("Scratch child"));
        assert!(!rendered.contains("[[People/Bob]]"));
    }

    #[test]
    fn excludes_setext_heading_sections() {
        let source = concat!(
            "Overview\n",
            "========\n\n",
            "Keep.\n\n",
            "Scratch\n",
            "-------\n\n",
            "Hidden details.\n\n",
            "Public\n",
            "------\n\n",
            "Visible.\n",
        );

        let rendered = apply_content_transforms(
            source,
            &ContentTransformConfig {
                exclude_callouts: Vec::new(),
                exclude_headings: vec!["scratch".to_string()],
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: Vec::new(),
                replace: Vec::new(),
            },
        );

        assert!(rendered.contains("Overview"));
        assert!(rendered.contains("Keep."));
        assert!(rendered.contains("Public"));
        assert!(rendered.contains("Visible."));
        assert!(!rendered.contains("Scratch\n-------"));
        assert!(!rendered.contains("Hidden details."));
    }

    #[test]
    fn excludes_matching_frontmatter_keys_and_frontmatter_links() {
        let source = concat!(
            "---\n",
            "Public: visible\n",
            "API Key: secret-value\n",
            "related: \"[[People/Bob]]\"\n",
            "---\n\n",
            "# Home\n",
        );

        let rendered = apply_content_transforms(
            source,
            &ContentTransformConfig {
                exclude_callouts: Vec::new(),
                exclude_headings: Vec::new(),
                exclude_frontmatter_keys: vec!["api-key".to_string(), "related".to_string()],
                exclude_inline_fields: Vec::new(),
                replace: Vec::new(),
            },
        );

        assert!(rendered.contains("Public: visible"));
        assert!(!rendered.contains("API Key"));
        assert!(!rendered.contains("[[People/Bob]]"));
        assert!(rendered.starts_with("---\nPublic: visible\n---\n"));
    }

    #[test]
    fn excludes_matching_inline_fields_from_body_and_tasks() {
        let source = concat!(
            "---\n",
            "public: visible\n",
            "---\n\n",
            "secret:: hidden\n",
            "Visible paragraph.\n",
            "Owner note [owner:: Alice]\n",
            "- [ ] Ship [due:: 2026-04-01] (private:: yes)\n",
        );

        let rendered = apply_content_transforms(
            source,
            &ContentTransformConfig {
                exclude_callouts: Vec::new(),
                exclude_headings: Vec::new(),
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: vec![
                    "secret".to_string(),
                    "due".to_string(),
                    "private".to_string(),
                ],
                replace: Vec::new(),
            },
        );

        assert!(!rendered.contains("secret:: hidden"));
        assert!(rendered.contains("Visible paragraph."));
        assert!(rendered.contains("Owner note [owner:: Alice]"));
        assert!(!rendered.contains("[due:: 2026-04-01]"));
        assert!(!rendered.contains("(private:: yes)"));
        assert!(rendered.contains("- [ ] Ship"));
    }

    #[test]
    fn applies_literal_and_regex_replacements_in_order() {
        let source = concat!(
            "---\n",
            "email: alpha@example.com\n",
            "---\n\n",
            "Visible [[People/Bob]].\n",
            "secret token\n",
        );

        let rendered = apply_content_transforms(
            source,
            &ContentTransformConfig {
                exclude_callouts: Vec::new(),
                exclude_headings: Vec::new(),
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: Vec::new(),
                replace: vec![
                    ContentReplacementRuleConfig {
                        pattern: "[[People/Bob]]".to_string(),
                        replacement: "[[People/Alice]]".to_string(),
                        regex: false,
                    },
                    ContentReplacementRuleConfig {
                        pattern: r"[A-Za-z0-9._%+-]+@example\.com".to_string(),
                        replacement: "[redacted]".to_string(),
                        regex: true,
                    },
                    ContentReplacementRuleConfig {
                        pattern: "secret".to_string(),
                        replacement: "public".to_string(),
                        regex: false,
                    },
                    ContentReplacementRuleConfig {
                        pattern: r"\bpublic token\b".to_string(),
                        replacement: "[token]".to_string(),
                        regex: true,
                    },
                ],
            },
        );

        assert!(rendered.contains("email: [redacted]"));
        assert!(rendered.contains("[[People/Alice]]"));
        assert!(!rendered.contains("[[People/Bob]]"));
        assert!(rendered.contains("[token]"));
        assert!(!rendered.contains("secret token"));
    }
}
