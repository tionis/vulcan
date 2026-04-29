use crate::parser::ParsedDocument;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteLineSpan {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteOutline {
    pub total_lines: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter_span: Option<NoteLineSpan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<NoteOutlineSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_refs: Vec<NoteOutlineBlockRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NoteOutlineOptions {
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "section")]
    pub section_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteOutlineSelection {
    pub total_lines: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter_span: Option<NoteLineSpan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_section: Option<NoteOutlineSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<NoteOutlineSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_refs: Vec<NoteOutlineBlockRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteOutlineSection {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    pub level: u8,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteOutlineBlockRef {
    pub id: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NoteReadOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "section")]
    pub section_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "match",
        alias = "match_pattern"
    )]
    pub match_pattern: Option<String>,
    #[serde(default)]
    pub context: usize,
    #[serde(default)]
    pub no_frontmatter: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteReadSelection {
    pub content: String,
    pub match_count: usize,
    pub total_lines: usize,
    pub has_more_before: bool,
    pub has_more_after: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_spans: Vec<NoteLineSpan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_lines: Vec<NoteSelectedLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteLocatedRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_id: Option<String>,
    pub line_span: NoteLineSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSelectedLine {
    pub line_number: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteSelectionError {
    message: String,
}

impl NoteSelectionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for NoteSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NoteSelectionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceLine {
    number: usize,
    raw: String,
    text: String,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrontmatterBlock {
    full_end: usize,
}

#[must_use]
pub fn outline_note(source: &str, parsed: &ParsedDocument) -> NoteOutline {
    let source_lines = build_source_lines(source);
    let frontmatter = find_frontmatter_block(source);
    let frontmatter_span =
        frontmatter.and_then(|block| line_span_for_byte_range(&source_lines, 0, block.full_end));
    let body_start = frontmatter.map_or(0, |block| block.full_end);
    let mut sections = Vec::new();

    let first_heading_byte = parsed
        .headings
        .first()
        .map_or(source.len(), |heading| heading.byte_offset);
    if body_start < first_heading_byte {
        let preamble_span = line_span_for_byte_range(&source_lines, body_start, first_heading_byte);
        if let Some(span) = preamble_span {
            let has_content = line_numbers_to_indices(span.start_line, span.end_line)
                .into_iter()
                .any(|index| !source_lines[index].text.trim().is_empty());
            if has_content {
                sections.push(NoteOutlineSection {
                    id: "preamble".to_string(),
                    heading: None,
                    heading_path: Vec::new(),
                    level: 0,
                    start_line: span.start_line,
                    end_line: span.end_line,
                });
            }
        }
    }

    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    for (index, heading) in parsed.headings.iter().enumerate() {
        while heading_stack
            .last()
            .is_some_and(|(level, _)| *level >= heading.level)
        {
            heading_stack.pop();
        }
        heading_stack.push((heading.level, heading.text.clone()));
        let heading_path = heading_stack
            .iter()
            .map(|(_, text)| text.clone())
            .collect::<Vec<_>>();
        let end = parsed
            .headings
            .iter()
            .skip(index + 1)
            .find(|candidate| candidate.level <= heading.level)
            .map_or(source.len(), |candidate| candidate.byte_offset);
        let Some(span) = line_span_for_byte_range(&source_lines, heading.byte_offset, end) else {
            continue;
        };
        sections.push(NoteOutlineSection {
            id: section_id_for_heading_path(&heading_path, span.start_line),
            heading: Some(heading.text.clone()),
            heading_path,
            level: heading.level,
            start_line: span.start_line,
            end_line: span.end_line,
        });
    }

    let block_refs = parsed
        .block_refs
        .iter()
        .filter_map(|block| {
            let span = line_span_for_byte_range(
                &source_lines,
                block.target_block_byte_start,
                block.target_block_byte_end,
            )?;
            let section_id = sections
                .iter()
                .filter(|section| {
                    span.start_line >= section.start_line && span.end_line <= section.end_line
                })
                .max_by_key(|section| section.level)
                .map(|section| section.id.clone());
            Some(NoteOutlineBlockRef {
                id: block.block_id_text.clone(),
                start_line: span.start_line,
                end_line: span.end_line,
                section_id,
            })
        })
        .collect();

    NoteOutline {
        total_lines: source_lines.len(),
        frontmatter_span,
        sections,
        block_refs,
    }
}

pub fn select_note_outline(
    outline: &NoteOutline,
    options: &NoteOutlineOptions,
) -> Result<NoteOutlineSelection, NoteSelectionError> {
    let scope_section = options
        .section_id
        .as_deref()
        .map(|section_id| section_for_id(outline, section_id).cloned())
        .transpose()?;
    let scope_depth = scope_section
        .as_ref()
        .map_or(0, note_outline_section_tree_depth);

    let sections = outline
        .sections
        .iter()
        .filter(|section| note_outline_section_matches_scope(section, scope_section.as_ref()))
        .filter(|section| {
            note_outline_section_matches_depth(
                section,
                scope_depth,
                scope_section.is_some(),
                options.depth,
            )
        })
        .cloned()
        .collect();

    let block_refs = outline
        .block_refs
        .iter()
        .filter(|block_ref| note_outline_block_ref_matches_scope(block_ref, scope_section.as_ref()))
        .filter(|block_ref| {
            note_outline_block_ref_matches_depth(
                block_ref,
                outline,
                scope_depth,
                scope_section.is_some(),
                options.depth,
            )
        })
        .cloned()
        .collect();

    Ok(NoteOutlineSelection {
        total_lines: outline.total_lines,
        frontmatter_span: outline.frontmatter_span.clone(),
        scope_section,
        sections,
        block_refs,
    })
}

pub fn read_note(
    source: &str,
    parsed: &ParsedDocument,
    options: &NoteReadOptions,
) -> Result<NoteReadSelection, NoteSelectionError> {
    if options.heading.is_some() && options.section_id.is_some() {
        return Err(NoteSelectionError::new(
            "heading and section selectors cannot be combined",
        ));
    }

    let source_lines = build_source_lines(source);
    let outline = outline_note(source, parsed);
    let mut selected = (0..source_lines.len()).collect::<Vec<_>>();
    let mut section_id = None;

    if let Some(heading) = options.heading.as_deref() {
        let section = section_for_heading(&outline, heading)?;
        selected = select_section_lines(&selected, section);
        section_id = Some(section.id.clone());
    }

    if let Some(requested_section_id) = options.section_id.as_deref() {
        let section = section_for_id(&outline, requested_section_id)?;
        selected = select_section_lines(&selected, section);
        section_id = Some(section.id.clone());
    }

    if let Some(block_ref) = options.block_ref.as_deref() {
        let block_ref = block_ref_for_id(parsed, block_ref)?;
        selected = intersect_sorted_line_indices(
            &selected,
            &line_indices_for_byte_range(
                &source_lines,
                block_ref.target_block_byte_start,
                block_ref.target_block_byte_end,
            ),
        );
        if section_id.is_none() {
            section_id = outline
                .block_refs
                .iter()
                .find(|entry| entry.id == block_ref.block_id_text)
                .and_then(|entry| entry.section_id.clone());
        }
    }

    if let Some(spec) = options.lines.as_deref() {
        selected = select_line_range(&selected, spec)?;
    }

    let mut match_count = 0;
    if let Some(pattern) = options.match_pattern.as_deref() {
        let regex =
            Regex::new(pattern).map_err(|error| NoteSelectionError::new(error.to_string()))?;
        let (filtered, hits) =
            select_matching_lines(&selected, &source_lines, &regex, options.context);
        selected = filtered;
        match_count = hits;
    }

    if options.no_frontmatter {
        selected = strip_frontmatter_lines(&selected, outline.frontmatter_span.as_ref());
    }

    Ok(build_note_read_selection(
        &source_lines,
        &selected,
        section_id,
        match_count,
    ))
}

#[must_use]
pub fn locate_note_range(
    source: &str,
    parsed: &ParsedDocument,
    start: usize,
    end: usize,
) -> Option<NoteLocatedRange> {
    let source_lines = build_source_lines(source);
    let line_span = line_span_for_byte_range(&source_lines, start, end)?;
    let outline = outline_note(source, parsed);
    let section_id = outline
        .sections
        .iter()
        .filter(|section| {
            line_span.start_line >= section.start_line && line_span.end_line <= section.end_line
        })
        .max_by_key(|section| section.level)
        .map(|section| section.id.clone());
    Some(NoteLocatedRange {
        section_id,
        line_span,
    })
}

#[must_use]
pub fn byte_range_for_line_span(source: &str, span: &NoteLineSpan) -> Option<(usize, usize)> {
    if span.start_line == 0 || span.end_line < span.start_line {
        return None;
    }

    let source_lines = build_source_lines(source);
    let start = source_lines.get(span.start_line.saturating_sub(1))?.start;
    let end = source_lines.get(span.end_line.saturating_sub(1))?.end;
    Some((start, end))
}

fn build_note_read_selection(
    source_lines: &[SourceLine],
    selected: &[usize],
    section_id: Option<String>,
    match_count: usize,
) -> NoteReadSelection {
    let line_spans = selected_line_spans(selected, source_lines);
    let content = render_selected_raw_content(selected, source_lines);
    let selected_lines = selected
        .iter()
        .map(|index| NoteSelectedLine {
            line_number: source_lines[*index].number,
            text: source_lines[*index].text.clone(),
        })
        .collect::<Vec<_>>();
    let has_more_before = selected.first().is_some_and(|index| *index > 0);
    let has_more_after = selected
        .last()
        .is_some_and(|index| *index + 1 < source_lines.len());

    NoteReadSelection {
        content,
        match_count,
        total_lines: source_lines.len(),
        has_more_before,
        has_more_after,
        section_id,
        line_spans,
        selected_lines,
    }
}

fn section_for_heading<'a>(
    outline: &'a NoteOutline,
    heading: &str,
) -> Result<&'a NoteOutlineSection, NoteSelectionError> {
    let matches = outline
        .sections
        .iter()
        .filter(|section| section.heading.as_deref() == Some(heading))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(NoteSelectionError::new(format!(
            "no heading named '{heading}' found"
        ))),
        [section] => Ok(*section),
        _ => Err(NoteSelectionError::new(format!(
            "multiple heading entries named '{heading}'"
        ))),
    }
}

