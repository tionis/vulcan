//! Locate the semantic element under an editor cursor.
//!
//! Lookups work on a parsed document and the exact source it was parsed
//! from, so they serve unsaved editor buffers as well as files on disk.

use crate::parser::{
    OriginContext, ParsedDocument, RawBlockRef, RawHeading, RawInlineField, RawLink, RawTag,
};
use std::ops::Range;

/// A semantic element and the source byte range it occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorElement<'a> {
    /// A link or embed, in the body or in frontmatter.
    Link {
        index: usize,
        link: &'a RawLink,
        range: Range<usize>,
    },
    /// An inline `#tag`. Tags listed in frontmatter have no source position.
    Tag {
        tag: &'a RawTag,
        range: Range<usize>,
    },
    /// A heading line.
    Heading {
        index: usize,
        heading: &'a RawHeading,
        range: Range<usize>,
    },
    /// A `^block-id` label.
    BlockId {
        block_ref: &'a RawBlockRef,
        range: Range<usize>,
    },
    /// An inline `key:: value` field.
    InlineField {
        field: &'a RawInlineField,
        range: Range<usize>,
    },
}

impl EditorElement<'_> {
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        match self {
            Self::Link { range, .. }
            | Self::Tag { range, .. }
            | Self::Heading { range, .. }
            | Self::BlockId { range, .. }
            | Self::InlineField { range, .. } => range.clone(),
        }
    }
}

/// The source range of a link, including its brackets.
#[must_use]
pub fn link_range(link: &RawLink) -> Range<usize> {
    link.byte_offset..link.byte_offset + link.raw_text.len()
}

/// The source range of a heading line, without its line terminator.
#[must_use]
pub fn heading_range(source: &str, heading: &RawHeading) -> Range<usize> {
    let start = heading.byte_offset.min(source.len());
    let end = source[start..]
        .find('\n')
        .map_or(source.len(), |newline| start + newline);
    start..source[start..end].trim_end_matches('\r').len() + start
}

/// The source range of a `^block-id` label, including the caret.
#[must_use]
pub fn block_id_range(source: &str, block_ref: &RawBlockRef) -> Range<usize> {
    let start = source
        .get(block_ref.block_id_byte_offset..)
        .and_then(|rest| rest.find('^'))
        .map_or(block_ref.block_id_byte_offset, |caret| {
            block_ref.block_id_byte_offset + caret
        });
    start..(start + 1 + block_ref.block_id_text.len()).min(source.len())
}

/// The source range of an inline tag, if it has one.
#[must_use]
pub fn tag_range(source: &str, tag: &RawTag) -> Option<Range<usize>> {
    let end = tag.byte_offset + 1 + tag.tag_text.len();
    let text = source.get(tag.byte_offset..end)?;
    (text.starts_with('#') && text[1..] == tag.tag_text).then_some(tag.byte_offset..end)
}

/// Find the innermost element whose range contains `offset`.
///
/// A cursor directly after an element (at its end) still selects it, so a
/// cursor placed after `]]` resolves the link it closes.
#[must_use]
pub fn element_at<'a>(
    source: &str,
    parsed: &'a ParsedDocument,
    offset: usize,
) -> Option<EditorElement<'a>> {
    let contains = |range: &Range<usize>| range.start <= offset && offset <= range.end;

    for (index, link) in parsed.links.iter().enumerate() {
        let range = link_range(link);
        if contains(&range) {
            return Some(EditorElement::Link { index, link, range });
        }
    }
    for tag in &parsed.tags {
        if let Some(range) = tag_range(source, tag).filter(&contains) {
            return Some(EditorElement::Tag { tag, range });
        }
    }
    for block_ref in &parsed.block_refs {
        let range = block_id_range(source, block_ref);
        if contains(&range) {
            return Some(EditorElement::BlockId { block_ref, range });
        }
    }
    for field in &parsed.inline_fields {
        if contains(&field.byte_range) {
            return Some(EditorElement::InlineField {
                field,
                range: field.byte_range.clone(),
            });
        }
    }
    for (index, heading) in parsed.headings.iter().enumerate() {
        let range = heading_range(source, heading);
        if contains(&range) {
            return Some(EditorElement::Heading {
                index,
                heading,
                range,
            });
        }
    }
    None
}

/// Whether a link was written in frontmatter rather than the body.
#[must_use]
pub fn is_frontmatter_link(link: &RawLink) -> bool {
    !matches!(link.origin_context, OriginContext::Body)
}

