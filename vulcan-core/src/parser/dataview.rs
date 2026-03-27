use crate::parser::comment_scanner::{overlaps_comment, visible_subranges};
use crate::parser::types::{
    InlineFieldKind, RawDataviewBlock, RawInlineField, RawTask, RawTaskField, SemanticBlock,
    SemanticBlockKind,
};
use regex::Regex;
use std::ops::Range;
use std::sync::OnceLock;

#[derive(Debug, Default)]
pub struct DataviewExtraction {
    pub inline_fields: Vec<RawInlineField>,
    pub tasks: Vec<RawTask>,
    pub dataview_blocks: Vec<RawDataviewBlock>,
    pub property_value_ranges: Vec<Range<usize>>,
}

pub fn extract_dataview_metadata(
    source: &str,
    comment_regions: &[Range<usize>],
    semantic_blocks: &[SemanticBlock],
) -> DataviewExtraction {
    let mut extraction = DataviewExtraction::default();
    let mut dataview_block_index = 0_usize;

    for block in semantic_blocks {
        if let Some(language) = dataview_language(block) {
            extraction.dataview_blocks.push(RawDataviewBlock {
                language: language.to_string(),
                text: block.text.clone(),
                block_index: dataview_block_index,
                byte_range: block.byte_offset_start..block.byte_offset_end,
                line_number: line_number_for_offset(source, block.byte_offset_start),
            });
            dataview_block_index += 1;
            continue;
        }

        if matches!(
            block.block_kind,
            SemanticBlockKind::CodeBlock | SemanticBlockKind::HtmlBlock
        ) {
            continue;
        }

        scan_block(
            source,
            comment_regions,
            block,
            &mut extraction.inline_fields,
            &mut extraction.tasks,
            &mut extraction.property_value_ranges,
        );
    }

    extraction
}

pub fn is_dataview_code_block(block: &SemanticBlock) -> bool {
    dataview_language(block).is_some()
}

fn dataview_language(block: &SemanticBlock) -> Option<&str> {
    match block.code_language.as_deref() {
        Some("dataview") => Some("dataview"),
        Some("dataviewjs") => Some("dataviewjs"),
        _ => None,
    }
}

fn scan_block(
    source: &str,
    comment_regions: &[Range<usize>],
    block: &SemanticBlock,
    inline_fields: &mut Vec<RawInlineField>,
    tasks: &mut Vec<RawTask>,
    property_value_ranges: &mut Vec<Range<usize>>,
) {
    let raw_block = &source[block.byte_offset_start..block.byte_offset_end];
    let mut local_offset = 0_usize;
    let mut task_stack: Vec<(usize, usize)> = Vec::new();

    for raw_line in raw_block.split_inclusive('\n') {
        let line_start = block.byte_offset_start + local_offset;
        local_offset += raw_line.len();
        let raw_line = raw_line.trim_end_matches('\n').trim_end_matches('\r');
        if raw_line.trim().is_empty() {
            continue;
        }
        let line_number = line_number_for_offset(source, line_start);

        if let Some((indent, status_char, text_start, text_end)) = parse_task_line(raw_line) {
            while task_stack
                .last()
                .is_some_and(|(stack_indent, _)| *stack_indent >= indent)
            {
                task_stack.pop();
            }
            let parent_task_index = task_stack.last().map(|(_, task_index)| *task_index);
            let visible_text = visible_line_text(source, comment_regions, line_start, raw_line.len());
            let trimmed_visible = visible_text
                .get(text_start.min(visible_text.len())..text_end.min(visible_text.len()))
                .unwrap_or("")
                .trim()
                .to_string();
            let mut inline_task_fields = Vec::new();
            scan_inline_field_variants(
                raw_line,
                line_start,
                line_number,
                comment_regions,
                InlineFieldTarget::Task(&mut inline_task_fields),
                property_value_ranges,
            );
            let task_index = tasks.len();
            tasks.push(RawTask {
                status_char,
                text: trimmed_visible,
                byte_offset: line_start,
                parent_task_index,
                section_heading: block.heading_path.last().cloned(),
                line_number,
                inline_fields: inline_task_fields,
            });
            task_stack.push((indent, task_index));
            continue;
        }

        scan_inline_field_variants(
            raw_line,
            line_start,
            line_number,
            comment_regions,
            InlineFieldTarget::Note(inline_fields),
            property_value_ranges,
        );
    }
}

