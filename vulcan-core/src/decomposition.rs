use crate::parser::{
    parse_document, parser_options, ParseDiagnosticKind, ParsedDocument, RawHeading,
};
use crate::paths::{normalize_relative_input_path, RelativePathOptions};
use crate::{FolderNotesConfig, VaultConfig};
use pulldown_cmark::{Event, Parser};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter, Write as _};
use std::ops::Range;
use std::path::Path;
use std::sync::LazyLock;

static HTML_ANCHOR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)<[a-z][^>]*\s(?:id|name)\s*=\s*(?:\"([^\"]+)\"|'([^']+)'|([^\s\"'=<>`]+))[^>]*>"#,
    )
    .expect("HTML anchor pattern must compile")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompositionOptions {
    pub from_level: u8,
    pub through_level: u8,
    pub destination_root: String,
    pub navigation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceByteSpan {
    pub start: usize,
    pub end: usize,
}

impl From<Range<usize>> for SourceByteSpan {
    fn from(range: Range<usize>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

impl SourceByteSpan {
    fn contains(&self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecompositionDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fragment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecompositionLinkPlacement {
    pub source_byte_offset: usize,
    pub output_byte_offset: usize,
    pub raw_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecompositionNotePlan {
    pub path: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_path: Option<String>,
    pub source_spans: Vec<SourceByteSpan>,
    pub children: Vec<String>,
    #[serde(skip)]
    pub content: String,
    #[serde(skip)]
    pub link_placements: Vec<DecompositionLinkPlacement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompositionSubpathTarget {
    pub source_byte_offset: usize,
    pub source_text: String,
    pub path: String,
    pub fragment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecompositionPlan {
    pub source_path: String,
    pub destination_root: String,
    pub root_path: String,
    pub notes: Vec<DecompositionNotePlan>,
    pub diagnostics: Vec<DecompositionDiagnostic>,
    #[serde(skip)]
    pub heading_targets: Vec<DecompositionSubpathTarget>,
    #[serde(skip)]
    pub anchor_targets: Vec<DecompositionSubpathTarget>,
    #[serde(skip)]
    pub block_targets: Vec<DecompositionSubpathTarget>,
}

impl DecompositionPlan {
    #[must_use]
    pub fn note_for_source_offset(&self, offset: usize) -> Option<&DecompositionNotePlan> {
        self.notes
            .iter()
            .find(|note| note.source_spans.iter().any(|span| span.contains(offset)))
    }

    #[must_use]
    pub fn heading_target(&self, heading: &str) -> Option<&DecompositionSubpathTarget> {
        self.heading_targets
            .iter()
            .find(|target| target.source_text == heading)
    }

    #[must_use]
    pub fn heading_target_count(&self, heading: &str) -> usize {
        self.heading_targets
            .iter()
            .filter(|target| target.source_text == heading)
            .count()
    }

    #[must_use]
    pub fn fragment_target(&self, fragment: &str) -> Option<&DecompositionSubpathTarget> {
        self.heading_target(fragment).or_else(|| {
            self.anchor_targets
                .iter()
                .find(|target| target.source_text == fragment)
        })
    }

    #[must_use]
    pub fn fragment_target_count(&self, fragment: &str) -> usize {
        self.heading_target_count(fragment)
            + self
                .anchor_targets
                .iter()
                .filter(|target| target.source_text == fragment)
                .count()
    }

    #[must_use]
    pub fn block_target(&self, block: &str) -> Option<&DecompositionSubpathTarget> {
        self.block_targets
            .iter()
            .find(|target| target.source_text == block)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompositionError {
    message: String,
}

impl DecompositionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for DecompositionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DecompositionError {}

#[derive(Debug, Clone)]
struct SelectedHeading {
    heading_index: usize,
    parent: Option<usize>,
    children: Vec<usize>,
    segment: String,
    path: String,
    parent_path: String,
    source_span: SourceByteSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExplicitHtmlAnchor {
    id: String,
    byte_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlignedOutlineHeading {
    pub level: u8,
    pub title: String,
    pub byte_offset: usize,
}

pub fn plan_document_decomposition(
    source_path: &str,
    source: &str,
    config: &VaultConfig,
    options: &DecompositionOptions,
) -> Result<DecompositionPlan, DecompositionError> {
    validate_options(options)?;
    config
        .folder_notes
        .validate()
        .map_err(DecompositionError::new)?;
    let parsed = validated_parsed_document(source, config)?;
    plan_parsed_document_decomposition(source_path, source, config, options, &parsed, true)
}

pub fn plan_document_decomposition_with_aligned_outline(
    source_path: &str,
    source: &str,
    config: &VaultConfig,
    options: &DecompositionOptions,
    headings: &[AlignedOutlineHeading],
) -> Result<DecompositionPlan, DecompositionError> {
    validate_options(options)?;
    config
        .folder_notes
        .validate()
        .map_err(DecompositionError::new)?;
    if headings.is_empty() {
        return Err(DecompositionError::new("aligned outline has no headings"));
    }
    let mut previous = None;
    for heading in headings {
        if !(1..=6).contains(&heading.level)
            || heading.title.trim().is_empty()
            || heading.byte_offset >= source.len()
            || !source.is_char_boundary(heading.byte_offset)
            || previous.is_some_and(|offset| heading.byte_offset <= offset)
        {
            return Err(DecompositionError::new(
                "aligned outline headings must have non-empty titles, levels 1..=6, and strictly increasing UTF-8 byte offsets inside the document",
            ));
        }
        previous = Some(heading.byte_offset);
    }
    let mut parsed = validated_parsed_document(source, config)?;
    parsed.headings = headings
        .iter()
        .map(|heading| RawHeading {
            level: heading.level,
            text: heading.title.clone(),
            byte_offset: heading.byte_offset,
        })
        .collect();
    plan_parsed_document_decomposition(source_path, source, config, options, &parsed, false)
}

fn validated_parsed_document(
    source: &str,
    config: &VaultConfig,
) -> Result<ParsedDocument, DecompositionError> {
    if let Some(line) = unsupported_definition_line(source) {
        return Err(DecompositionError::new(format!(
            "split-note does not yet support Markdown footnotes or reference-style definitions (first occurrence at line {line}); convert them to inline links or keep them within one note before splitting"
        )));
    }
    let parsed = parse_document(source, config);
    if let Some(diagnostic) = parsed.diagnostics.iter().find(|diagnostic| {
        matches!(
            diagnostic.kind,
            ParseDiagnosticKind::HtmlLink
                | ParseDiagnosticKind::MalformedFrontmatter
                | ParseDiagnosticKind::UnsupportedSyntax
                | ParseDiagnosticKind::ResourceLimit
        )
    }) {
        let location = diagnostic.byte_range.as_ref().map_or_else(
            || "an unknown location".to_string(),
            |range| format!("line {}", line_number_for_offset(source, range.start)),
        );
        return Err(DecompositionError::new(format!(
            "cannot safely split source with parser diagnostic at {location}: {}",
            diagnostic.message
        )));
    }
    Ok(parsed)
}

#[allow(clippy::too_many_lines)]
fn plan_parsed_document_decomposition(
    source_path: &str,
    source: &str,
    config: &VaultConfig,
    options: &DecompositionOptions,
    parsed: &ParsedDocument,
    normalize_heading_markers: bool,
) -> Result<DecompositionPlan, DecompositionError> {
    let selected_flags = parsed
        .headings
        .iter()
        .map(|heading| {
            heading.level >= options.from_level && heading.level <= options.through_level
        })
        .collect::<Vec<_>>();
    if !selected_flags.iter().any(|selected| *selected) {
        return Err(DecompositionError::new(format!(
            "source has no headings in the requested level range {}..={}",
            options.from_level, options.through_level
        )));
    }

    let root_path = config
        .folder_notes
        .note_path_for_folder(&options.destination_root)
        .ok_or_else(|| {
            DecompositionError::new(format!(
                "cannot map destination folder `{}` with the configured folder-note convention",
                options.destination_root
            ))
        })?;
    let root_title = source_title(source_path)?;
    let mut selected = build_selected_headings(source, parsed, &selected_flags);
    assign_output_paths(
        &mut selected,
        &parsed.headings,
        &options.destination_root,
        &root_path,
        &config.folder_notes,
    )?;

    let selected_spans = selected
        .iter()
        .map(|heading| heading.source_span.clone())
        .collect::<Vec<_>>();
    let root_spans = complement_spans(source.len(), &selected_spans)?;

    let anchors = explicit_html_anchors(source);
    let mut diagnostics = duplicate_heading_diagnostics(parsed);
    diagnostics.extend(duplicate_anchor_diagnostics(&anchors));
    let mut notes = Vec::with_capacity(selected.len() + 1);
    notes.push(build_note_plan(
        source,
        parsed,
        &root_path,
        &root_title,
        None,
        root_spans,
        selected
            .iter()
            .enumerate()
            .filter(|(_, heading)| heading.parent.is_none())
            .map(|(_, heading)| heading.path.clone())
            .collect(),
        options.navigation,
        &selected,
    )?);

    for heading in &selected {
        let parsed_heading = &parsed.headings[heading.heading_index];
        notes.push(build_note_plan(
            source,
            parsed,
            &heading.path,
            &parsed_heading.text,
            normalize_heading_markers.then_some(parsed_heading.level),
            vec![heading.source_span.clone()],
            heading
                .children
                .iter()
                .map(|index| selected[*index].path.clone())
                .collect(),
            options.navigation,
            &selected,
        )?);
    }

    verify_complete_coverage(source.len(), &notes)?;
    let heading_targets = build_heading_targets(parsed, &selected, &notes)?;
    let anchor_targets = build_anchor_targets(&anchors, &notes)?;
    let block_targets = build_block_targets(parsed, &notes)?;
    diagnostics.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then_with(|| left.heading.cmp(&right.heading))
            .then_with(|| left.fragment.cmp(&right.fragment))
    });

    Ok(DecompositionPlan {
        source_path: source_path.to_string(),
        destination_root: options.destination_root.clone(),
        root_path,
        notes,
        diagnostics,
        heading_targets,
        anchor_targets,
        block_targets,
    })
}

fn validate_options(options: &DecompositionOptions) -> Result<(), DecompositionError> {
    if !(1..=6).contains(&options.from_level)
        || !(1..=6).contains(&options.through_level)
        || options.from_level > options.through_level
    {
        return Err(DecompositionError::new(
            "heading levels must satisfy 1 <= from_level <= through_level <= 6",
        ));
    }
    let normalized = normalize_relative_input_path(
        &options.destination_root,
        RelativePathOptions {
            expected_extension: None,
            append_extension_if_missing: false,
        },
    )
    .map_err(|error| DecompositionError::new(format!("invalid destination folder: {error}")))?;
    if normalized != options.destination_root {
        return Err(DecompositionError::new(format!(
            "destination folder must use normalized vault-relative syntax; use `{normalized}`"
        )));
    }
    Ok(())
}

fn source_title(source_path: &str) -> Result<String, DecompositionError> {
    Path::new(source_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_string)
        .ok_or_else(|| DecompositionError::new("source note has no usable filename stem"))
}

fn line_number_for_offset(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn unsupported_definition_line(source: &str) -> Option<usize> {
    source.lines().enumerate().find_map(|(index, line)| {
        if line.contains("[^") {
            return Some(index + 1);
        }
        let is_definition = {
            let trimmed = line.trim_start();
            let rest = trimmed.strip_prefix('[')?;
            rest.find("]:").is_some_and(|end| end > 0)
        };
        is_definition.then_some(index + 1)
    })
}

fn build_selected_headings(
    source: &str,
    parsed: &ParsedDocument,
    selected_flags: &[bool],
) -> Vec<SelectedHeading> {
    let mut selected = Vec::<SelectedHeading>::new();
    let mut stack = Vec::<usize>::new();
    for (heading_index, heading) in parsed.headings.iter().enumerate() {
        if !selected_flags[heading_index] {
            continue;
        }
        while stack.last().is_some_and(|index| {
            parsed.headings[selected[*index].heading_index].level >= heading.level
        }) {
            stack.pop();
        }
        let parent = stack.last().copied();
        let end = parsed
            .headings
            .iter()
            .enumerate()
            .skip(heading_index + 1)
            .find(|(candidate_index, candidate)| {
                selected_flags[*candidate_index] || candidate.level <= heading.level
            })
            .map_or(source.len(), |(_, candidate)| candidate.byte_offset);
        let index = selected.len();
        selected.push(SelectedHeading {
            heading_index,
            parent,
            children: Vec::new(),
            segment: String::new(),
            path: String::new(),
            parent_path: String::new(),
            source_span: (heading.byte_offset..end).into(),
        });
        if let Some(parent) = parent {
            selected[parent].children.push(index);
        }
        stack.push(index);
    }
    selected
}

fn assign_output_paths(
    selected: &mut [SelectedHeading],
    headings: &[RawHeading],
    destination_root: &str,
    root_path: &str,
    folder_notes: &FolderNotesConfig,
) -> Result<(), DecompositionError> {
    let root_children = selected
        .iter()
        .enumerate()
        .filter_map(|(index, heading)| heading.parent.is_none().then_some(index))
        .collect::<Vec<_>>();
    let mut used_paths = BTreeSet::from([root_path.to_lowercase()]);
    assign_children(
        selected,
        headings,
        &root_children,
        destination_root,
        root_path,
        folder_notes,
        &mut used_paths,
    )
}

fn assign_children(
    selected: &mut [SelectedHeading],
    headings: &[RawHeading],
    children: &[usize],
    parent_folder: &str,
    parent_note: &str,
    folder_notes: &FolderNotesConfig,
    used_paths: &mut BTreeSet<String>,
) -> Result<(), DecompositionError> {
    let mut sibling_segments = BTreeSet::new();
    for child_index in children {
        let base = portable_note_segment(&headings[selected[*child_index].heading_index].text);
        let has_children = !selected[*child_index].children.is_empty();
        let mut suffix = 1usize;
        let (segment, path, child_folder) = loop {
            let segment = if suffix == 1 {
                base.clone()
            } else {
                format!("{base}-{suffix}")
            };
            suffix += 1;
            if !sibling_segments.insert(segment.to_lowercase()) {
                continue;
            }
            let child_folder = join_path(parent_folder, &segment);
            let path = if has_children {
                folder_notes
                    .note_path_for_folder(&child_folder)
                    .ok_or_else(|| {
                        DecompositionError::new(format!(
                            "cannot map generated folder `{child_folder}` with the configured folder-note convention"
                        ))
                    })?
            } else {
                format!("{parent_folder}/{segment}.md")
            };
            if used_paths.insert(path.to_lowercase()) {
                break (segment, path, child_folder);
            }
        };
        selected[*child_index].segment = segment;
        selected[*child_index].path.clone_from(&path);
        selected[*child_index].parent_path = parent_note.to_string();
        let grandchildren = selected[*child_index].children.clone();
        assign_children(
            selected,
            headings,
            &grandchildren,
            &child_folder,
            &path,
            folder_notes,
            used_paths,
        )?;
    }
    Ok(())
}

fn join_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}/{child}")
    }
}

fn portable_note_segment(title: &str) -> String {
    let mut rendered = String::new();
    let mut pending_space = false;
    for character in title.trim().chars() {
        if character.is_control()
            || matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        {
            pending_space = true;
            continue;
        }
        if character.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !rendered.is_empty() {
            rendered.push(' ');
        }
        pending_space = false;
        rendered.push(character);
        if rendered.chars().count() >= 100 {
            break;
        }
    }
    let rendered = rendered.trim_matches([' ', '.', '-']).to_string();
    let rendered = if rendered.is_empty() {
        "Untitled".to_string()
    } else {
        rendered
    };
    if is_windows_reserved_name(&rendered) {
        format!("_{rendered}")
    } else {
        rendered
    }
}

fn is_windows_reserved_name(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit()
            && upper.as_bytes()[3] != b'0')
}

fn complement_spans(
    source_len: usize,
    selected_spans: &[SourceByteSpan],
) -> Result<Vec<SourceByteSpan>, DecompositionError> {
    let mut spans = selected_spans.to_vec();
    spans.sort_by_key(|span| span.start);
    let mut cursor = 0usize;
    let mut complement = Vec::new();
    for span in spans {
        if span.start < cursor || span.end < span.start || span.end > source_len {
            return Err(DecompositionError::new(
                "selected heading spans overlap or exceed the source document",
            ));
        }
        if cursor < span.start {
            complement.push((cursor..span.start).into());
        }
        cursor = span.end;
    }
    if cursor < source_len {
        complement.push((cursor..source_len).into());
    }
    Ok(complement)
}

#[allow(clippy::too_many_arguments)]
fn build_note_plan(
    source: &str,
    parsed: &ParsedDocument,
    path: &str,
    title: &str,
    source_heading_level: Option<u8>,
    source_spans: Vec<SourceByteSpan>,
    children: Vec<String>,
    navigation: bool,
    selected: &[SelectedHeading],
) -> Result<DecompositionNotePlan, DecompositionError> {
    let parent_path = selected
        .iter()
        .find(|heading| heading.path == path)
        .map(|heading| heading.parent_path.clone());
    let mut content = String::new();
    let mut link_placements = Vec::new();
    for span in &source_spans {
        if !content.is_empty() && !content.ends_with("\n\n") {
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push('\n');
        }
        let output_base = content.len();
        let (rendered, placements) =
            render_source_span(source, parsed, span, source_heading_level, output_base)?;
        content.push_str(&rendered);
        link_placements.extend(placements);
    }
    if navigation && !children.is_empty() {
        append_navigation(&mut content, &children, selected, parsed, path);
    }

    Ok(DecompositionNotePlan {
        path: path.to_string(),
        title: title.to_string(),
        parent_path,
        source_spans,
        children,
        content,
        link_placements,
    })
}

fn render_source_span(
    source: &str,
    parsed: &ParsedDocument,
    span: &SourceByteSpan,
    source_heading_level: Option<u8>,
    output_base: usize,
) -> Result<(String, Vec<DecompositionLinkPlacement>), DecompositionError> {
    let mut marker_edits = Vec::<(usize, usize, String)>::new();
    if let Some(base_level) = source_heading_level {
        for heading in parsed
            .headings
            .iter()
            .filter(|heading| span.contains(heading.byte_offset))
        {
            let new_level = heading.level.saturating_sub(base_level).saturating_add(1);
            let marker_end = heading.byte_offset + usize::from(heading.level);
            if marker_end > source.len()
                || !source.as_bytes()[heading.byte_offset..marker_end]
                    .iter()
                    .all(|byte| *byte == b'#')
            {
                return Err(DecompositionError::new(format!(
                    "cannot locate heading marker for `{}` at byte {}",
                    heading.text, heading.byte_offset
                )));
            }
            marker_edits.push((
                heading.byte_offset,
                marker_end,
                "#".repeat(usize::from(new_level)),
            ));
        }
    }

    let mut rendered = String::new();
    let mut cursor = span.start;
    for (start, end, replacement) in &marker_edits {
        rendered.push_str(&source[cursor..*start]);
        rendered.push_str(replacement);
        cursor = *end;
    }
    rendered.push_str(&source[cursor..span.end]);

    let placements = parsed
        .links
        .iter()
        .filter(|link| span.contains(link.byte_offset))
        .map(|link| {
            let removed_bytes = marker_edits
                .iter()
                .filter(|(_, end, _)| *end <= link.byte_offset)
                .map(|(start, end, replacement)| (end - start) - replacement.len())
                .sum::<usize>();
            let original_offset = link.byte_offset - span.start;
            let output_byte_offset = original_offset - removed_bytes + output_base;
            DecompositionLinkPlacement {
                source_byte_offset: link.byte_offset,
                output_byte_offset,
                raw_text: link.raw_text.clone(),
            }
        })
        .collect();
    Ok((rendered, placements))
}

fn append_navigation(
    content: &mut String,
    children: &[String],
    selected: &[SelectedHeading],
    parsed: &ParsedDocument,
    current_path: &str,
) {
    if !content.ends_with('\n') {
        content.push('\n');
    }
    if !content.ends_with("\n\n") {
        content.push('\n');
    }
    content.push_str("## Contents\n\n");
    for child in children {
        let title = selected
            .iter()
            .find(|heading| &heading.path == child)
            .map_or_else(
                || {
                    Path::new(child)
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or(child)
                        .to_string()
                },
                |heading| parsed.headings[heading.heading_index].text.clone(),
            );
        let destination = crate::move_rewrite::relative_path_from_source(current_path, child);
        writeln!(
            content,
            "- [{}](<{destination}>)",
            escape_markdown_label(&title)
        )
        .expect("writing to a String cannot fail");
    }
}

fn escape_markdown_label(value: &str) -> String {
    value.replace('\\', "\\\\").replace(']', "\\]")
}

fn verify_complete_coverage(
    source_len: usize,
    notes: &[DecompositionNotePlan],
) -> Result<(), DecompositionError> {
    let mut spans = notes
        .iter()
        .flat_map(|note| note.source_spans.iter().cloned())
        .collect::<Vec<_>>();
    spans.sort_by_key(|span| span.start);
    let mut cursor = 0usize;
    for span in spans {
        if span.start != cursor || span.end < span.start {
            return Err(DecompositionError::new(format!(
                "decomposition source coverage is not contiguous at byte {cursor}"
            )));
        }
        cursor = span.end;
    }
    if cursor != source_len {
        return Err(DecompositionError::new(format!(
            "decomposition source coverage ends at byte {cursor}, expected {source_len}"
        )));
    }
    Ok(())
}

fn build_heading_targets(
    parsed: &ParsedDocument,
    selected: &[SelectedHeading],
    notes: &[DecompositionNotePlan],
) -> Result<Vec<DecompositionSubpathTarget>, DecompositionError> {
    let selected_by_heading = selected
        .iter()
        .map(|selected| (selected.heading_index, selected))
        .collect::<BTreeMap<_, _>>();
    parsed
        .headings
        .iter()
        .enumerate()
        .map(|(index, heading)| {
            if let Some(selected) = selected_by_heading.get(&index) {
                return Ok(DecompositionSubpathTarget {
                    source_byte_offset: heading.byte_offset,
                    source_text: heading.text.clone(),
                    path: selected.path.clone(),
                    fragment: None,
                });
            }
            let note = note_for_offset(notes, heading.byte_offset)?;
            Ok(DecompositionSubpathTarget {
                source_byte_offset: heading.byte_offset,
                source_text: heading.text.clone(),
                path: note.path.clone(),
                fragment: Some(heading.text.clone()),
            })
        })
        .collect()
}

fn explicit_html_anchors(source: &str) -> Vec<ExplicitHtmlAnchor> {
    let mut anchors = Vec::new();
    for (event, range) in Parser::new_ext(source, parser_options()).into_offset_iter() {
        let (Event::Html(html) | Event::InlineHtml(html)) = event else {
            continue;
        };
        for captures in HTML_ANCHOR_PATTERN.captures_iter(&html) {
            let Some(whole_match) = captures.get(0) else {
                continue;
            };
            let Some(id) = (1..=3).find_map(|index| captures.get(index)) else {
                continue;
            };
            anchors.push(ExplicitHtmlAnchor {
                id: id.as_str().to_string(),
                byte_offset: range.start + whole_match.start(),
            });
        }
    }
    anchors
}

fn build_anchor_targets(
    anchors: &[ExplicitHtmlAnchor],
    notes: &[DecompositionNotePlan],
) -> Result<Vec<DecompositionSubpathTarget>, DecompositionError> {
    anchors
        .iter()
        .map(|anchor| {
            let note = note_for_offset(notes, anchor.byte_offset)?;
            Ok(DecompositionSubpathTarget {
                source_byte_offset: anchor.byte_offset,
                source_text: anchor.id.clone(),
                path: note.path.clone(),
                fragment: Some(anchor.id.clone()),
            })
        })
        .collect()
}

fn build_block_targets(
    parsed: &ParsedDocument,
    notes: &[DecompositionNotePlan],
) -> Result<Vec<DecompositionSubpathTarget>, DecompositionError> {
    parsed
        .block_refs
        .iter()
        .map(|block| {
            let note = note_for_offset(notes, block.block_id_byte_offset)?;
            Ok(DecompositionSubpathTarget {
                source_byte_offset: block.block_id_byte_offset,
                source_text: block.block_id_text.clone(),
                path: note.path.clone(),
                fragment: Some(block.block_id_text.clone()),
            })
        })
        .collect()
}

fn note_for_offset(
    notes: &[DecompositionNotePlan],
    offset: usize,
) -> Result<&DecompositionNotePlan, DecompositionError> {
    notes
        .iter()
        .find(|note| note.source_spans.iter().any(|span| span.contains(offset)))
        .ok_or_else(|| {
            DecompositionError::new(format!(
                "no generated note owns source byte offset {offset}"
            ))
        })
}

fn duplicate_heading_diagnostics(parsed: &ParsedDocument) -> Vec<DecompositionDiagnostic> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for heading in &parsed.headings {
        *counts.entry(&heading.text).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(heading, count)| DecompositionDiagnostic {
            code: "duplicate_heading_target".to_string(),
            message: format!(
                "heading `{heading}` occurs {count} times; filename paths are disambiguated, but links to that heading are ambiguous"
            ),
            heading: Some(heading.to_string()),
            fragment: None,
        })
        .collect()
}

fn duplicate_anchor_diagnostics(anchors: &[ExplicitHtmlAnchor]) -> Vec<DecompositionDiagnostic> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for anchor in anchors {
        *counts.entry(&anchor.id).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(anchor, count)| DecompositionDiagnostic {
            code: "duplicate_anchor_target".to_string(),
            message: format!(
                "HTML anchor `{anchor}` occurs {count} times; links to that anchor are ambiguous"
            ),
            heading: None,
            fragment: Some(anchor.to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FolderNotePlacement, FolderNotesConfig};

    fn options() -> DecompositionOptions {
        DecompositionOptions {
            from_level: 2,
            through_level: 3,
            destination_root: "Rulebook".to_string(),
            navigation: true,
        }
    }

    #[test]
    fn plans_nested_folder_note_tree_and_covers_every_source_byte() {
        let source = "---\ntitle: Rules\n---\n# Rules\n\nIntro.\n\n## Combat\n\nChapter intro.\n\n### Initiative\n\nAct first.\n\n### Damage\n\nTake harm.\n\n## Magic\n\nCast spells.\n";
        let plan =
            plan_document_decomposition("Rulebook.md", source, &VaultConfig::default(), &options())
                .expect("plan");

        assert_eq!(plan.root_path, "Rulebook/Rulebook.md");
        assert_eq!(
            plan.notes
                .iter()
                .map(|note| note.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Rulebook/Rulebook.md",
                "Rulebook/Combat/Combat.md",
                "Rulebook/Combat/Initiative.md",
                "Rulebook/Combat/Damage.md",
                "Rulebook/Magic.md",
            ]
        );
        assert!(plan.notes[0].content.contains("# Rules"));
        assert!(plan.notes[0]
            .content
            .contains("[Combat](<Combat/Combat.md>)"));
        assert!(plan.notes[1].content.starts_with("# Combat"));
        assert!(plan.notes[1]
            .content
            .contains("[Initiative](<Initiative.md>)"));
        assert!(plan.notes[2].content.starts_with("# Initiative"));
        assert!(!plan.notes[2].content.contains("### Initiative"));

        let mut spans = plan
            .notes
            .iter()
            .flat_map(|note| note.source_spans.iter())
            .collect::<Vec<_>>();
        spans.sort_by_key(|span| span.start);
        assert_eq!(spans.first().map(|span| span.start), Some(0));
        assert_eq!(spans.last().map(|span| span.end), Some(source.len()));
        assert!(spans
            .windows(2)
            .all(|window| window[0].end == window[1].start));
    }

    #[test]
    fn aligned_outline_can_supply_titles_without_rewriting_source_markers() {
        let source = "Preface.\n\nFirst section.\n\nSecond section.\n";
        let first = source.find("First").expect("first offset");
        let second = source.find("Second").expect("second offset");
        let plan = plan_document_decomposition_with_aligned_outline(
            "document.md",
            source,
            &VaultConfig::default(),
            &DecompositionOptions {
                from_level: 1,
                through_level: 2,
                destination_root: "Artifact".to_string(),
                navigation: false,
            },
            &[
                AlignedOutlineHeading {
                    level: 1,
                    title: "Opening".to_string(),
                    byte_offset: first,
                },
                AlignedOutlineHeading {
                    level: 2,
                    title: "Continuation".to_string(),
                    byte_offset: second,
                },
            ],
        )
        .expect("aligned plan");

        assert_eq!(plan.notes[1].title, "Opening");
        assert_eq!(plan.notes[2].title, "Continuation");
        assert!(plan.notes[1].content.starts_with("First section."));
        assert!(!plan.notes[1].content.starts_with('#'));
    }

    #[test]
    fn outside_folder_notes_keep_root_at_the_original_style_path() {
        let config = VaultConfig {
            folder_notes: FolderNotesConfig {
                placement: FolderNotePlacement::Outside,
                name: "{{folder_name}}".to_string(),
            },
            ..VaultConfig::default()
        };
        let plan = plan_document_decomposition(
            "Books/Rules.md",
            "# Rules\n\n## Combat\nText\n\n### Damage\nMore\n",
            &config,
            &DecompositionOptions {
                destination_root: "Books/Rules".to_string(),
                ..options()
            },
        )
        .expect("plan");

        assert_eq!(plan.root_path, "Books/Rules.md");
        assert_eq!(plan.notes[1].path, "Books/Rules/Combat.md");
        assert_eq!(plan.notes[2].path, "Books/Rules/Combat/Damage.md");
    }

    #[test]
    fn duplicate_and_unsafe_titles_get_portable_unique_paths() {
        let plan = plan_document_decomposition(
            "Rules.md",
            "# Rules\n## A/B: C?\nOne\n## A/B: C?\nTwo\n## CON\nThree\n",
            &VaultConfig::default(),
            &DecompositionOptions {
                through_level: 2,
                ..options()
            },
        )
        .expect("plan");

        assert_eq!(plan.notes[1].path, "Rulebook/A B C.md");
        assert_eq!(plan.notes[2].path, "Rulebook/A B C-2.md");
        assert_eq!(plan.notes[3].path, "Rulebook/_CON.md");
        assert_eq!(plan.diagnostics.len(), 1);
        assert_eq!(plan.heading_target_count("A/B: C?"), 2);
    }

    #[test]
    fn preserves_explicit_html_anchors_without_polluting_note_titles() {
        let source = "# <span id=\"page-1-0\"></span>Rules\nSee [combat](#page-2-0).\n\n## <span id=\"page-2-0\"></span>Combat\nFight.\n";
        let plan =
            plan_document_decomposition("Rulebook.md", source, &VaultConfig::default(), &options())
                .expect("plan");

        assert_eq!(plan.notes[1].path, "Rulebook/Combat.md");
        assert_eq!(plan.notes[1].title, "Combat");
        assert!(plan.notes[1]
            .content
            .starts_with("# <span id=\"page-2-0\"></span>Combat"));

        let root_anchor = plan.fragment_target("page-1-0").expect("root anchor");
        assert_eq!(root_anchor.path, "Rulebook/Rulebook.md");
        assert_eq!(root_anchor.fragment.as_deref(), Some("page-1-0"));
        let combat_anchor = plan.fragment_target("page-2-0").expect("combat anchor");
        assert_eq!(combat_anchor.path, "Rulebook/Combat.md");
        assert_eq!(combat_anchor.fragment.as_deref(), Some("page-2-0"));
    }

    #[test]
    fn tracks_link_offsets_after_heading_demotion() {
        let source =
            "# Rules\n## Chapter\nSee [asset](assets/map.png).\n### Concept\nSee [[#Concept]].\n";
        let plan = plan_document_decomposition(
            "Rules.md",
            source,
            &VaultConfig::default(),
            &DecompositionOptions {
                through_level: 2,
                navigation: false,
                ..options()
            },
        )
        .expect("plan");
        let chapter = &plan.notes[1];

        assert!(chapter.content.starts_with("# Chapter"));
        assert_eq!(chapter.link_placements.len(), 2);
        for placement in &chapter.link_placements {
            assert_eq!(
                &chapter.content[placement.output_byte_offset
                    ..placement.output_byte_offset + placement.raw_text.len()],
                placement.raw_text
            );
        }
        let concept = plan.heading_target("Concept").expect("concept target");
        assert_eq!(concept.path, chapter.path);
        assert_eq!(concept.fragment.as_deref(), Some("Concept"));
    }

    #[test]
    fn rejects_invalid_ranges_missing_headings_and_definition_syntax() {
        let mut invalid = options();
        invalid.from_level = 4;
        invalid.through_level = 2;
        assert!(plan_document_decomposition(
            "Rules.md",
            "# Rules\n",
            &VaultConfig::default(),
            &invalid
        )
        .is_err());
        assert!(plan_document_decomposition(
            "Rules.md",
            "# Rules\n",
            &VaultConfig::default(),
            &options()
        )
        .is_err());
        assert!(plan_document_decomposition(
            "Rules.md",
            "# Rules\n## Chapter\nSee note[^1].\n\n[^1]: Footnote.\n",
            &VaultConfig::default(),
            &options()
        )
        .is_err());
        let html_error = plan_document_decomposition(
            "Rules.md",
            "# Rules\n## Chapter\n<img src=\"assets/map.png\">\n",
            &VaultConfig::default(),
            &options(),
        )
        .expect_err("HTML asset should fail closed");
        assert!(html_error.to_string().contains("line 3"));
    }
}
