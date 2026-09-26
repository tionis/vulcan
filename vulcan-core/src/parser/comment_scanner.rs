use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};

/// Finds Obsidian `%%` comment regions.
///
/// `%%` is only a comment delimiter in ordinary Markdown text: markers inside code spans,
/// code blocks, math, and frontmatter are literal and never open or close a comment.
#[must_use]
pub fn scan_comment_regions(source: &str, options: Options) -> Vec<Range<usize>> {
    if !source.contains("%%") {
        return Vec::new();
    }

    let literal_regions = literal_regions(source, options);
    let bytes = source.as_bytes();
    let mut regions = Vec::new();
    let mut open_region = None;
    let mut literal_index = 0;
    let mut index = 0;

    while index + 1 < bytes.len() {
        while literal_index < literal_regions.len() && literal_regions[literal_index].end <= index {
            literal_index += 1;
        }
        if let Some(literal) = literal_regions.get(literal_index) {
            if literal.start <= index {
                index = literal.end;
                continue;
            }
        }

        if bytes[index] == b'%' && bytes[index + 1] == b'%' {
            if let Some(start) = open_region.take() {
                regions.push(start..index + 2);
            } else {
                open_region = Some(index);
            }
            index += 2;
        } else {
            index += 1;
        }
    }

    regions
}

/// Byte ranges whose content is literal for comment detection, sorted by start offset.
fn literal_regions(source: &str, options: Options) -> Vec<Range<usize>> {
    let mut regions = Parser::new_ext(source, options)
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::Start(Tag::CodeBlock(_) | Tag::MetadataBlock(_)) => Some(range),
            _ => None,
        })
        .collect::<Vec<_>>();
    regions.sort_by_key(|range| range.start);
    regions
}

#[must_use]
pub fn overlaps_comment(range: &Range<usize>, comment_regions: &[Range<usize>]) -> bool {
    comment_regions
        .iter()
        .any(|comment| comment.start < range.end && range.start < comment.end)
}

#[must_use]
pub fn visible_subranges(
    range: Range<usize>,
    comment_regions: &[Range<usize>],
) -> Vec<Range<usize>> {
    let mut visible = Vec::new();
    let mut cursor = range.start;

    for comment in comment_regions
        .iter()
        .filter(|comment| comment.start < range.end && range.start < comment.end)
    {
        if cursor < comment.start {
            visible.push(cursor..comment.start.min(range.end));
        }
        cursor = cursor.max(comment.end.min(range.end));
        if cursor >= range.end {
            break;
        }
    }

    if cursor < range.end {
        visible.push(cursor..range.end);
    }

    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parser_options;

    fn scan(source: &str) -> Vec<Range<usize>> {
        scan_comment_regions(source, parser_options())
    }

    #[test]
    fn paired_comments_are_recorded() {
        let source = "alpha %%secret%% beta";

        assert_eq!(scan(source), vec![6..16]);
    }

    #[test]
    fn nested_markers_pair_linearly() {
        let source = "%%outer %% inner %%";

        assert_eq!(scan(source), vec![0..10]);
    }

    #[test]
    fn unclosed_comments_are_treated_as_literal_text() {
        let source = "alpha %%secret";

        assert!(scan(source).is_empty());
    }

    #[test]
    fn adjacent_comments_are_recorded_as_single_region() {
        let source = "%%%%";

        assert_eq!(scan(source), vec![0..4]);
    }

    #[test]
    fn markers_inside_code_are_literal() {
        let source = concat!(
            "```sql\nSELECT 1 WHERE a LIKE '%%x';\n```\n\n",
            "Visible `50%%` text.\n\n",
            "%%secret%%\n",
        );
        let start = source.find("%%secret").expect("comment should exist");

        assert_eq!(scan(source), vec![start..start + 10]);
    }

    #[test]
    fn markers_inside_frontmatter_and_math_are_literal() {
        let source = "---\nformat: \"%%\"\n---\n$a %% b$ then %%hidden%%";
        let start = source.find("%%hidden").expect("comment should exist");

        assert_eq!(scan(source), vec![start..start + 10]);
    }

    #[test]
    fn comments_may_span_code_blocks() {
        let source = "%%\n```\ncode\n```\n%%\nafter";

        assert_eq!(scan(source), vec![0..source.find("\nafter").unwrap()]);
    }
}