enum InlineFieldTarget<'a> {
    Note(&'a mut Vec<RawInlineField>),
    Task(&'a mut Vec<RawTaskField>),
}

fn scan_inline_field_variants(
    raw_line: &str,
    line_start: usize,
    line_number: usize,
    comment_regions: &[Range<usize>],
    target: InlineFieldTarget<'_>,
    property_value_ranges: &mut Vec<Range<usize>>,
) {
    let mut target = target;

    for (regex, kind) in [
        (bracket_inline_field_regex(), InlineFieldKind::Bracket),
        (parenthesized_inline_field_regex(), InlineFieldKind::Parenthesized),
    ] {
        for captures in regex.captures_iter(raw_line) {
            let Some(full_match) = captures.get(0) else {
                continue;
            };
            let byte_range = (line_start + full_match.start())..(line_start + full_match.end());
            if overlaps_comment(&byte_range, comment_regions) {
                continue;
            }
            let Some(key_match) = captures.name("key") else {
                continue;
            };
            let Some(value_match) = captures.name("value") else {
                continue;
            };
            let value_byte_range =
                (line_start + value_match.start())..(line_start + value_match.end());
            property_value_ranges.push(value_byte_range.clone());
            push_inline_field(
                &mut target,
                normalize_inline_field_key(key_match.as_str()),
                value_match.as_str().trim().to_string(),
                kind,
                byte_range,
                value_byte_range,
                line_number,
            );
        }
    }

    if let Some(field) = parse_bare_inline_field(raw_line, line_start, comment_regions, line_number)
    {
        property_value_ranges.push(field.1.clone());
        push_inline_field(
            &mut target,
            field.0.key,
            field.0.value_text,
            field.0.kind,
            field.0.byte_range,
            field.0.value_byte_range,
            field.0.line_number,
        );
    }
}

fn push_inline_field(
    target: &mut InlineFieldTarget<'_>,
    key: String,
    value_text: String,
    kind: InlineFieldKind,
    byte_range: Range<usize>,
    value_byte_range: Range<usize>,
    line_number: usize,
) {
    match target {
        InlineFieldTarget::Note(fields) => fields.push(RawInlineField {
            key,
            value_text,
            kind,
            byte_range,
            value_byte_range,
            line_number,
        }),
        InlineFieldTarget::Task(fields) => fields.push(RawTaskField {
            key,
            value_text,
            kind,
            byte_range,
            value_byte_range,
        }),
    }
}

fn parse_bare_inline_field(
    raw_line: &str,
    line_start: usize,
    comment_regions: &[Range<usize>],
    line_number: usize,
) -> Option<(RawInlineField, Range<usize>)> {
    let leading = raw_line.len().saturating_sub(raw_line.trim_start().len());
    let trimmed = &raw_line[leading..];
    let captures = bare_inline_field_regex().captures(trimmed)?;
    let full_match = captures.get(0)?;
    let key_match = captures.name("key")?;
    let value_match = captures.name("value")?;
    let byte_range = (line_start + leading + full_match.start())..(line_start + leading + full_match.end());
    if overlaps_comment(&byte_range, comment_regions) {
        return None;
    }
    let value_byte_range =
        (line_start + leading + value_match.start())..(line_start + leading + value_match.end());
    Some((
        RawInlineField {
            key: normalize_inline_field_key(key_match.as_str()),
            value_text: value_match.as_str().trim().to_string(),
            kind: InlineFieldKind::Bare,
            byte_range,
            value_byte_range: value_byte_range.clone(),
            line_number,
        },
        value_byte_range,
    ))
}

fn parse_task_line(raw_line: &str) -> Option<(usize, char, usize, usize)> {
    let captures = task_line_regex().captures(raw_line)?;
    let indent = captures
        .name("indent")
        .map_or(0, |value| value.as_str().chars().count());
    let status_char = captures
        .name("status")
        .and_then(|value| value.as_str().chars().next())?;
    let text_match = captures.name("text")?;
    Some((indent, status_char, text_match.start(), text_match.end()))
}

fn normalize_inline_field_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn visible_line_text(
    source: &str,
    comment_regions: &[Range<usize>],
    line_start: usize,
    line_len: usize,
) -> String {
    visible_subranges(line_start..(line_start + line_len), comment_regions)
        .into_iter()
        .map(|range| source[range].to_string())
        .collect::<Vec<_>>()
        .join("")
}

fn line_number_for_offset(source: &str, offset: usize) -> usize {
    1 + source[..offset]
        .as_bytes()
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

fn task_line_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^(?P<indent>\s*)(?:[-+*]|\d+\.)\s+\[(?P<status>.)\]\s+(?P<text>.+?)\s*$")
            .expect("task regex should compile")
    })
}

