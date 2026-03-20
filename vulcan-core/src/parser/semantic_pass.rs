use crate::chunking::chunk_blocks;
use crate::config::VaultConfig;
use crate::parser::block_ref::{detect_block_refs, is_block_id_block};
use crate::parser::comment_scanner::{overlaps_comment, visible_subranges};
use crate::parser::link_classifier::{classify_link, explicit_display_text};
use crate::parser::tag_extractor::extract_inline_tags;
use crate::parser::types::{
    ParseDiagnostic, ParseDiagnosticKind, ParsedDocument, RawHeading, SemanticBlock,
    SemanticBlockKind,
};
use pulldown_cmark::{Event, Tag, TagEnd};
use std::ops::Range;

pub fn process_events<'a, I>(
    source: &'a str,
    config: &VaultConfig,
    comment_regions: &[Range<usize>],
    events: I,
) -> ParsedDocument
where
    I: IntoIterator<Item = (Event<'a>, Range<usize>)>,
{
    let mut state = SemanticProcessor::new(source, config, comment_regions);

    for (event, range) in events {
        state.process_event(event, range);
    }

    state.finish()
}

struct SemanticProcessor<'a> {
    source: &'a str,
    config: &'a VaultConfig,
    comment_regions: &'a [Range<usize>],
    parsed: ParsedDocument,
    heading_path: Vec<String>,
    current_heading: Option<HeadingAccumulator>,
    current_block: Option<BlockAccumulator>,
    current_metadata: Option<MetadataAccumulator>,
    current_links: Vec<LinkAccumulator>,
    semantic_blocks: Vec<SemanticBlock>,
}

impl<'a> SemanticProcessor<'a> {
    fn new(source: &'a str, config: &'a VaultConfig, comment_regions: &'a [Range<usize>]) -> Self {
        Self {
            source,
            config,
            comment_regions,
            parsed: ParsedDocument::default(),
            heading_path: Vec::new(),
            current_heading: None,
            current_block: None,
            current_metadata: None,
            current_links: Vec::new(),
            semantic_blocks: Vec::new(),
        }
    }

    fn process_event(&mut self, event: Event<'a>, range: Range<usize>) {
        match event {
            Event::Start(tag) => self.handle_start(tag, range),
            Event::End(tag_end) => self.handle_end(tag_end, range),
            Event::Text(text) => self.handle_text(&text, range),
            Event::Code(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.handle_literal(&text, range);
            }
            Event::Html(text) | Event::InlineHtml(text) => self.handle_html(&text, range),
            Event::FootnoteReference(label) => self.handle_literal(&format!("[^{label}]"), range),
            Event::SoftBreak | Event::HardBreak => self.push_text_to_active("\n"),
            Event::TaskListMarker(checked) => {
                self.push_text_to_active(if checked { "[x] " } else { "[ ] " });
            }
            Event::Rule => self.push_text_to_active("\n---\n"),
        }
    }

    fn handle_start(&mut self, tag: Tag<'a>, range: Range<usize>) {
        match tag {
            Tag::Heading { level, .. } => {
                self.current_heading = Some(HeadingAccumulator {
                    level: level as u8,
                    byte_offset: range.start,
                    text: String::new(),
                });
            }
            Tag::MetadataBlock(_) => {
                self.current_metadata = Some(MetadataAccumulator {
                    raw_text: String::new(),
                });
            }
            Tag::Paragraph if self.current_block.is_none() => {
                self.start_block(SemanticBlockKind::Paragraph, TagEnd::Paragraph, range.start);
            }
            Tag::BlockQuote(_) if self.current_block.is_none() => {
                self.start_block(
                    SemanticBlockKind::BlockQuote,
                    TagEnd::BlockQuote(None),
                    range.start,
                );
            }
            Tag::CodeBlock(_) if self.current_block.is_none() => {
                self.start_block(SemanticBlockKind::CodeBlock, TagEnd::CodeBlock, range.start);
            }
            Tag::HtmlBlock if self.current_block.is_none() => {
                self.start_block(SemanticBlockKind::HtmlBlock, TagEnd::HtmlBlock, range.start);
            }
            Tag::List(ordered) if self.current_block.is_none() => {
                self.start_block(
                    SemanticBlockKind::List,
                    TagEnd::List(ordered.is_some()),
                    range.start,
                );
            }
            Tag::Table(_) if self.current_block.is_none() => {
                self.start_block(SemanticBlockKind::Table, TagEnd::Table, range.start);
            }
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => self.current_links.push(LinkAccumulator {
                byte_offset: range.start,
                display_text: String::new(),
                dest_url: dest_url.to_string(),
                is_image: false,
                link_type,
            }),
            Tag::Image {
                link_type,
                dest_url,
                ..
            } => self.current_links.push(LinkAccumulator {
                byte_offset: range.start,
                display_text: String::new(),
                dest_url: dest_url.to_string(),
                is_image: true,
                link_type,
            }),
            _ => {}
        }
    }

