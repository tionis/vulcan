//! Setting Markdown checkboxes in a note (`vulcan note checkbox`).
//!
//! Plan-only: the caller supplies the note text and decides how to write
//! the result, so the CLI and editors share the same selection rules.

use crate::AppError;
use regex::Regex;

/// The state to set a checkbox to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckboxState {
    /// `[ ]` becomes `[x]` and `[x]` becomes `[ ]`.
    #[default]
    Toggle,
    Checked,
    Unchecked,
}

/// Which checkbox to change: a scope (section, heading, block, or line
/// range, as for `note get`), then a 1-based line or checkbox index within
/// it. Without either, the scope must hold exactly one checkbox.
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckboxSelector<'a> {
    pub section_id: Option<&'a str>,
    pub heading: Option<&'a str>,
    pub block_ref: Option<&'a str>,
    pub lines: Option<&'a str>,
    pub line: Option<usize>,
    pub index: Option<usize>,
}

/// A planned checkbox change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckboxPlan {
    /// The whole note after the change.
    pub updated: String,
    pub changed: bool,
    /// The section the scope resolved to, if any.
    pub section_id: Option<String>,
    pub line_number: usize,
    pub checkbox_index: usize,
    /// `checked` or `unchecked`.
    pub state: &'static str,
    pub before_marker: char,
    pub after_marker: char,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone)]
struct CheckboxCandidate {
    checkbox_index: usize,
    line_number: usize,
    marker: char,
    line: String,
}

/// Plan setting one checkbox of `source` to `state`.
pub fn plan_checkbox(
    source: &str,
    config: &vulcan_core::VaultConfig,
    selector: &CheckboxSelector<'_>,
    state: CheckboxState,
) -> Result<CheckboxPlan, AppError> {
    if matches!(selector.line, Some(0)) {
        return Err(AppError::operation(
            "`note checkbox --line` must be at least 1",
        ));
    }
    if matches!(selector.index, Some(0)) {
        return Err(AppError::operation(
            "`note checkbox --index` must be at least 1",
        ));
    }
    let selection = resolve_note_checkbox_selection(
        source,
        config,
        selector.section_id,
        selector.heading,
        selector.block_ref,
        selector.lines,
    )?;
    let candidates = collect_note_checkbox_candidates(&selection.selected_lines);
    let scope_label = note_checkbox_scope_label(
        selector.section_id,
        selector.heading,
        selector.block_ref,
        selector.lines,
        selection.section_id.as_deref(),
    );
    let candidate =
        select_note_checkbox_candidate(&candidates, selector.line, selector.index, &scope_label)?;
    let (updated_line, after_marker) = update_note_checkbox_line(&candidate.line, state)?;
    let changed = updated_line != candidate.line;
    let updated = if changed {
        replace_source_line(source, candidate.line_number, &updated_line)?
    } else {
        source.to_string()
    };
    Ok(CheckboxPlan {
        updated,
        changed,
        section_id: selection.section_id,
        line_number: candidate.line_number,
        checkbox_index: candidate.checkbox_index,
        state: note_checkbox_marker_state_name(after_marker),
        before_marker: candidate.marker,
        after_marker,
        before: candidate.line,
        after: updated_line,
    })
}

fn resolve_note_checkbox_selection(
    source: &str,
    config: &vulcan_core::VaultConfig,
    section_id: Option<&str>,
    heading: Option<&str>,
    block_ref: Option<&str>,
    lines: Option<&str>,
) -> Result<vulcan_core::NoteReadSelection, AppError> {
    let parsed = vulcan_core::parse_document(source, config);
    vulcan_core::read_note(
        source,
        &parsed,
        &vulcan_core::NoteReadOptions {
            heading: heading.map(ToOwned::to_owned),
            section_id: section_id.map(ToOwned::to_owned),
            block_ref: block_ref.map(ToOwned::to_owned),
            lines: lines.map(ToOwned::to_owned),
            match_pattern: None,
            context: 0,
            no_frontmatter: false,
        },
    )
    .map_err(AppError::operation)
}

