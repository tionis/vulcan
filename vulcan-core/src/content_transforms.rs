use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContentTransformConfig {
    #[serde(default)]
    pub exclude_callouts: Vec<String>,
}

impl ContentTransformConfig {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exclude_callouts.is_empty()
    }

    pub fn merge_in(&mut self, other: &Self) {
        merge_string_lists(&mut self.exclude_callouts, &other.exclude_callouts);
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
    let excluded = transforms
        .exclude_callouts
        .iter()
        .filter_map(|value| normalize_callout_name(value))
        .collect::<HashSet<_>>();
    if excluded.is_empty() {
        return source.to_string();
    }
    strip_excluded_callouts(source, &excluded)
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

fn merge_string_lists(target: &mut Vec<String>, values: &[String]) {
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() || target.iter().any(|existing| existing == trimmed) {
            continue;
        }
        target.push(trimmed.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_content_transforms, ContentTransformConfig, ContentTransformRuleConfig};

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
        };
        base.merge_in(&ContentTransformConfig {
            exclude_callouts: vec![
                "internal".to_string(),
                "secret gm".to_string(),
                "  ".to_string(),
            ],
        });

        assert_eq!(
            base.exclude_callouts,
            vec!["secret gm".to_string(), "internal".to_string()]
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
            },
        }
        .is_empty());
    }
}