fn section_for_id<'a>(
    outline: &'a NoteOutline,
    section_id: &str,
) -> Result<&'a NoteOutlineSection, NoteSelectionError> {
    outline
        .sections
        .iter()
        .find(|section| section.id == section_id)
        .ok_or_else(|| NoteSelectionError::new(format!("no section id '{section_id}' found")))
}

fn note_outline_section_tree_depth(section: &NoteOutlineSection) -> usize {
    section.heading_path.len()
}

fn note_outline_section_matches_scope(
    section: &NoteOutlineSection,
    scope_section: Option<&NoteOutlineSection>,
) -> bool {
    scope_section.is_none_or(|scope| {
        section.id != scope.id
            && section.start_line >= scope.start_line
            && section.end_line <= scope.end_line
    })
}

fn note_outline_section_matches_depth(
    section: &NoteOutlineSection,
    scope_depth: usize,
    has_scope: bool,
    max_depth: Option<usize>,
) -> bool {
    let relative_depth = note_outline_section_tree_depth(section).saturating_sub(scope_depth);
    (!has_scope || relative_depth > 0) && max_depth.is_none_or(|depth| relative_depth <= depth)
}

fn note_outline_block_ref_matches_scope(
    block_ref: &NoteOutlineBlockRef,
    scope_section: Option<&NoteOutlineSection>,
) -> bool {
    scope_section.is_none_or(|scope| {
        block_ref.start_line >= scope.start_line && block_ref.end_line <= scope.end_line
    })
}