/// What the text before a cursor is in the middle of typing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionContext {
    /// Inside `[[...` (or `![[...`) before any `#` or `|`. `query` is the
    /// text typed so far, replaced through `range`.
    LinkTarget { query: String, range: Range<usize> },
    /// Inside `[[note#...`: a heading of `note` (empty for the current note).
    LinkHeading {
        note: String,
        query: String,
        range: Range<usize>,
    },
    /// Inside `[[note#^...`: a block id of `note` (empty for the current note).
    LinkBlock {
        note: String,
        query: String,
        range: Range<usize>,
    },
    /// After `#` in body text. `range` covers the tag text without `#`.
    Tag { query: String, range: Range<usize> },
    /// A frontmatter key being typed at the start of a line.
    PropertyKey { query: String, range: Range<usize> },
    /// A frontmatter value after `key:`.
    PropertyValue {
        key: String,
        query: String,
        range: Range<usize>,
    },
}

impl CompletionContext {
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        match self {
            Self::LinkTarget { range, .. }
            | Self::LinkHeading { range, .. }
            | Self::LinkBlock { range, .. }
            | Self::Tag { range, .. }
            | Self::PropertyKey { range, .. }
            | Self::PropertyValue { range, .. } => range.clone(),
        }
    }
}

/// Classify the text before `offset` for completion.
#[must_use]
pub fn completion_context(source: &str, offset: usize) -> Option<CompletionContext> {
    let offset = floor_boundary(source, offset.min(source.len()));
    let line_start = source[..offset]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let before = &source[line_start..offset];

    if let Some(frontmatter_end) = frontmatter_body_end(source) {
        if line_start > 0 && offset <= frontmatter_end {
            return property_context(before, line_start);
        }
    }

    if let Some(context) = link_context(before, line_start) {
        return Some(context);
    }
    tag_context(before, line_start)
}

fn link_context(before: &str, line_start: usize) -> Option<CompletionContext> {
    let open = before.rfind("[[")?;
    let typed = &before[open + 2..];
    if typed.contains("]]") || typed.contains('|') {
        return None;
    }
    let typed_start = line_start + open + 2;
    let end = typed_start + typed.len();
    if let Some(hash) = typed.find('#') {
        let note = typed[..hash].to_string();
        let rest = &typed[hash + 1..];
        if let Some(block) = rest.strip_prefix('^') {
            let start = typed_start + hash + 2;
            return Some(CompletionContext::LinkBlock {
                note,
                query: block.to_string(),
                range: start..end,
            });
        }
        let start = typed_start + hash + 1;
        return Some(CompletionContext::LinkHeading {
            note,
            query: rest.to_string(),
            range: start..end,
        });
    }
    Some(CompletionContext::LinkTarget {
        query: typed.to_string(),
        range: typed_start..end,
    })
}

fn tag_context(before: &str, line_start: usize) -> Option<CompletionContext> {
    let tag_start = before
        .char_indices()
        .rev()
        .take_while(|(_, character)| is_tag_char(*character))
        .last()
        .map_or(before.len(), |(index, _)| index);
    let hash = tag_start.checked_sub(1)?;
    if before.as_bytes()[hash] != b'#' {
        return None;
    }
    let preceding = before[..hash].chars().next_back();
    if preceding.is_some_and(|character| is_tag_char(character) || character == '#') {
        return None;
    }
    let query = &before[tag_start..];
    // `# ` at the start of a line is a heading, and `#1984` is not a tag.
    if !query.is_empty() && query.chars().all(|c| c.is_numeric() || c == '/') {
        return None;
    }
    Some(CompletionContext::Tag {
        query: query.to_string(),
        range: line_start + tag_start..line_start + before.len(),
    })
}

fn property_context(before: &str, line_start: usize) -> Option<CompletionContext> {
    if before.starts_with([' ', '\t', '-']) {
        return None;
    }
    if let Some(colon) = before.find(':') {
        let key = before[..colon].trim().to_string();
        let value = &before[colon + 1..];
        let value_start = colon + 1 + (value.len() - value.trim_start().len());
        let query = &before[value_start..];
        if key.is_empty() || query.starts_with(['[', '{', '"', '\'']) {
            return None;
        }
        return Some(CompletionContext::PropertyValue {
            key,
            query: query.to_string(),
            range: line_start + value_start..line_start + before.len(),
        });
    }
    Some(CompletionContext::PropertyKey {
        query: before.to_string(),
        range: line_start..line_start + before.len(),
    })
}

/// The byte offset where the frontmatter's closing `---` line starts.
fn frontmatter_body_end(source: &str) -> Option<usize> {
    let rest = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))?;
    let opening = source.len() - rest.len();
    let mut position = opening;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Some(position);
        }
        position += line.len();
    }
    None
}

fn is_tag_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '/' | '_' | '-')
}

