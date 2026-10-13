//! Semantic tokens: vault-aware highlighting of links, embeds, tags, inline
//! fields, and block ids, with unresolved and ambiguous links marked.
//!
//! Syntax highlighters already colour Markdown; these tokens add what only
//! the vault knows, such as whether a link resolves.

use super::{EditorRange, EditorSession};
use crate::AppError;
use std::ops::Range;
use vulcan_core::editor::{block_id_range, link_range, tag_range};
use vulcan_core::resolver::LinkResolutionProblem;
use vulcan_core::LinkKind;

/// What a semantic token marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorTokenKind {
    WikiLink,
    MarkdownLink,
    Embed,
    Tag,
    /// The key of an inline `key:: value` field.
    InlineField,
    BlockId,
}

impl EditorTokenKind {
    /// Every kind, in legend order.
    pub const ALL: [Self; 6] = [
        Self::WikiLink,
        Self::MarkdownLink,
        Self::Embed,
        Self::Tag,
        Self::InlineField,
        Self::BlockId,
    ];

    /// The token type name editors see.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::WikiLink => "wikiLink",
            Self::MarkdownLink => "markdownLink",
            Self::Embed => "embed",
            Self::Tag => "tag",
            Self::InlineField => "inlineField",
            Self::BlockId => "blockId",
        }
    }
}

/// One token, always within a single line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorSemanticToken {
    pub range: EditorRange,
    pub kind: EditorTokenKind,
    /// A link whose target does not exist.
    pub unresolved: bool,
    /// A link that matches several notes.
    pub ambiguous: bool,
}

impl EditorSession {
    /// Semantic tokens for an open document in source order, or `None` when
    /// it is not open. Links are only marked unresolved or ambiguous once
    /// the vault is indexed.
    pub fn semantic_tokens(
        &self,
        path: &str,
    ) -> Result<Option<Vec<EditorSemanticToken>>, AppError> {
        let Some(document) = self.document(path) else {
            return Ok(None);
        };
        let text = &document.text;
        let parsed = &document.parsed;
        let resolver = if self.has_cache() {
            Some(self.resolver()?)
        } else {
            None
        };

        let mut spans: Vec<(Range<usize>, EditorTokenKind, bool, bool)> = Vec::new();
        for link in &parsed.links {
            let kind = match link.link_kind {
                LinkKind::Wikilink => EditorTokenKind::WikiLink,
                LinkKind::Markdown => EditorTokenKind::MarkdownLink,
                LinkKind::Embed => EditorTokenKind::Embed,
                LinkKind::External => continue,
            };
            let range = link_range(link);
            if text.get(range.clone()) != Some(link.raw_text.as_str()) {
                continue;
            }
            let problem = resolver.as_ref().and_then(|resolver| {
                resolver
                    .resolve(path, link, self.config().link_resolution)
                    .problem
            });
            spans.push((
                range,
                kind,
                matches!(problem, Some(LinkResolutionProblem::Unresolved)),
                matches!(problem, Some(LinkResolutionProblem::Ambiguous(_))),
            ));
        }
        for tag in &parsed.tags {
            if let Some(range) = tag_range(text, tag) {
                spans.push((range, EditorTokenKind::Tag, false, false));
            }
        }
        for field in &parsed.inline_fields {
            let Some(start) = text
                .get(field.byte_range.clone())
                .and_then(|source| source.find(field.key.as_str()))
                .map(|offset| field.byte_range.start + offset)
            else {
                continue;
            };
            spans.push((
                start..start + field.key.len(),
                EditorTokenKind::InlineField,
                false,
                false,
            ));
        }
        for block_ref in &parsed.block_refs {
            let range = block_id_range(text, block_ref);
            if text
                .get(range.clone())
                .is_some_and(|id| id.starts_with('^'))
            {
                spans.push((range, EditorTokenKind::BlockId, false, false));
            }
        }

        // Editors expect tokens in order and without overlaps.
        spans.sort_by_key(|(range, ..)| (range.start, range.end));
        let mut tokens = Vec::with_capacity(spans.len());
        let mut covered = 0;
        for (range, kind, unresolved, ambiguous) in spans {
            if range.start < covered || range.is_empty() {
                continue;
            }
            covered = range.end;
            // Tokens may not span lines (without a client capability few
            // editors have), so a link wrapped across lines becomes one token
            // per line.
            for piece in line_pieces(text, range) {
                tokens.push(EditorSemanticToken {
                    range: self.range(text, &document.line_index, piece),
                    kind,
                    unresolved,
                    ambiguous,
                });
            }
        }
        Ok(Some(tokens))
    }
}