fn bracket_inline_field_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"\[(?P<key>[^\[\]\r\n:][^\[\]\r\n]*?)::\s*(?P<value>.+)\]")
            .expect("bracket inline field regex should compile")
    })
}

fn parenthesized_inline_field_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"\((?P<key>[^()\r\n:][^()\r\n]*?)::\s*(?P<value>[^)\r\n]+?)\)")
            .expect("parenthesized inline field regex should compile")
    })
}

fn bare_inline_field_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^(?P<key>[A-Za-z0-9_./ -]+?)::\s*(?P<value>.+?)\s*$")
            .expect("bare inline field regex should compile")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_bare_and_bracket_inline_fields() {
        let source = "status:: active\n- [ ] Ship [due:: 2026-04-01]\n";
        let block = SemanticBlock {
            block_kind: SemanticBlockKind::List,
            text: source.to_string(),
            byte_offset_start: 0,
            byte_offset_end: source.len(),
            heading_path: vec!["Roadmap".to_string()],
            code_language: None,
        };

        let extraction = extract_dataview_metadata(source, &[], &[block]);

        assert_eq!(extraction.inline_fields.len(), 1);
        assert_eq!(extraction.inline_fields[0].key, "status");
        assert_eq!(extraction.tasks.len(), 1);
        assert_eq!(extraction.tasks[0].status_char, ' ');
        assert_eq!(extraction.tasks[0].inline_fields[0].key, "due");
    }

    #[test]
    fn recognizes_dataview_code_blocks() {
        let source = "TABLE status\nFROM #project\n";
        let block = SemanticBlock {
            block_kind: SemanticBlockKind::CodeBlock,
            text: source.to_string(),
            byte_offset_start: 0,
            byte_offset_end: source.len(),
            heading_path: Vec::new(),
            code_language: Some("dataview".to_string()),
        };

        let extraction = extract_dataview_metadata(source, &[], &[block]);

        assert_eq!(extraction.dataview_blocks.len(), 1);
        assert_eq!(extraction.dataview_blocks[0].language, "dataview");
        assert_eq!(extraction.dataview_blocks[0].text, source);
    }

    #[test]
    fn detects_nested_tasks_and_custom_status_chars() {
        let source = "- [/] Build release\n  - [x] Ship docs [due:: 2026-04-01]\n";
        let block = SemanticBlock {
            block_kind: SemanticBlockKind::List,
            text: source.to_string(),
            byte_offset_start: 0,
            byte_offset_end: source.len(),
            heading_path: vec!["Tasks".to_string()],
            code_language: None,
        };

        let extraction = extract_dataview_metadata(source, &[], &[block]);

        assert_eq!(extraction.tasks.len(), 2);
        assert_eq!(extraction.tasks[0].status_char, '/');
        assert_eq!(extraction.tasks[1].status_char, 'x');
        assert_eq!(extraction.tasks[1].parent_task_index, Some(0));
        assert_eq!(extraction.tasks[1].inline_fields[0].key, "due");
    }
}