fn note_outline_block_ref_matches_depth(
    block_ref: &NoteOutlineBlockRef,
    outline: &NoteOutline,
    scope_depth: usize,
    has_scope: bool,
    max_depth: Option<usize>,
) -> bool {
    let relative_depth = block_ref
        .section_id
        .as_deref()
        .and_then(|section_id| {
            outline
                .sections
                .iter()
                .find(|section| section.id == section_id)
                .map(note_outline_section_tree_depth)
        })
        .unwrap_or(0)
        .saturating_sub(scope_depth);

    if has_scope {
        return max_depth.is_none_or(|depth| relative_depth > 0 && relative_depth <= depth);
    }

    max_depth.is_none_or(|depth| relative_depth <= depth)
}

fn select_section_lines(current: &[usize], section: &NoteOutlineSection) -> Vec<usize> {
    intersect_sorted_line_indices(
        current,
        &line_numbers_to_indices(section.start_line, section.end_line),
    )
}

fn block_ref_for_id<'a>(
    parsed: &'a ParsedDocument,
    block_ref: &str,
) -> Result<&'a crate::RawBlockRef, NoteSelectionError> {
    let matches = parsed
        .block_refs
        .iter()
        .filter(|candidate| candidate.block_id_text == block_ref)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(NoteSelectionError::new(format!(
            "no block ref named '{block_ref}' found"
        ))),
        [block_ref] => Ok(*block_ref),
        _ => Err(NoteSelectionError::new(format!(
            "multiple block refs named '{block_ref}'"
        ))),
    }
}

