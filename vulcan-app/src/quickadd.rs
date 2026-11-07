//! Running imported `QuickAdd` choices without Obsidian: capture choices
//! append rendered text to a note, template choices create a note from a
//! template. Choices that need an editor (the active file, folder pickers,
//! macros) are refused with a reason.

use crate::mcp_protocol::{McpMethodError, McpNoteAppendArgs, McpNoteCreateArgs};
use crate::templates::{render_template_request, TemplateEngineKind, TemplateRenderRequest};
use crate::AppError;
use std::collections::{BTreeMap, HashMap};
use vulcan_core::config::{QuickAddCaptureChoiceConfig, QuickAddTemplateChoiceConfig};
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::{load_vault_config, ProfilePermissionGuard, VaultConfig, VaultPaths};

/// A configured choice, by kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickAddChoice {
    Capture(QuickAddCaptureChoiceConfig),
    Template(QuickAddTemplateChoiceConfig),
}

impl QuickAddChoice {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Capture(choice) => &choice.name,
            Self::Template(choice) => &choice.name,
        }
    }
}

/// What running a choice wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickAddRunReport {
    pub choice: String,
    pub path: String,
    /// Whether the note was created by this run.
    pub created: bool,
    pub changed_paths: Vec<String>,
}

/// Every capture and template choice, in configuration order.
#[must_use]
pub fn quickadd_choices(config: &VaultConfig) -> Vec<QuickAddChoice> {
    config
        .quickadd
        .capture_choices
        .iter()
        .cloned()
        .map(QuickAddChoice::Capture)
        .chain(
            config
                .quickadd
                .template_choices
                .iter()
                .cloned()
                .map(QuickAddChoice::Template),
        )
        .collect()
}

/// Split `input` into the choice whose name it starts with (the longest
/// match, case-insensitive, at a word boundary) and the rest as the value.
#[must_use]
pub fn match_quickadd_choice<'a>(
    choices: &'a [QuickAddChoice],
    input: &str,
) -> Option<(&'a QuickAddChoice, String)> {
    let input = input.trim();
    choices
        .iter()
        .filter_map(|choice| {
            let name = choice.name().trim();
            let head = input.get(..name.len())?;
            let rest = &input[name.len()..];
            (!name.is_empty()
                && head.eq_ignore_ascii_case(name)
                && (rest.is_empty() || rest.starts_with(char::is_whitespace)))
            .then(|| (choice, rest.trim().to_string()))
        })
        .max_by_key(|(choice, _)| choice.name().len())
}

/// Run `choice` with `value` as `{{VALUE}}`, with the guard's authority;
/// the written note is refreshed in the cache and auto-committed.
pub fn run_quickadd_choice(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile: &str,
    choice: &QuickAddChoice,
    value: &str,
) -> Result<QuickAddRunReport, AppError> {
    match choice {
        QuickAddChoice::Capture(choice) => run_capture(paths, guard, profile, choice, value),
        QuickAddChoice::Template(choice) => run_template(paths, guard, profile, choice, value),
    }
}

/// The note `choice` writes for `value`, without writing it.
pub fn quickadd_target_path(
    paths: &VaultPaths,
    choice: &QuickAddChoice,
    value: &str,
) -> Result<String, AppError> {
    match choice {
        QuickAddChoice::Capture(choice) => capture_path(paths, choice, value),
        QuickAddChoice::Template(choice) => template_note_path(paths, choice, value),
    }
}

fn capture_path(
    paths: &VaultPaths,
    choice: &QuickAddCaptureChoiceConfig,
    value: &str,
) -> Result<String, AppError> {
    if choice.capture_to_active_file {
        return Err(AppError::operation(format!(
            "QuickAdd choice `{}` captures to the active file, which only an editor has",
            choice.name
        )));
    }
    let target = choice
        .capture_to
        .as_deref()
        .map(str::trim)
        .filter(|target| !target.is_empty() && !target.ends_with('/'))
        .ok_or_else(|| {
            AppError::operation(format!(
                "QuickAdd choice `{}` captures to a folder or asks for a file, which needs an editor",
                choice.name
            ))
        })?;
    let config = load_vault_config(paths).config;
    note_path(&render(paths, &config, target, value)?)
}

fn template_note_path(
    paths: &VaultPaths,
    choice: &QuickAddTemplateChoiceConfig,
    value: &str,
) -> Result<String, AppError> {
    if choice.folder.choose_when_creating_note || choice.folder.create_in_same_folder_as_active_file
    {
        return Err(AppError::operation(format!(
            "QuickAdd choice `{}` asks for a folder, which needs an editor",
            choice.name
        )));
    }
    let config = load_vault_config(paths).config;
    let format = choice
        .file_name_format
        .as_deref()
        .filter(|format| !format.trim().is_empty())
        .unwrap_or("{{VALUE}}");
    let name = render(paths, &config, format, value)?;
    let folder = choice
        .folder
        .folders
        .first()
        .map(|folder| folder.to_string_lossy().trim_matches('/').to_string())
        .unwrap_or_default();
    note_path(&if folder.is_empty() {
        name
    } else {
        format!("{folder}/{name}")
    })
}