fn collect_note_checkbox_candidates(
    lines: &[vulcan_core::NoteSelectedLine],
) -> Vec<CheckboxCandidate> {
    let checkbox =
        Regex::new(r"^(\s*(?:[-*+]|\d+[.)])\s+\[)(.)(\])").expect("checkbox regex should compile");
    lines
        .iter()
        .filter_map(|line| {
            let captures = checkbox.captures(&line.text)?;
            let marker = captures.get(2)?.as_str().chars().next()?;
            Some((line.line_number, marker, line.text.clone()))
        })
        .enumerate()
        .map(|(index, (line_number, marker, line))| CheckboxCandidate {
            checkbox_index: index + 1,
            line_number,
            marker,
            line,
        })
        .collect()
}

fn select_note_checkbox_candidate(
    candidates: &[CheckboxCandidate],
    line: Option<usize>,
    index: Option<usize>,
    scope_label: &str,
) -> Result<CheckboxCandidate, AppError> {
    if let Some(line_number) = line {
        return candidates
            .iter()
            .find(|candidate| candidate.line_number == line_number)
            .cloned()
            .ok_or_else(|| {
                let available = note_checkbox_candidate_summary(candidates);
                let detail = if available.is_empty() {
                    String::new()
                } else {
                    format!(" Available checkbox lines: {available}.")
                };
                AppError::operation(format!(
                    "no checkbox found at line {line_number} in {scope_label}.{detail}"
                ))
            });
    }

    if let Some(requested_index) = index {
        return candidates
            .iter()
            .find(|candidate| candidate.checkbox_index == requested_index)
            .cloned()
            .ok_or_else(|| {
                AppError::operation(format!(
                    "checkbox index {requested_index} is out of range for {scope_label}; found {} checkbox{}",
                    candidates.len(),
                    if candidates.len() == 1 { "" } else { "es" }
                ))
            });
    }

    match candidates {
        [] => Err(AppError::operation(format!(
            "no markdown checkboxes found in {scope_label}"
        ))),
        [candidate] => Ok(candidate.clone()),
        _ => Err(AppError::operation(format!(
            "found {} markdown checkboxes in {scope_label}; rerun with --line <n> or --index <n>: {}",
            candidates.len(),
            note_checkbox_candidate_preview(candidates)
        ))),
    }
}

fn note_checkbox_scope_label(
    section_id: Option<&str>,
    heading: Option<&str>,
    block_ref: Option<&str>,
    lines: Option<&str>,
    resolved_section_id: Option<&str>,
) -> String {
    if let Some(section_id) = section_id {
        return format!("section `{section_id}`");
    }
    if let Some(section_id) = resolved_section_id {
        if heading.is_some() || block_ref.is_some() {
            return format!("selected scope `{section_id}`");
        }
    }
    if let Some(heading) = heading {
        return format!("heading `{heading}`");
    }
    if let Some(block_ref) = block_ref {
        return format!("block reference `^{block_ref}`");
    }
    if let Some(lines) = lines {
        return format!("line range `{lines}`");
    }
    "note".to_string()
}

