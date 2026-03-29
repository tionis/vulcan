use crate::chunking::chunk_blocks;
use crate::config::VaultConfig;
use crate::parser::block_ref::{detect_block_refs, is_block_id_block};
use crate::parser::comment_scanner::{overlaps_comment, visible_subranges};
use crate::parser::dataview::{extract_dataview_metadata, is_dataview_code_block};
use crate::parser::link_classifier::{classify_link, explicit_display_text};
use crate::parser::parse_document_fragment;
use crate::parser::tag_extractor::extract_inline_tags;
use crate::parser::types::{
    OriginContext, ParseDiagnostic, ParseDiagnosticKind, ParsedDocument, RawHeading, SemanticBlock,
    SemanticBlockKind,
};
use pulldown_cmark::{CodeBlockKind, Event, Tag, TagEnd};
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
    saw_body_content: bool,
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
            saw_body_content: false,
        }
    }

    fn process_event(&mut self, event: Event<'a>, range: Range<usize>) {
        match event {
            Event::Start(tag) => self.handle_start(tag, range),
            Event::End(tag_end) => self.handle_end(tag_end, range),
            Event::Text(text) => self.handle_text(&text, range),
            Event::Code(text) => self.handle_inline_code(&text, range),
            Event::InlineMath(text) | Event::DisplayMath(text) => {
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
                    byte_offset_start: None,
                });
            }
            Tag::Paragraph if self.current_block.is_none() => {
                self.start_block(
                    SemanticBlockKind::Paragraph,
                    TagEnd::Paragraph,
                    range.start,
                    None,
                );
            }
            Tag::BlockQuote(_) if self.current_block.is_none() => {
                self.start_block(
                    SemanticBlockKind::BlockQuote,
                    TagEnd::BlockQuote(None),
                    range.start,
                    None,
                );
            }
            Tag::CodeBlock(kind) if self.current_block.is_none() => {
                self.start_block(
                    SemanticBlockKind::CodeBlock,
                    TagEnd::CodeBlock,
                    range.start,
                    code_block_language(kind),
                );
            }
            Tag::HtmlBlock if self.current_block.is_none() => {
                self.start_block(
                    SemanticBlockKind::HtmlBlock,
                    TagEnd::HtmlBlock,
                    range.start,
                    None,
                );
            }
            Tag::List(ordered) if self.current_block.is_none() => {
                self.start_block(
                    SemanticBlockKind::List,
                    TagEnd::List(ordered.is_some()),
                    range.start,
                    None,
                );
            }
            Tag::Table(_) if self.current_block.is_none() => {
                self.start_block(SemanticBlockKind::Table, TagEnd::Table, range.start, None);
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
            metadata.byte_offset_start.get_or_insert(range.start);
            metadata.raw_text.push_str(&self.source[range.clone()]);
            return;
        }

        for visible_range in visible_subranges(range.clone(), self.comment_regions) {
            let visible_text = &self.source[visible_range.clone()];
            let clean_text = strip_highlight_markers(visible_text);
            if !clean_text.trim().is_empty() {
                self.saw_body_content = true;
            }
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
        if let Some(metadata) = self.current_metadata.as_mut() {
            metadata.byte_offset_start.get_or_insert(range.start);
            metadata.raw_text.push_str(text);
            return;
        }

        if overlaps_comment(&range, self.comment_regions) {
            return;
        }

        if !text.trim().is_empty() {
            self.saw_body_content = true;
        }
        self.push_text_to_active(text);

        if let Some(heading) = self.current_heading.as_mut() {
            heading.text.push_str(text);
        }

        for link in &mut self.current_links {
            link.display_text.push_str(text);
        }
    }

    fn handle_inline_code(&mut self, text: &str, range: Range<usize>) {
        if overlaps_comment(&range, self.comment_regions) {
            return;
        }

        let inline_query_prefix = self.config.dataview.inline_query_prefix.as_str();
        if !inline_query_prefix.is_empty() {
            if let Some(expression) = text.strip_prefix(inline_query_prefix).map(str::trim) {
                if !expression.is_empty() {
                    self.parsed
                        .inline_expressions
                        .push(crate::RawInlineExpression {
                            expression: expression.to_string(),
                            byte_range: range.clone(),
                            line_number: line_number_for_offset(self.source, range.start),
                        });
                }
                return;
            }
        }

        self.handle_literal(text, range);
    }

    fn handle_html(&mut self, text: &str, range: Range<usize>) {
        if let Some(metadata) = self.current_metadata.as_mut() {
            metadata.byte_offset_start.get_or_insert(range.start);
            metadata.raw_text.push_str(text);
            return;
        }

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

        if !text.trim().is_empty() {
            self.saw_body_content = true;
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

    fn start_block(
        &mut self,
        kind: SemanticBlockKind,
        end_tag: TagEnd,
        byte_offset_start: usize,
        code_language: Option<String>,
    ) {
        self.current_block = Some(BlockAccumulator {
            block_kind: kind,
            byte_offset_start,
            end_tag,
            heading_path: self.heading_path.clone(),
            text: String::new(),
            code_language,
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
            code_language: block.code_language,
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

        let is_leading_frontmatter =
            !self.saw_body_content && self.parsed.raw_frontmatter.is_none();
        if !is_leading_frontmatter {
            let fragment = parse_document_fragment(
                &metadata.raw_text,
                self.config,
                metadata.byte_offset_start.unwrap_or_default(),
            );
            self.merge_fragment(fragment);
            return;
        }

        self.parsed.raw_frontmatter = Some(metadata.raw_text.clone());
        self.parsed.links.extend(extract_metadata_links(
            &metadata.raw_text,
            metadata.byte_offset_start,
            self.config,
        ));
        match parse_frontmatter_with_recovery(&metadata.raw_text) {
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

    fn merge_fragment(&mut self, parsed: ParsedDocument) {
        self.parsed.headings.extend(parsed.headings);
        self.parsed.block_refs.extend(parsed.block_refs);
        self.parsed.links.extend(parsed.links);
        self.parsed.tags.extend(parsed.tags);
        self.parsed.aliases.extend(parsed.aliases);
        self.parsed.inline_fields.extend(parsed.inline_fields);
        self.parsed.list_items.extend(parsed.list_items);
        self.parsed.tasks.extend(parsed.tasks);
        self.parsed.dataview_blocks.extend(parsed.dataview_blocks);
        self.parsed
            .inline_expressions
            .extend(parsed.inline_expressions);
        self.parsed.chunk_texts.extend(parsed.chunk_texts);
        self.parsed.diagnostics.extend(parsed.diagnostics);
    }

    fn finish(mut self) -> ParsedDocument {
        self.parsed.block_refs = detect_block_refs(&self.semantic_blocks);
        let dataview =
            extract_dataview_metadata(self.source, self.comment_regions, &self.semantic_blocks);
        self.parsed.inline_fields = dataview.inline_fields;
        self.parsed.list_items = dataview.list_items;
        self.parsed.tasks = dataview.tasks;
        self.parsed.dataview_blocks = dataview.dataview_blocks;
        for property_range in dataview.property_value_ranges {
            for link in &mut self.parsed.links {
                if property_range.start <= link.byte_offset && link.byte_offset < property_range.end
                {
                    link.origin_context = OriginContext::Property;
                }
            }
        }
        for block in &self.parsed.dataview_blocks {
            if block.language == "dataviewjs" {
                self.parsed.diagnostics.push(ParseDiagnostic {
                    kind: ParseDiagnosticKind::UnsupportedSyntax,
                    message: "DataviewJS blocks require the `dataviewjs` feature flag".to_string(),
                    byte_range: Some(block.byte_range.clone()),
                });
            }
        }
        let semantic_blocks = self
            .semantic_blocks
            .iter()
            .filter(|block| !is_block_id_block(block) && !is_dataview_code_block(block))
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
    code_language: Option<String>,
}

struct MetadataAccumulator {
    raw_text: String,
    byte_offset_start: Option<usize>,
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

fn extract_metadata_links(
    raw_text: &str,
    byte_offset_start: Option<usize>,
    config: &VaultConfig,
) -> Vec<crate::RawLink> {
    let Some(byte_offset_start) = byte_offset_start else {
        return Vec::new();
    };
    if raw_text.is_empty() {
        return Vec::new();
    }

    let mut parsed = crate::parser::parse_document(raw_text, config);
    for link in &mut parsed.links {
        link.byte_offset += byte_offset_start;
        link.origin_context = OriginContext::Property;
    }

    parsed.links
}

fn parse_frontmatter_with_recovery(raw_text: &str) -> Result<serde_yaml::Value, serde_yaml::Error> {
    match serde_yaml::from_str(raw_text) {
        Ok(frontmatter) => Ok(frontmatter),
        Err(error) => {
            let Some(repaired) = repair_block_scalar_frontmatter(raw_text) else {
                return Err(error);
            };
            serde_yaml::from_str(&repaired).map_err(|_| error)
        }
    }
}

fn repair_block_scalar_frontmatter(raw_text: &str) -> Option<String> {
    let normalized = normalize_yaml_newlines(raw_text);
    let mut repaired = Vec::new();
    let mut changed = normalized != raw_text;
    let mut block_scalar_indent = None;

    for line in normalized.split('\n') {
        let indent = count_leading_spaces(line);

        if let Some(required_indent) = block_scalar_indent {
            if line.trim().is_empty() {
                repaired.push(line.to_string());
                continue;
            }

            if indent < required_indent && !looks_like_yaml_mapping_key(line) {
                repaired.push(format!(
                    "{}{}",
                    " ".repeat(required_indent),
                    line.trim_start()
                ));
                changed = true;
                continue;
            }

            if indent < required_indent {
                block_scalar_indent = None;
            }
        }

        repaired.push(line.to_string());
        block_scalar_indent = block_scalar_content_indent(line).or(block_scalar_indent);
    }

    changed.then(|| repaired.join("\n"))
}

fn normalize_yaml_newlines(raw_text: &str) -> String {
    raw_text.replace("\r\n", "\n").replace('\r', "\n")
}

fn count_leading_spaces(line: &str) -> usize {
    line.as_bytes()
        .iter()
        .take_while(|byte| **byte == b' ')
        .count()
}

fn block_scalar_content_indent(line: &str) -> Option<usize> {
    let indent = count_leading_spaces(line);
    let trimmed = line[indent..].trim_end();
    let (_, value) = trimmed.split_once(':')?;
    let indicator = value.trim_start();
    matches!(indicator.chars().next(), Some('|' | '>')).then_some(indent + 2)
}

fn looks_like_yaml_mapping_key(line: &str) -> bool {
    let indent = count_leading_spaces(line);
    let trimmed = line[indent..].trim_end();
    let Some((key, _)) = trimmed.split_once(':') else {
        return false;
    };

    !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn complete_raw_link_text(source: &str, start: usize, end: usize) -> String {
    let mut current_end = floor_char_boundary(source, end.min(source.len()));
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
        current_end = next_char_boundary(source, current_end);
        raw = &source[start..current_end];
    }

    raw.to_string()
}

fn floor_char_boundary(source: &str, index: usize) -> usize {
    let mut boundary = index.min(source.len());
    while boundary > 0 && !source.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn next_char_boundary(source: &str, index: usize) -> usize {
    if index >= source.len() {
        return source.len();
    }

    let mut chars = source[index..].chars();
    index + chars.next().map_or(0, char::len_utf8)
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

fn code_block_language(kind: CodeBlockKind<'_>) -> Option<String> {
    match kind {
        CodeBlockKind::Fenced(info) => info
            .split_ascii_whitespace()
            .next()
            .map(|value| value.to_ascii_lowercase()),
        CodeBlockKind::Indented => None,
    }
}

fn line_number_for_offset(source: &str, offset: usize) -> usize {
    1 + source[..offset]
        .as_bytes()
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
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

#[cfg(test)]
mod tests {
    use super::complete_raw_link_text;

    #[test]
    fn completes_markdown_links_without_slicing_inside_utf8() {
        let source = "prefix [Doc](docs/\u{00a0}name.md) suffix";
        let start = source.find('[').unwrap();
        let nbsp = source.find('\u{00a0}').unwrap();
        let raw = complete_raw_link_text(source, start, nbsp + 1);

        assert_eq!(raw, "[Doc](docs/\u{00a0}name.md)");
    }
}