    fn handle_end(&mut self, tag_end: TagEnd, range: Range<usize>) {
        match tag_end {
            TagEnd::Heading(level) => {
                if let Some(heading) = self.current_heading.take() {
                    let text = heading.text.trim().to_string();
                    if !text.is_empty() {
                        self.parsed.headings.push(RawHeading {
                            level: heading.level,
                            text: text.clone(),
                            byte_offset: heading.byte_offset,
                        });
                        self.heading_path.truncate(level as usize - 1);
                        self.heading_path.push(text);
                    }
                }
            }
            TagEnd::MetadataBlock(_) => self.finalize_metadata(),
            TagEnd::Paragraph
            | TagEnd::BlockQuote(_)
            | TagEnd::CodeBlock
            | TagEnd::HtmlBlock
            | TagEnd::List(_)
            | TagEnd::Table => self.finalize_block(tag_end, range.end),
            TagEnd::Link | TagEnd::Image => self.finalize_link(range.end),
            TagEnd::Item | TagEnd::TableRow | TagEnd::TableCell => {
                self.push_text_to_active("\n");
            }
            _ => {}
        }
    }

    fn handle_text(&mut self, _text: &str, range: Range<usize>) {
        if let Some(metadata) = self.current_metadata.as_mut() {
            metadata.raw_text.push_str(&self.source[range.clone()]);
            return;
        }

        for visible_range in visible_subranges(range.clone(), self.comment_regions) {
            let visible_text = &self.source[visible_range.clone()];
            let clean_text = strip_highlight_markers(visible_text);
            self.push_text_to_active(&clean_text);

            if let Some(heading) = self.current_heading.as_mut() {
                heading.text.push_str(&clean_text);
            }

            for link in &mut self.current_links {
                link.display_text.push_str(&clean_text);
            }

            self.parsed
                .tags
                .extend(extract_inline_tags(visible_text, visible_range.start));
        }

        if overlaps_comment(&range, self.comment_regions) {
            self.push_comment_gap();
        }
    }

    fn handle_literal(&mut self, text: &str, range: Range<usize>) {
        if overlaps_comment(&range, self.comment_regions) {
            return;
        }

        self.push_text_to_active(text);

        if let Some(heading) = self.current_heading.as_mut() {
            heading.text.push_str(text);
        }

        for link in &mut self.current_links {
            link.display_text.push_str(text);
        }
    }

    fn handle_html(&mut self, text: &str, range: Range<usize>) {
        if contains_html_link(text) {
            self.parsed.diagnostics.push(ParseDiagnostic {
                kind: ParseDiagnosticKind::HtmlLink,
                message: "HTML link detected; HTML links are not tracked in the graph".to_string(),
                byte_range: Some(range.clone()),
            });
        }

        if overlaps_comment(&range, self.comment_regions) {
            return;
        }

        self.push_text_to_active(text);
        if let Some(heading) = self.current_heading.as_mut() {
            heading.text.push_str(text);
        }
    }

    fn push_comment_gap(&mut self) {
        if let Some(block) = self.current_block.as_mut() {
            if !block.text.ends_with(' ') {
                block.text.push(' ');
            }
        }
    }

    fn push_text_to_active(&mut self, text: &str) {
        if let Some(block) = self.current_block.as_mut() {
            block.text.push_str(text);
        }
    }

    fn start_block(&mut self, kind: SemanticBlockKind, end_tag: TagEnd, byte_offset_start: usize) {
        self.current_block = Some(BlockAccumulator {
            block_kind: kind,
            byte_offset_start,
            end_tag,
            heading_path: self.heading_path.clone(),
            text: String::new(),
        });
    }

    fn finalize_block(&mut self, tag_end: TagEnd, byte_offset_end: usize) {
        let Some(block) = self.current_block.take() else {
            return;
        };
        if block.end_tag != tag_end {
            self.current_block = Some(block);
            return;
        }

        let text = block.text.trim().to_string();
        if text.is_empty() {
            return;
        }

        self.semantic_blocks.push(SemanticBlock {
            block_kind: block.block_kind,
            text,
            byte_offset_start: block.byte_offset_start,
            byte_offset_end,
            heading_path: block.heading_path,
        });
    }

    fn finalize_link(&mut self, byte_offset_end: usize) {
        let Some(link) = self.current_links.pop() else {
            return;
        };

        let raw_text = complete_raw_link_text(self.source, link.byte_offset, byte_offset_end);
        let parsed_link = classify_link(
            link.link_type,
            link.is_image,
            &link.dest_url,
            raw_text,
            explicit_display_text(link.link_type, link.is_image, &link.display_text),
            link.byte_offset,
        );

        if overlaps_comment(&(link.byte_offset..byte_offset_end), self.comment_regions) {
            self.parsed.diagnostics.push(ParseDiagnostic {
                kind: ParseDiagnosticKind::LinkInComment,
                message: "Link appears inside an Obsidian comment".to_string(),
                byte_range: Some(link.byte_offset..byte_offset_end),
            });
        }

        self.parsed.links.push(parsed_link);
    }

