mod block_ref;
mod comment_scanner;
mod dataview;
mod link_classifier;
mod options;
mod semantic_pass;
mod tag_extractor;
pub mod types;

pub use types::{
    ChunkText, LinkKind, OriginContext, ParseDiagnostic, ParseDiagnosticKind, ParsedDocument,
    RawBlockRef, RawDataviewBlock, RawHeading, RawInlineExpression, RawInlineField, RawLink,
    RawListItem, RawTag, RawTask, RawTaskField,
};

use crate::config::VaultConfig;
use comment_scanner::scan_comment_regions;
use options::{fragment_parser_options, parser_options};
use pulldown_cmark::Parser;
use semantic_pass::process_events;

#[must_use]
pub fn parse_document(source: &str, config: &VaultConfig) -> ParsedDocument {
    parse_document_internal(source, config, true)
}

#[must_use]
pub(crate) fn parse_document_fragment(
    source: &str,
    config: &VaultConfig,
    base_offset: usize,
) -> ParsedDocument {
    let mut parsed = parse_document_internal(source, config, false);
    parsed.inline_fields.clear();
    parsed.list_items.clear();
    parsed.tasks.clear();
    parsed.dataview_blocks.clear();
    parsed.inline_expressions.clear();
    shift_parsed_document_offsets(&mut parsed, base_offset);
    parsed
}

fn parse_document_internal(
    source: &str,
    config: &VaultConfig,
    include_metadata_blocks: bool,
) -> ParsedDocument {
    let comment_regions = scan_comment_regions(source);
    let options = if include_metadata_blocks {
        parser_options()
    } else {
        fragment_parser_options()
    };
    let parser = Parser::new_ext(source, options).into_offset_iter();
    process_events(source, config, &comment_regions, parser)
}