fn build_source_lines(source: &str) -> Vec<SourceLine> {
    let mut offset = 0usize;
    source
        .split_inclusive('\n')
        .enumerate()
        .map(|(index, raw_line)| {
            let line = SourceLine {
                number: index + 1,
                raw: raw_line.to_string(),
                text: raw_line.trim_end_matches(['\n', '\r']).to_string(),
                start: offset,
                end: offset + raw_line.len(),
            };
            offset += raw_line.len();
            line
        })
        .collect()
}

fn find_frontmatter_block(source: &str) -> Option<FrontmatterBlock> {
    let mut lines = source.split_inclusive('\n');
    let first_line = lines.next()?;
    if trim_line(first_line) != "---" {
        return None;
    }

    let mut offset = first_line.len();
    for line in lines {
        if trim_line(line) == "---" {
            return Some(FrontmatterBlock {
                full_end: offset + line.len(),
            });
        }
        offset += line.len();
    }

    None
}

fn trim_line(line: &str) -> &str {
    line.trim_end_matches('\n').trim_end_matches('\r')
}

fn line_span_for_byte_range(
    source_lines: &[SourceLine],
    start: usize,
    end: usize,
) -> Option<NoteLineSpan> {
    let indices = line_indices_for_byte_range(source_lines, start, end);
    let (Some(first), Some(last)) = (indices.first(), indices.last()) else {
        return None;
    };
    Some(NoteLineSpan {
        start_line: source_lines[*first].number,
        end_line: source_lines[*last].number,
    })
}

fn line_indices_for_byte_range(
    source_lines: &[SourceLine],
    start: usize,
    end: usize,
) -> Vec<usize> {
    source_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.start < end && line.end > start)
        .map(|(index, _)| index)
        .collect()
}

fn line_numbers_to_indices(start_line: usize, end_line: usize) -> Vec<usize> {
    if start_line == 0 || end_line < start_line {
        return Vec::new();
    }
    ((start_line - 1)..end_line).collect()
}

fn intersect_sorted_line_indices(current: &[usize], allowed: &[usize]) -> Vec<usize> {
    let mut left = 0usize;
    let mut right = 0usize;
    let mut intersection = Vec::new();
    while left < current.len() && right < allowed.len() {
        match current[left].cmp(&allowed[right]) {
            std::cmp::Ordering::Less => left += 1,
            std::cmp::Ordering::Greater => right += 1,
            std::cmp::Ordering::Equal => {
                intersection.push(current[left]);
                left += 1;
                right += 1;
            }
        }
    }
    intersection
}