fn run_capture(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile: &str,
    choice: &QuickAddCaptureChoiceConfig,
    value: &str,
) -> Result<QuickAddRunReport, AppError> {
    let path = capture_path(paths, choice, value)?;
    let exists = paths.vault_root().join(&path).is_file();
    let mut changed_paths = Vec::new();
    if !exists {
        if !choice.create_file_if_missing.enabled {
            return Err(AppError::operation(format!(
                "{path} does not exist and QuickAdd choice `{}` does not create it",
                choice.name
            )));
        }
        let template = choice
            .create_file_if_missing
            .create_with_template
            .then(|| choice.create_file_if_missing.template.clone())
            .flatten();
        let created = create_note(paths, guard, profile, &path, template)?;
        changed_paths.extend(created);
    }
    let format = choice
        .format
        .as_deref()
        .filter(|format| !format.trim().is_empty())
        .unwrap_or("{{VALUE}}");
    let format = if choice.task && !format.trim_start().starts_with("- [") {
        format!("- [ ] {format}")
    } else {
        format.to_string()
    };
    let (mode, heading) = match &choice.insert_after {
        Some(insert) if !insert.heading.trim().is_empty() => {
            ("after_heading", Some(insert.heading.trim().to_string()))
        }
        _ if choice.prepend => ("prepend", None),
        _ => ("append", None),
    };
    let report = crate::mcp_notes::note_append(
        paths,
        guard,
        profile,
        McpNoteAppendArgs {
            note: Some(path.clone()),
            text: format,
            mode: Some(mode.to_string()),
            heading,
            periodic: None,
            date: None,
            vars: BTreeMap::from([("value".to_string(), value.to_string())]),
            check: false,
            no_commit: false,
        },
    )
    .map_err(method_error)?;
    changed_paths.push(report.path.clone());
    changed_paths.sort();
    changed_paths.dedup();
    Ok(QuickAddRunReport {
        choice: choice.name.clone(),
        path: report.path,
        created: !exists,
        changed_paths,
    })
}

fn run_template(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile: &str,
    choice: &QuickAddTemplateChoiceConfig,
    value: &str,
) -> Result<QuickAddRunReport, AppError> {
    let path = template_note_path(paths, choice, value)?;
    let template = choice.template_path.as_ref().ok_or_else(|| {
        AppError::operation(format!(
            "QuickAdd choice `{}` names no template",
            choice.name
        ))
    })?;
    if paths.vault_root().join(&path).exists() {
        return Err(AppError::operation(format!("{path} already exists")));
    }
    let changed_paths = create_note(
        paths,
        guard,
        profile,
        &path,
        Some(template.to_string_lossy().into_owned()),
    )?;
    Ok(QuickAddRunReport {
        choice: choice.name.clone(),
        path,
        created: true,
        changed_paths,
    })
}

fn create_note(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile: &str,
    path: &str,
    template: Option<String>,
) -> Result<Vec<String>, AppError> {
    crate::mcp_notes::note_create(
        paths,
        guard,
        profile,
        McpNoteCreateArgs {
            path: path.to_string(),
            body: String::new(),
            template,
            frontmatter: BTreeMap::new(),
            check: false,
            no_commit: false,
        },
    )
    .map(|report| report.changed_paths)
    .map_err(method_error)
}

/// Render `QuickAdd` tokens (`{{DATE:YYYY-MM-DD}}`, `{{VALUE}}`, …) in `text`.
fn render(
    paths: &VaultPaths,
    config: &VaultConfig,
    text: &str,
    value: &str,
) -> Result<String, AppError> {
    let vars = HashMap::from([("value".to_string(), value.to_string())]);
    render_template_request(TemplateRenderRequest {
        paths,
        vault_config: config,
        templates: &[],
        template_path: None,
        template_text: text,
        target_path: "",
        target_contents: None,
        engine: TemplateEngineKind::Native,
        vars: &vars,
        allow_mutations: false,
        run_mode: crate::templates::TemplateRunMode::Append,
        reference_date: None,
    })
    .map(|output| output.content.trim().to_string())
}

fn note_path(path: &str) -> Result<String, AppError> {
    normalize_relative_input_path(
        path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(AppError::operation)
}

fn method_error(error: McpMethodError) -> AppError {
    match error {
        McpMethodError::JsonRpc { message, .. } | McpMethodError::Tool { message, .. } => {
            AppError::operation(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(name: &str) -> QuickAddChoice {
        QuickAddChoice::Capture(QuickAddCaptureChoiceConfig {
            id: name.to_string(),
            name: name.to_string(),
            ..QuickAddCaptureChoiceConfig::default()
        })
    }

    #[test]
    fn choices_match_the_longest_name_at_a_word_boundary() {
        let choices = [capture("Log"), capture("Log work"), capture("Idea")];
        let (choice, value) = match_quickadd_choice(&choices, "log work fix it").unwrap();
        assert_eq!((choice.name(), value.as_str()), ("Log work", "fix it"));
        let (choice, value) = match_quickadd_choice(&choices, "LOG slept").unwrap();
        assert_eq!((choice.name(), value.as_str()), ("Log", "slept"));
        let (choice, value) = match_quickadd_choice(&choices, " idea ").unwrap();
        assert_eq!((choice.name(), value.as_str()), ("Idea", ""));
        assert!(match_quickadd_choice(&choices, "logbook entry").is_none());
        assert!(match_quickadd_choice(&choices, "other").is_none());
        assert!(
            match_quickadd_choice(&choices, "é").is_none(),
            "no panic on short input"
        );
    }
}