/// `range` split at line breaks, without the breaks and empty pieces.
fn line_pieces(text: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let mut pieces = Vec::new();
    let mut start = range.start;
    for (offset, _) in text[range.clone()].match_indices('\n') {
        let end = range.start + offset;
        let line_end = if text[..end].ends_with('\r') {
            end - 1
        } else {
            end
        };
        if line_end > start {
            pieces.push(start..line_end);
        }
        start = end + 1;
    }
    if range.end > start {
        pieces.push(start..range.end);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::EditorTokenKind;

    #[test]
    fn marks_links_tags_fields_and_block_ids() {
        let text = "See [[Home]] and [[Gone]], ![[Home]], [md](Home.md).\n\nstatus:: open\n\nA #tag here ^block1\n\n[ext](https://example.com)\n";
        let (_dir, mut session) = vault(&[("Home.md", "# Home\n"), ("Note.md", text)]);
        session.open("Note.md", 1, text.into());
        let tokens = session.semantic_tokens("Note.md").unwrap().unwrap();
        let summary = tokens
            .iter()
            .map(|token| (token.kind, token.unresolved, token.range.start))
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            vec![
                (
                    EditorTokenKind::WikiLink,
                    false,
                    position_of(text, "[[Home]]", 0)
                ),
                (
                    EditorTokenKind::WikiLink,
                    true,
                    position_of(text, "[[Gone]]", 0)
                ),
                (EditorTokenKind::Embed, false, position_of(text, "![[", 0)),
                (
                    EditorTokenKind::MarkdownLink,
                    false,
                    position_of(text, "[md]", 0)
                ),
                (
                    EditorTokenKind::InlineField,
                    false,
                    position_of(text, "status", 0)
                ),
                (EditorTokenKind::Tag, false, position_of(text, "#tag", 0)),
                (
                    EditorTokenKind::BlockId,
                    false,
                    position_of(text, "^block1", 0)
                ),
            ]
        );
        // The inline field token covers the key only.
        let field = &tokens[4];
        assert_eq!(field.range.end.column - field.range.start.column, 6);
        assert!(session.semantic_tokens("Missing.md").unwrap().is_none());
    }

    #[test]
    fn splits_links_wrapped_across_lines() {
        let text = "See [a wrapped\r\nlabel](Gone.md) here\n";
        let (_dir, mut session) = vault(&[("Note.md", text)]);
        session.open("Note.md", 1, text.into());
        let tokens = session.semantic_tokens("Note.md").unwrap().unwrap();
        let ranges = tokens
            .iter()
            .map(|token| (token.range.start, token.range.end, token.unresolved))
            .collect::<Vec<_>>();
        assert_eq!(
            ranges,
            vec![
                (
                    position_of(text, "[a", 0),
                    position_of(text, "\r\n", 0),
                    true
                ),
                (
                    position_of(text, "label", 0),
                    position_of(text, " here", 0),
                    true
                ),
            ]
        );
    }

    #[test]
    fn marks_ambiguous_links() {
        let text = "[[Same]]\n";
        let (_dir, mut session) = vault(&[
            ("a/Same.md", "a\n"),
            ("b/Same.md", "b\n"),
            ("Note.md", text),
        ]);
        session.open("Note.md", 1, text.into());
        let tokens = session.semantic_tokens("Note.md").unwrap().unwrap();
        assert_eq!(tokens.len(), 1);
        assert!(tokens[0].ambiguous && !tokens[0].unresolved);
    }
}