fn select_line_range(current: &[usize], spec: &str) -> Result<Vec<usize>, NoteSelectionError> {
    if current.is_empty() {
        return Ok(Vec::new());
    }

    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(NoteSelectionError::new("line range must not be empty"));
    }

    let length = current.len();
    let (start, end) = if let Some(last_count) = trimmed.strip_prefix('-') {
        let count = parse_positive_usize(last_count, "line range")?;
        let start = length.saturating_sub(count).saturating_add(1);
        (start.max(1), length)
    } else if let Some((start, end)) = trimmed.split_once('-') {
        let start = parse_positive_usize(start, "line range start")?;
        let end = if end.trim().is_empty() {
            length
        } else {
            parse_positive_usize(end, "line range end")?
        };
        (start, end)
    } else {
        let line = parse_positive_usize(trimmed, "line range")?;
        (line, line)
    };

    if start == 0 || end == 0 || start > end {
        return Err(NoteSelectionError::new(format!(
            "invalid line range: {spec}"
        )));
    }

    let start_index = start.saturating_sub(1).min(length);
    let end_index = end.min(length);
    Ok(current[start_index..end_index].to_vec())
}

fn parse_positive_usize(value: &str, label: &str) -> Result<usize, NoteSelectionError> {
    let parsed = value
        .trim()
        .parse::<usize>()
        .map_err(|error| NoteSelectionError::new(error.to_string()))?;
    if parsed == 0 {
        return Err(NoteSelectionError::new(format!("{label} must be >= 1")));
    }
    Ok(parsed)
}

fn select_matching_lines(
    current: &[usize],
    source_lines: &[SourceLine],
    pattern: &Regex,
    context: usize,
) -> (Vec<usize>, usize) {
    let hit_positions = current
        .iter()
        .enumerate()
        .filter_map(|(position, index)| {
            pattern
                .is_match(&source_lines[*index].text)
                .then_some(position)
        })
        .collect::<Vec<_>>();

    if hit_positions.is_empty() {
        return (Vec::new(), 0);
    }

    let mut selected_positions = Vec::new();
    for hit_position in &hit_positions {
        let start = hit_position.saturating_sub(context);
        let end = (*hit_position + context + 1).min(current.len());
        for position in start..end {
            if selected_positions.last() != Some(&position) {
                selected_positions.push(position);
            }
        }
    }

    (
        selected_positions
            .into_iter()
            .map(|position| current[position])
            .collect(),
        hit_positions.len(),
    )
}

fn strip_frontmatter_lines(
    current: &[usize],
    frontmatter_span: Option<&NoteLineSpan>,
) -> Vec<usize> {
    let Some(frontmatter_span) = frontmatter_span else {
        return current.to_vec();
    };
    current
        .iter()
        .copied()
        .filter(|index| {
            let line_number = index + 1;
            line_number < frontmatter_span.start_line || line_number > frontmatter_span.end_line
        })
        .collect()
}

fn selected_line_spans(selected: &[usize], source_lines: &[SourceLine]) -> Vec<NoteLineSpan> {
    if selected.is_empty() {
        return Vec::new();
    }

    let mut spans = Vec::new();
    let mut start_index = selected[0];
    let mut previous_index = selected[0];

    for current_index in selected.iter().copied().skip(1) {
        if current_index != previous_index + 1 {
            spans.push(NoteLineSpan {
                start_line: source_lines[start_index].number,
                end_line: source_lines[previous_index].number,
            });
            start_index = current_index;
        }
        previous_index = current_index;
    }

    spans.push(NoteLineSpan {
        start_line: source_lines[start_index].number,
        end_line: source_lines[previous_index].number,
    });
    spans
}

fn render_selected_raw_content(selected: &[usize], source_lines: &[SourceLine]) -> String {
    let mut rendered = String::new();
    for index in selected {
        rendered.push_str(&source_lines[*index].raw);
    }
    rendered
}