fn floor_boundary(text: &str, mut offset: usize) -> usize {
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_document, VaultConfig};

    const SOURCE: &str = concat!(
        "---\n",
        "aliases: [Home]\n",
        "related: \"[[Projects/Alpha]]\"\n",
        "---\n",
        "# Title\n",
        "\n",
        "See [[Beta#Status|beta]] and #project/x here. ^para\n",
        "\n",
        "due:: 2026-10-12\n",
    );

    fn parsed() -> ParsedDocument {
        parse_document(SOURCE, &VaultConfig::default())
    }

    fn at(needle: &str, shift: usize) -> usize {
        SOURCE.find(needle).unwrap() + shift
    }

    #[test]
    fn finds_links_including_frontmatter_links() {
        let parsed = parsed();
        let Some(EditorElement::Link { link, range, .. }) =
            element_at(SOURCE, &parsed, at("Beta#", 2))
        else {
            panic!("expected link");
        };
        assert_eq!(&SOURCE[range], "[[Beta#Status|beta]]");
        assert_eq!(link.target_heading.as_deref(), Some("Status"));

        let Some(EditorElement::Link { link, range, .. }) =
            element_at(SOURCE, &parsed, at("Projects/Alpha", 3))
        else {
            panic!("expected frontmatter link");
        };
        assert_eq!(&SOURCE[range], "[[Projects/Alpha]]");
        assert!(is_frontmatter_link(link));
    }

    #[test]
    fn finds_tags_headings_block_ids_and_inline_fields() {
        let parsed = parsed();
        let element = element_at(SOURCE, &parsed, at("#project", 3)).unwrap();
        assert!(matches!(element, EditorElement::Tag { .. }));
        assert_eq!(&SOURCE[element.range()], "#project/x");

        let element = element_at(SOURCE, &parsed, at("# Title", 4)).unwrap();
        assert!(matches!(element, EditorElement::Heading { .. }));
        assert_eq!(&SOURCE[element.range()], "# Title");

        let element = element_at(SOURCE, &parsed, at("^para", 1)).unwrap();
        assert!(matches!(element, EditorElement::BlockId { .. }));
        assert_eq!(&SOURCE[element.range()], "^para");

        let element = element_at(SOURCE, &parsed, at("due::", 1)).unwrap();
        assert!(matches!(element, EditorElement::InlineField { .. }));

        assert_eq!(element_at(SOURCE, &parsed, at("here", 1)), None);
    }

    #[test]
    fn frontmatter_tags_have_no_source_range() {
        let source = "---\ntags: [alpha]\n---\nalpha\n";
        let parsed = parse_document(source, &VaultConfig::default());
        assert!(parsed
            .tags
            .iter()
            .all(|tag| tag_range(source, tag).is_none()));
    }

    #[test]
    fn classifies_link_completion() {
        let text = "x [[Proj";
        assert_eq!(
            completion_context(text, text.len()),
            Some(CompletionContext::LinkTarget {
                query: "Proj".into(),
                range: 4..8
            })
        );
        let text = "x ![[Beta#Sta";
        assert_eq!(
            completion_context(text, text.len()),
            Some(CompletionContext::LinkHeading {
                note: "Beta".into(),
                query: "Sta".into(),
                range: 10..13
            })
        );
        let text = "[[#^bl";
        assert_eq!(
            completion_context(text, text.len()),
            Some(CompletionContext::LinkBlock {
                note: String::new(),
                query: "bl".into(),
                range: 4..6
            })
        );
        assert_eq!(completion_context("[[a|disp", 8), None);
        assert_eq!(completion_context("[[a]] b", 7), None);
    }

    #[test]
    fn classifies_tag_completion() {
        assert_eq!(
            completion_context("see #pro", 8),
            Some(CompletionContext::Tag {
                query: "pro".into(),
                range: 5..8
            })
        );
        assert!(matches!(
            completion_context("#", 1),
            Some(CompletionContext::Tag { .. })
        ));
        assert_eq!(completion_context("a#pro", 5), None);
        assert_eq!(completion_context("#1984", 5), None);
        assert_eq!(completion_context("## He", 5), None);
    }

    #[test]
    fn classifies_frontmatter_completion() {
        let source = "---\nsta\nstatus: dr\n  - x\n---\nbody #t\n";
        let key = source.find("sta\n").unwrap() + 3;
        assert_eq!(
            completion_context(source, key),
            Some(CompletionContext::PropertyKey {
                query: "sta".into(),
                range: key - 3..key
            })
        );
        let value = source.find("dr").unwrap() + 2;
        assert_eq!(
            completion_context(source, value),
            Some(CompletionContext::PropertyValue {
                key: "status".into(),
                query: "dr".into(),
                range: value - 2..value
            })
        );
        assert_eq!(
            completion_context(source, source.find("x\n").unwrap() + 1),
            None
        );
        // The opening delimiter line is not a key.
        assert_eq!(completion_context(source, 3), None);
        let body = source.find("#t").unwrap() + 2;
        assert!(matches!(
            completion_context(source, body),
            Some(CompletionContext::Tag { .. })
        ));
    }
}