fn shift_parsed_document_offsets(parsed: &mut ParsedDocument, base_offset: usize) {
    for heading in &mut parsed.headings {
        heading.byte_offset += base_offset;
    }
    for block_ref in &mut parsed.block_refs {
        block_ref.block_id_byte_offset += base_offset;
        block_ref.target_block_byte_start += base_offset;
        block_ref.target_block_byte_end += base_offset;
    }
    for link in &mut parsed.links {
        link.byte_offset += base_offset;
    }
    for tag in &mut parsed.tags {
        tag.byte_offset += base_offset;
    }
    for inline_field in &mut parsed.inline_fields {
        inline_field.byte_range.start += base_offset;
        inline_field.byte_range.end += base_offset;
        inline_field.value_byte_range.start += base_offset;
        inline_field.value_byte_range.end += base_offset;
    }
    for list_item in &mut parsed.list_items {
        list_item.byte_offset += base_offset;
    }
    for task in &mut parsed.tasks {
        task.byte_offset += base_offset;
        for inline_field in &mut task.inline_fields {
            inline_field.byte_range.start += base_offset;
            inline_field.byte_range.end += base_offset;
            inline_field.value_byte_range.start += base_offset;
            inline_field.value_byte_range.end += base_offset;
        }
    }
    for block in &mut parsed.dataview_blocks {
        block.byte_range.start += base_offset;
        block.byte_range.end += base_offset;
    }
    for expression in &mut parsed.inline_expressions {
        expression.byte_range.start += base_offset;
        expression.byte_range.end += base_offset;
    }
    for chunk in &mut parsed.chunk_texts {
        chunk.byte_offset_start += base_offset;
        chunk.byte_offset_end += base_offset;
    }
    for diagnostic in &mut parsed.diagnostics {
        if let Some(range) = diagnostic.byte_range.as_mut() {
            range.start += base_offset;
            range.end += base_offset;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChunkingConfig, ChunkingStrategy};
    use crate::config::{LinkResolutionMode, LinkStylePreference};

    #[test]
    fn parses_links_embeds_and_subpaths() {
        let parsed = parse_document(
            "See [[Note]], [[Note|Display]], [Doc](docs/doc.md#Section), ![[image.png]], ![[Note#^block]], [Open](obsidian://open?vault=V&file=N).",
            &VaultConfig::default(),
        );

        assert_eq!(parsed.links.len(), 6);
        assert_eq!(parsed.links[0].link_kind, LinkKind::Wikilink);
        assert_eq!(
            parsed.links[0].target_path_candidate.as_deref(),
            Some("Note")
        );
        assert_eq!(parsed.links[0].display_text, None);
        assert_eq!(parsed.links[1].display_text.as_deref(), Some("Display"));
        assert_eq!(parsed.links[2].link_kind, LinkKind::Markdown);
        assert_eq!(parsed.links[2].target_heading.as_deref(), Some("Section"));
        assert_eq!(parsed.links[3].link_kind, LinkKind::Embed);
        assert!(!parsed.links[3].is_note_embed);
        assert_eq!(parsed.links[4].target_block.as_deref(), Some("block"));
        assert!(parsed.links[4].is_note_embed);
        assert_eq!(parsed.links[5].link_kind, LinkKind::External);
    }

    #[test]
    fn frontmatter_aliases_and_tags_are_extracted() {
        let parsed = parse_document(
            "---\naliases:\n  - One\ntags:\n  - project\n  - work\n---\n\n# Note\nBody",
            &VaultConfig::default(),
        );

        assert_eq!(parsed.aliases, vec!["One".to_string()]);
        assert_eq!(
            parsed
                .tags
                .iter()
                .map(|tag| tag.tag_text.clone())
                .collect::<Vec<_>>(),
            vec!["project".to_string(), "work".to_string()]
        );
        assert_eq!(parsed.headings[0].text, "Note");
        assert_eq!(parsed.chunk_texts.len(), 1);
    }

    #[test]
    fn frontmatter_links_use_property_context_and_preserve_source_offsets() {
        let source = concat!(
            "---\n",
            "related:\n",
            "  - \"[[Projects/Alpha]]\"\n",
            "reference: \"[Alpha doc](Projects/Alpha.md#Status)\"\n",
            "---\n",
            "\n",
            "# Note\n",
        );
        let parsed = parse_document(source, &VaultConfig::default());

        assert_eq!(parsed.links.len(), 2);
        assert!(parsed
            .links
            .iter()
            .all(|link| link.origin_context == OriginContext::Property));
        assert_eq!(parsed.links[0].raw_text, "[[Projects/Alpha]]");
        assert_eq!(
            parsed.links[1].raw_text,
            "[Alpha doc](Projects/Alpha.md#Status)"
        );
        for link in &parsed.links {
            let end = link.byte_offset + link.raw_text.len();
            assert_eq!(&source[link.byte_offset..end], link.raw_text);
        }
    }

    #[test]
    fn malformed_frontmatter_emits_diagnostic() {
        let parsed = parse_document("---\na: [\n---\nBody", &VaultConfig::default());

        assert!(parsed.frontmatter.is_none());
        assert_eq!(parsed.diagnostics.len(), 1);
        assert_eq!(
            parsed.diagnostics[0].kind,
            ParseDiagnosticKind::MalformedFrontmatter
        );
    }

    #[test]
    fn frontmatter_repairs_unindented_markdown_lines_in_block_scalars() {
        let source = concat!(
            "---\n",
            "title: Panam Palmer\n",
            "cardFirstMessage: |-\n",
            "  **Panam:** First line.\n",
            "![AnyText or No Text](assets/example.webp)\"Come ooonnn babe\"\n",
            "cardSummary: Summary\n",
            "---\n",
            "\n",
            "# Panam Palmer Card\n",
        );
        let parsed = parse_document(source, &VaultConfig::default());

        assert!(parsed.frontmatter.is_some());
        assert!(!parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ParseDiagnosticKind::MalformedFrontmatter));
        let frontmatter = parsed.frontmatter.expect("frontmatter should parse");
        assert!(frontmatter["cardFirstMessage"]
            .as_str()
            .expect("block scalar should parse to text")
            .contains("![AnyText or No Text](assets/example.webp)"));
    }

    #[test]
    fn frontmatter_repairs_crlf_block_scalar_continuations() {
        let source = concat!(
            "---\n",
            "title: Panam\n",
            "cardPersonality: |-\n",
            "  Intro line.\n",
            "  [Open: image](assets/example.gif)\r\n",
            "![Tumblr](assets/example.gif) @laezels on Tumblr\n",
            "cardScenario: Scenario text\n",
            "---\n",
            "\n",
            "# Panam\n",
        );
        let parsed = parse_document(source, &VaultConfig::default());

        assert!(parsed.frontmatter.is_some());
        assert!(!parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ParseDiagnosticKind::MalformedFrontmatter));
        let frontmatter = parsed.frontmatter.expect("frontmatter should parse");
        assert!(frontmatter["cardPersonality"]
            .as_str()
            .expect("block scalar should parse to text")
            .contains("![Tumblr](assets/example.gif)"));
    }

    #[test]
    fn non_leading_metadata_block_is_treated_as_body_content() {
        let source = concat!(
            "---\n",
            "title: Session\n",
            "---\n",
            "\n",
            "# User\n",
            "Before the separator.\n",
            "\n",
            "--- \n",
            "## Model\n",
            "\n",
            "[[Inside]]\n",
        );
        let parsed = parse_document(source, &VaultConfig::default());

        assert!(parsed.frontmatter.is_some());
        assert_eq!(parsed.links.len(), 1);
        assert_eq!(
            parsed.links[0].target_path_candidate.as_deref(),
            Some("Inside")
        );
        assert!(parsed.headings.iter().any(|heading| heading.text == "User"));
        assert!(parsed
            .headings
            .iter()
            .any(|heading| heading.text == "Model"));
        assert!(parsed
            .chunk_texts
            .iter()
            .any(|chunk| chunk.content.contains("Before the separator.")));
        assert!(parsed
            .chunk_texts
            .iter()
            .any(|chunk| chunk.content.contains("Inside")));
        assert!(!parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ParseDiagnosticKind::MalformedFrontmatter));
    }

    #[test]
    fn thematic_break_sections_after_frontmatter_do_not_emit_parse_failures() {
        let source = concat!(
            "---\n",
            "title: Elara\n",
            "status: inbox\n",
            "---\n",
            "\n",
            "# Elara Card\n",
            "\n",
            "### Personality\n",
            "\n",
            "**Elara**\n",
            "---\n",
            "---\n",
            "*It was one cool evening.*\n",
            "---\n",
            "---\n",
            "***Part 3/4 of the elf slave series.***\n",
            "---\n",
            "---\n",
            "[Av.Rose](https://example.com)\n",
            "\n",
            "### Description\n",
            "\n",
            "Body text.\n",
        );
        let parsed = parse_document(source, &VaultConfig::default());

        assert!(parsed.frontmatter.is_some());
        assert!(parsed
            .headings
            .iter()
            .any(|heading| heading.text == "Elara Card"));
        assert!(parsed
            .links
            .iter()
            .any(|link| link.raw_text.contains("https://example.com")));
        assert!(!parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ParseDiagnosticKind::MalformedFrontmatter));
    }

    #[test]
    fn comments_are_stripped_from_chunks_but_links_inside_are_reported() {
        let parsed = parse_document(
            "Visible %% hidden [[Secret]] %% still visible\n\n[[Open]]",
            &VaultConfig::default(),
        );

        assert_eq!(parsed.links.len(), 2);
        assert!(parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ParseDiagnosticKind::LinkInComment));
        let chunk_text = parsed
            .chunk_texts
            .iter()
            .map(|chunk| chunk.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!chunk_text.contains("hidden"));
        assert!(!chunk_text.contains("Secret"));
        assert!(chunk_text.contains("Visible"));
        assert!(chunk_text.contains("Open"));
    }

    #[test]
    fn highlights_and_nested_tags_are_cleaned() {
        let parsed = parse_document(
            "# Note\nThis is ==very== bright and tagged #tag/subtag/deep.",
            &VaultConfig::default(),
        );

        assert_eq!(parsed.tags.len(), 1);
        assert_eq!(parsed.tags[0].tag_text, "tag/subtag/deep");
        assert!(parsed.chunk_texts[0].content.contains("very"));
        assert!(!parsed.chunk_texts[0].content.contains("=="));
    }

    #[test]
    fn html_links_and_block_refs_are_detected() {
        let parsed = parse_document(
            "Paragraph\n\n^para\n\n- item\n- item two\n\n^list\n\n> quote\n\n^quote\n\n```\ncode\n```\n\n^code\n\n<a href=\"https://example.com\">html</a>",
            &VaultConfig::default(),
        );

        assert_eq!(parsed.block_refs.len(), 4);
        assert!(parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ParseDiagnosticKind::HtmlLink));
    }

    #[test]
    fn footnotes_and_callouts_keep_links_visible() {
        let parsed = parse_document(
            "> [!NOTE]\n> Callout [[Callout Note]]\n\nReference[^1]\n\n[^1]: Footnote [[Footnote Note]]",
            &VaultConfig::default(),
        );

        assert_eq!(parsed.links.len(), 2);
        assert_eq!(
            parsed
                .links
                .iter()
                .map(|link| link.target_path_candidate.clone().unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["Callout Note".to_string(), "Footnote Note".to_string()]
        );
    }

    #[test]
    fn unicode_and_frontmatter_only_documents_do_not_panic() {
        let parsed = parse_document("---\ntitle: Привет\n---", &VaultConfig::default());

        assert!(parsed.chunk_texts.is_empty());
        assert!(parsed.headings.is_empty());
    }

    #[test]
    fn markdown_links_with_nonbreaking_spaces_do_not_panic() {
        let parsed = parse_document(
            "Prelude\u{00a0}text [Doc](docs/\u{00a0}name.md#Section)",
            &VaultConfig::default(),
        );

        assert_eq!(parsed.links.len(), 1);
        assert_eq!(parsed.links[0].link_kind, LinkKind::Markdown);
        assert_eq!(
            parsed.links[0].raw_text,
            "[Doc](docs/\u{00a0}name.md#Section)"
        );
        assert_eq!(
            parsed.links[0].target_path_candidate.as_deref(),
            Some("docs/\u{00a0}name.md")
        );
        assert_eq!(parsed.links[0].target_heading.as_deref(), Some("Section"));
    }

    #[test]
    fn empty_files_and_unclosed_wikilinks_are_safe() {
        let empty = parse_document("", &VaultConfig::default());
        let broken = parse_document("Text [[oops", &VaultConfig::default());

        assert!(empty.chunk_texts.is_empty());
        assert!(broken.links.is_empty());
        assert_eq!(broken.chunk_texts.len(), 1);
    }

    #[test]
    fn chunking_respects_configurable_strategy() {
        let config = VaultConfig {
            chunking: ChunkingConfig {
                strategy: ChunkingStrategy::Paragraph,
                target_size: 8,
                overlap: 0,
            },
            ..VaultConfig::default()
        };
        let parsed = parse_document("# Title\n\nOne\n\nTwo", &config);

        assert_eq!(parsed.chunk_texts.len(), 2);
        assert_eq!(parsed.chunk_texts[0].chunk_strategy, "paragraph");
    }

    #[test]
    fn dataview_metadata_is_extracted_without_polluting_chunks() {
        let parsed = parse_document(
            concat!(
                "status:: draft\n",
                "- [ ] Write docs [due:: 2026-04-01]\n",
                "\n",
                "`= this.status`\n",
                "\n",
                "```dataview\n",
                "TABLE status\n",
                "FROM #project\n",
                "```\n",
                "\n",
                "```dataviewjs\n",
                "dv.table([\"Status\"], [[this.status]])\n",
                "```\n",
            ),
            &VaultConfig::default(),
        );

        assert_eq!(parsed.inline_fields.len(), 1);
        assert_eq!(parsed.inline_fields[0].key, "status");
        assert_eq!(parsed.tasks.len(), 1);
        assert_eq!(parsed.tasks[0].inline_fields[0].key, "due");
        assert_eq!(parsed.inline_expressions.len(), 1);
        assert_eq!(parsed.inline_expressions[0].expression, "this.status");
        assert_eq!(parsed.dataview_blocks.len(), 2);
        assert_eq!(parsed.dataview_blocks[0].language, "dataview");
        assert_eq!(parsed.dataview_blocks[1].language, "dataviewjs");
        assert!(parsed.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("require the `dataviewjs` feature flag")));
        assert!(parsed
            .chunk_texts
            .iter()
            .all(|chunk| !chunk.content.contains("TABLE status")));
        assert!(parsed
            .chunk_texts
            .iter()
            .all(|chunk| !chunk.content.contains("this.status")));
        assert!(parsed
            .chunk_texts
            .iter()
            .all(|chunk| !chunk.content.contains("dv.table")));
    }

    #[test]
    fn inline_expression_prefix_comes_from_dataview_config() {
        let mut config = VaultConfig::default();
        config.dataview.inline_query_prefix = "dv:".to_string();

        let parsed = parse_document("`dv: this.status`\n`= this.other`\n", &config);

        assert_eq!(parsed.inline_expressions.len(), 1);
        assert_eq!(parsed.inline_expressions[0].expression, "this.status");
    }

    #[test]
    fn config_defaults_remain_sane() {
        let config = VaultConfig::default();

        assert_eq!(config.link_resolution, LinkResolutionMode::Shortest);
        assert_eq!(config.link_style, LinkStylePreference::Wikilink);
    }
}