fn section_id_for_heading_path(heading_path: &[String], start_line: usize) -> String {
    if heading_path.is_empty() {
        return "preamble".to_string();
    }
    format!("{}@{start_line}", slug_heading_path(heading_path))
}

fn slug_heading_path(heading_path: &[String]) -> String {
    heading_path
        .iter()
        .map(|heading| slug_heading(heading))
        .collect::<Vec<_>>()
        .join("/")
}

fn slug_heading(heading: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in heading.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "section".to_string()
    } else {
        slug.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_document, VaultConfig};

    fn sample_source() -> &'static str {
        concat!(
            "---\n",
            "status: active\n",
            "---\n",
            "\n",
            "Intro line\n",
            "## Tasks\n",
            "Before\n",
            "TODO first\n",
            "### Nested\n",
            "TODO nested\n",
            "## Done\n",
            "- Item line\n",
            "^done-item\n",
        )
    }

    #[test]
    fn outline_note_returns_sections_block_refs_and_frontmatter() {
        let parsed = parse_document(sample_source(), &VaultConfig::default());
        let outline = outline_note(sample_source(), &parsed);

        assert_eq!(outline.total_lines, 13);
        assert_eq!(
            outline.frontmatter_span,
            Some(NoteLineSpan {
                start_line: 1,
                end_line: 3,
            })
        );
        assert_eq!(
            outline.sections,
            vec![
                NoteOutlineSection {
                    id: "preamble".to_string(),
                    heading: None,
                    heading_path: Vec::new(),
                    level: 0,
                    start_line: 4,
                    end_line: 5,
                },
                NoteOutlineSection {
                    id: "tasks@6".to_string(),
                    heading: Some("Tasks".to_string()),
                    heading_path: vec!["Tasks".to_string()],
                    level: 2,
                    start_line: 6,
                    end_line: 10,
                },
                NoteOutlineSection {
                    id: "tasks/nested@9".to_string(),
                    heading: Some("Nested".to_string()),
                    heading_path: vec!["Tasks".to_string(), "Nested".to_string()],
                    level: 3,
                    start_line: 9,
                    end_line: 10,
                },
                NoteOutlineSection {
                    id: "done@11".to_string(),
                    heading: Some("Done".to_string()),
                    heading_path: vec!["Done".to_string()],
                    level: 2,
                    start_line: 11,
                    end_line: 13,
                },
            ]
        );
    }

    #[test]
    fn outline_note_reports_block_refs_when_the_parser_emits_them() {
        let source = "Paragraph\n\n^para\n";
        let parsed = parse_document(source, &VaultConfig::default());
        let outline = outline_note(source, &parsed);

        assert_eq!(
            outline.block_refs,
            vec![NoteOutlineBlockRef {
                id: "para".to_string(),
                start_line: 1,
                end_line: 1,
                section_id: Some("preamble".to_string()),
            }]
        );
    }

    #[test]
    fn select_note_outline_limits_depth_from_document_root() {
        let parsed = parse_document(sample_source(), &VaultConfig::default());
        let outline = outline_note(sample_source(), &parsed);
        let selection = select_note_outline(
            &outline,
            &NoteOutlineOptions {
                depth: Some(1),
                ..NoteOutlineOptions::default()
            },
        )
        .expect("depth-limited selection should succeed");

        assert_eq!(selection.scope_section, None);
        assert_eq!(
            selection.sections,
            vec![
                NoteOutlineSection {
                    id: "preamble".to_string(),
                    heading: None,
                    heading_path: Vec::new(),
                    level: 0,
                    start_line: 4,
                    end_line: 5,
                },
                NoteOutlineSection {
                    id: "tasks@6".to_string(),
                    heading: Some("Tasks".to_string()),
                    heading_path: vec!["Tasks".to_string()],
                    level: 2,
                    start_line: 6,
                    end_line: 10,
                },
                NoteOutlineSection {
                    id: "done@11".to_string(),
                    heading: Some("Done".to_string()),
                    heading_path: vec!["Done".to_string()],
                    level: 2,
                    start_line: 11,
                    end_line: 13,
                },
            ]
        );
    }

    #[test]
    fn select_note_outline_scopes_to_section_and_relative_depth() {
        let source = concat!(
            "# Root\n",
            "Body\n",
            "^root-block\n",
            "## Child\n",
            "Child body\n",
            "^child-block\n",
            "### Grandchild\n",
            "Grandchild body\n",
            "^grandchild-block\n",
        );
        let parsed = parse_document(source, &VaultConfig::default());
        let outline = outline_note(source, &parsed);
        let selection = select_note_outline(
            &outline,
            &NoteOutlineOptions {
                section_id: Some("root@1".to_string()),
                depth: Some(1),
            },
        )
        .expect("scoped selection should succeed");

        assert_eq!(
            selection.scope_section,
            Some(NoteOutlineSection {
                id: "root@1".to_string(),
                heading: Some("Root".to_string()),
                heading_path: vec!["Root".to_string()],
                level: 1,
                start_line: 1,
                end_line: 9,
            })
        );
        assert_eq!(
            selection.sections,
            vec![NoteOutlineSection {
                id: "root/child@4".to_string(),
                heading: Some("Child".to_string()),
                heading_path: vec!["Root".to_string(), "Child".to_string()],
                level: 2,
                start_line: 4,
                end_line: 9,
            }]
        );
        assert!(selection.block_refs.is_empty());
    }

    #[test]
    fn read_note_supports_section_selection_and_contextual_matches() {
        let parsed = parse_document(sample_source(), &VaultConfig::default());
        let selection = read_note(
            sample_source(),
            &parsed,
            &NoteReadOptions {
                section_id: Some("tasks@6".to_string()),
                match_pattern: Some("TODO".to_string()),
                context: 1,
                ..NoteReadOptions::default()
            },
        )
        .expect("selection should succeed");

        assert_eq!(selection.section_id.as_deref(), Some("tasks@6"));
        assert_eq!(selection.match_count, 2);
        assert_eq!(
            selection.content,
            "Before\nTODO first\n### Nested\nTODO nested\n"
        );
        assert_eq!(
            selection.line_spans,
            vec![NoteLineSpan {
                start_line: 7,
                end_line: 10,
            }]
        );
        assert!(selection.has_more_before);
        assert!(selection.has_more_after);
    }

    #[test]
    fn read_note_rejects_ambiguous_heading_selection() {
        let source = concat!(
            "# Alpha\n",
            "Body\n",
            "## Repeated\n",
            "One\n",
            "## Repeated\n",
            "Two\n",
        );
        let parsed = parse_document(source, &VaultConfig::default());
        let error = read_note(
            source,
            &parsed,
            &NoteReadOptions {
                heading: Some("Repeated".to_string()),
                ..NoteReadOptions::default()
            },
        )
        .expect_err("duplicate headings should be rejected");

        assert_eq!(
            error.to_string(),
            "multiple heading entries named 'Repeated'"
        );
    }

    #[test]
    fn locate_note_range_returns_containing_section_and_line_span() {
        let parsed = parse_document(sample_source(), &VaultConfig::default());
        let start = sample_source()
            .find("TODO nested")
            .expect("nested todo should exist");
        let end = start + "TODO nested".len();
        let located =
            locate_note_range(sample_source(), &parsed, start, end).expect("range should resolve");

        assert_eq!(located.section_id.as_deref(), Some("tasks/nested@9"));
        assert_eq!(
            located.line_span,
            NoteLineSpan {
                start_line: 10,
                end_line: 10,
            }
        );
    }

    #[test]
    fn byte_range_for_line_span_round_trips_to_source_slice() {
        let span = NoteLineSpan {
            start_line: 6,
            end_line: 8,
        };
        let (start, end) =
            byte_range_for_line_span(sample_source(), &span).expect("span should resolve");

        assert_eq!(
            &sample_source()[start..end],
            "## Tasks\nBefore\nTODO first\n"
        );
    }
}