    fn finalize_metadata(&mut self) {
        let Some(metadata) = self.current_metadata.take() else {
            return;
        };

        self.parsed.raw_frontmatter = Some(metadata.raw_text.clone());
        match serde_yaml::from_str(&metadata.raw_text) {
            Ok(frontmatter) => {
                self.parsed.aliases = extract_aliases(&frontmatter);
                self.parsed
                    .tags
                    .extend(extract_frontmatter_tags(&frontmatter));
                self.parsed.frontmatter = Some(frontmatter);
            }
            Err(error) => {
                self.parsed.diagnostics.push(ParseDiagnostic {
                    kind: ParseDiagnosticKind::MalformedFrontmatter,
                    message: format!("Failed to parse frontmatter: {error}"),
                    byte_range: None,
                });
            }
        }
    }

    fn finish(mut self) -> ParsedDocument {
        self.parsed.block_refs = detect_block_refs(&self.semantic_blocks);
        let semantic_blocks = self
            .semantic_blocks
            .iter()
            .filter(|block| !is_block_id_block(block))
            .cloned()
            .collect::<Vec<_>>();
        self.parsed.chunk_texts = chunk_blocks(&semantic_blocks, &self.config.chunking);
        self.parsed.aliases = dedupe_strings(self.parsed.aliases);
        self.parsed.tags = dedupe_tags(self.parsed.tags);
        self.parsed.links.sort_by_key(|link| link.byte_offset);
        self.parsed
            .headings
            .sort_by_key(|heading| heading.byte_offset);
        self.parsed
            .block_refs
            .sort_by_key(|block_ref| block_ref.block_id_byte_offset);
        self.parsed.tags.sort_by_key(|tag| tag.byte_offset);
        self.parsed
    }
}

struct HeadingAccumulator {
    level: u8,
    byte_offset: usize,
    text: String,
}

struct BlockAccumulator {
    block_kind: SemanticBlockKind,
    byte_offset_start: usize,
    end_tag: TagEnd,
    heading_path: Vec<String>,
    text: String,
}

struct MetadataAccumulator {
    raw_text: String,
}

struct LinkAccumulator {
    byte_offset: usize,
    display_text: String,
    dest_url: String,
    is_image: bool,
    link_type: pulldown_cmark::LinkType,
}

fn strip_highlight_markers(text: &str) -> String {
    text.replace("==", "")
}

fn contains_html_link(text: &str) -> bool {
    let lowercase = text.to_ascii_lowercase();
    (lowercase.contains("<a ") && lowercase.contains("href="))
        || (lowercase.contains("<img ") && lowercase.contains("src="))
}

fn complete_raw_link_text(source: &str, start: usize, end: usize) -> String {
    let mut current_end = end.min(source.len());
    let mut raw = &source[start..current_end];
    let expected_suffix = if raw.starts_with("[[") || raw.starts_with("![[") {
        "]]"
    } else if raw.starts_with('[') || raw.starts_with("![") {
        ")"
    } else {
        ""
    };

    while !expected_suffix.is_empty()
        && !raw.ends_with(expected_suffix)
        && current_end < source.len()
    {
        current_end += 1;
        raw = &source[start..current_end];
    }

    raw.to_string()
}

fn extract_aliases(frontmatter: &serde_yaml::Value) -> Vec<String> {
    extract_string_list(frontmatter, "aliases")
}

fn extract_frontmatter_tags(frontmatter: &serde_yaml::Value) -> Vec<crate::parser::types::RawTag> {
    extract_string_list(frontmatter, "tags")
        .into_iter()
        .flat_map(|value| {
            value
                .split([',', ' '])
                .filter(|candidate| !candidate.is_empty())
                .map(|tag| crate::parser::types::RawTag {
                    tag_text: tag.trim_start_matches('#').to_string(),
                    byte_offset: 0,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn extract_string_list(frontmatter: &serde_yaml::Value, key: &str) -> Vec<String> {
    let Some(mapping) = frontmatter.as_mapping() else {
        return Vec::new();
    };
    let Some(value) = mapping.get(serde_yaml::Value::String(key.to_string())) else {
        return Vec::new();
    };

    match value {
        serde_yaml::Value::String(value) => vec![value.clone()],
        serde_yaml::Value::Sequence(values) => values
            .iter()
            .filter_map(serde_yaml::Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();

    for value in values {
        if seen.insert(value.clone()) {
            deduped.push(value);
        }
    }

    deduped
}

fn dedupe_tags(values: Vec<crate::parser::types::RawTag>) -> Vec<crate::parser::types::RawTag> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();

    for value in values {
        if seen.insert((value.tag_text.clone(), value.byte_offset)) {
            deduped.push(value);
        }
    }

    deduped
}