fn note_checkbox_candidate_summary(candidates: &[CheckboxCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| candidate.line_number.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn note_checkbox_candidate_preview(candidates: &[CheckboxCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| {
            format!(
                "{}:{}",
                candidate.line_number,
                note_checkbox_preview_text(&candidate.line)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn note_checkbox_preview_text(line: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 48;
    let preview = line.trim();
    let mut truncated = preview.chars().take(MAX_PREVIEW_CHARS).collect::<String>();
    if preview.chars().count() > MAX_PREVIEW_CHARS {
        truncated.push_str("...");
    }
    truncated
}

fn update_note_checkbox_line(line: &str, state: CheckboxState) -> Result<(String, char), AppError> {
    let checkbox =
        Regex::new(r"^(\s*(?:[-*+]|\d+[.)])\s+\[)(.)(\])").expect("checkbox regex should compile");
    let captures = checkbox.captures(line).ok_or_else(|| {
        AppError::operation(format!(
            "line is not a markdown checkbox and cannot be edited: {line}"
        ))
    })?;
    let full = captures
        .get(0)
        .ok_or_else(|| AppError::operation("failed to locate checkbox marker"))?;
    let prefix = captures.get(1).map_or("", |capture| capture.as_str());
    let marker = captures
        .get(2)
        .and_then(|capture| capture.as_str().chars().next())
        .ok_or_else(|| AppError::operation("failed to read checkbox marker"))?;
    let suffix = captures.get(3).map_or("", |capture| capture.as_str());
    let updated_marker = match state {
        CheckboxState::Toggle => note_checkbox_toggled_marker(marker)?,
        CheckboxState::Checked => 'x',
        CheckboxState::Unchecked => ' ',
    };

    Ok((
        format!(
            "{}{}{}{}{}",
            &line[..full.start()],
            prefix,
            updated_marker,
            suffix,
            &line[full.end()..]
        ),
        updated_marker,
    ))
}

fn note_checkbox_toggled_marker(marker: char) -> Result<char, AppError> {
    match marker {
        ' ' => Ok('x'),
        'x' | 'X' => Ok(' '),
        other => Err(AppError::operation(format!(
            "cannot toggle non-standard checkbox marker `[{other}]`; use `--state checked` or `--state unchecked` to normalize it"
        ))),
    }
}

fn note_checkbox_marker_state_name(marker: char) -> &'static str {
    if marker == ' ' {
        "unchecked"
    } else {
        "checked"
    }
}

fn replace_source_line(
    source: &str,
    line_number: usize,
    updated_line: &str,
) -> Result<String, AppError> {
    let index = line_number
        .checked_sub(1)
        .ok_or_else(|| AppError::operation(format!("invalid line number: {line_number}")))?;
    let mut lines = source
        .split_inclusive('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let current = lines
        .get(index)
        .ok_or_else(|| AppError::operation(format!("line {line_number} not found")))?;
    let newline = if current.ends_with("\r\n") {
        "\r\n"
    } else if current.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    lines[index] = format!("{updated_line}{newline}");
    Ok(lines.concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "# Plan\n\n- [ ] one\n- [x] two\n\n## Later\n\n* [ ] three\r\n";

    fn plan(
        selector: CheckboxSelector<'_>,
        state: CheckboxState,
    ) -> Result<CheckboxPlan, AppError> {
        plan_checkbox(NOTE, &vulcan_core::VaultConfig::default(), &selector, state)
    }

    #[test]
    fn toggles_sets_and_preserves_line_endings() {
        let toggled = plan(
            CheckboxSelector {
                line: Some(3),
                ..Default::default()
            },
            CheckboxState::Toggle,
        )
        .unwrap();
        assert!(toggled.changed);
        assert_eq!(toggled.after, "- [x] one");
        assert_eq!(toggled.state, "checked");
        let unchecked = plan(
            CheckboxSelector {
                index: Some(2),
                ..Default::default()
            },
            CheckboxState::Unchecked,
        )
        .unwrap();
        assert_eq!(unchecked.after, "- [ ] two");
        let crlf = plan(
            CheckboxSelector {
                heading: Some("Later"),
                ..Default::default()
            },
            CheckboxState::Checked,
        )
        .unwrap();
        assert!(crlf.updated.ends_with("* [x] three\r\n"));
        let unchanged = plan(
            CheckboxSelector {
                line: Some(4),
                ..Default::default()
            },
            CheckboxState::Checked,
        )
        .unwrap();
        assert!(!unchanged.changed);
        assert_eq!(unchanged.updated, NOTE);
    }

    #[test]
    fn explains_ambiguous_and_missing_selections() {
        let error = plan(CheckboxSelector::default(), CheckboxState::Toggle).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("found 3 markdown checkboxes in note"),
            "{error}"
        );
        let error = plan(
            CheckboxSelector {
                line: Some(1),
                ..Default::default()
            },
            CheckboxState::Toggle,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Available checkbox lines: 3, 4, 8"),
            "{error}"
        );
        assert!(plan(
            CheckboxSelector {
                index: Some(0),
                ..Default::default()
            },
            CheckboxState::Toggle
        )
        .is_err());
    }
}
