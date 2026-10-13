//! Code actions and task toggling for editors.
//!
//! Actions that only change the current buffer carry their edit; actions
//! that touch other files (creating a note) or need the Tasks semantics on
//! the writer side (toggling a task) are commands the transport executes
//! through the same open/closed routing as renames.

use super::edits::{text_edits, EditorDocumentEdit, EditorEditPlan};
use super::{is_markdown_path, EditorSession};
use crate::AppError;
use std::collections::BTreeMap;
use vulcan_core::editor::{element_at, link_range, EditorElement, TextPosition};
use vulcan_core::ordinary_write::OrdinaryWriteChange;
use vulcan_core::resolver::LinkResolutionProblem;
use vulcan_core::tasks::{apply_inline_task_toggle, toggle_inline_task_line, RecurrencePlacement};
use vulcan_core::LinkKind;

/// What kind of action this is, for editors that group actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorActionKind {
    /// Fixes a diagnostic.
    QuickFix,
    /// Rewrites code without fixing anything.
    Refactor,
    /// Moves code somewhere new.
    Extract,
}

/// What an action does when chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorAction {
    /// Edit the open document directly.
    Edit(EditorDocumentEdit),
    /// Create an empty note at a vault path.
    CreateNote { path: String },
    /// Toggle the task on a 0-based line.
    ToggleTask { path: String, line: u32 },
    /// Extract the section of the heading at a position into a new note.
    ExtractSection {
        path: String,
        position: TextPosition,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCodeAction {
    pub title: String,
    pub kind: EditorActionKind,
    /// The diagnostic code this action fixes.
    pub fixes: Option<&'static str>,
    /// Whether this is the obvious fix.
    pub preferred: bool,
    pub action: EditorAction,
}

impl EditorSession {
    /// Actions available at `position` in an open document.
    pub fn code_actions(
        &self,
        path: &str,
        position: TextPosition,
    ) -> Result<Vec<EditorCodeAction>, AppError> {
        let Some(document) = self.document(path) else {
            return Ok(Vec::new());
        };
        let Some(offset) = self.offset(path, position) else {
            return Ok(Vec::new());
        };
        let mut actions = Vec::new();
        if let Some(EditorElement::Link { link, range, .. }) =
            element_at(&document.text, &document.parsed, offset)
        {
            if !matches!(link.link_kind, LinkKind::External) {
                self.link_actions(path, document, link, range, &mut actions)?;
            }
        }
        let line_start = document.text[..offset].rfind('\n').map_or(0, |at| at + 1);
        let line_end = document.text[offset..]
            .find('\n')
            .map_or(document.text.len(), |at| offset + at);
        let line = document.text[line_start..line_end].trim_end_matches('\r');
        if let Some(toggle) = toggle_inline_task_line(
            line,
            &self.config().tasks.statuses,
            "",
            RecurrencePlacement::Above,
        ) {
            actions.push(EditorCodeAction {
                title: if toggle.completed {
                    "Complete task".to_string()
                } else {
                    "Reopen task".to_string()
                },
                kind: EditorActionKind::Refactor,
                fixes: None,
                preferred: false,
                action: EditorAction::ToggleTask {
                    path: path.to_string(),
                    line: position.line,
                },
            });
        }
        if self.extractable_heading(path, position).is_some() {
            actions.push(EditorCodeAction {
                title: "Extract section into a new note".to_string(),
                kind: EditorActionKind::Extract,
                fixes: None,
                preferred: false,
                action: EditorAction::ExtractSection {
                    path: path.to_string(),
                    position,
                },
            });
        }
        Ok(actions)
    }

    fn link_actions(
        &self,
        path: &str,
        document: &super::OpenDocument,
        link: &vulcan_core::RawLink,
        range: std::ops::Range<usize>,
        actions: &mut Vec<EditorCodeAction>,
    ) -> Result<(), AppError> {
        let resolver = self.resolver()?;
        let resolution = resolver.resolve(path, link, self.config().link_resolution);
        let candidate = link.target_path_candidate.as_deref().unwrap_or_default();
        let replace = |new_range: std::ops::Range<usize>, new_text: String| EditorDocumentEdit {
            path: path.to_string(),
            version: document.version,
            edits: vec![super::edits::EditorTextEdit {
                range: self.range(&document.text, &document.line_index, new_range),
                new_text,
            }],
        };
        match resolution.problem {
            Some(LinkResolutionProblem::Unresolved) => {
                if let Some(note) = new_note_path(candidate) {
                    actions.push(EditorCodeAction {
                        title: format!("Create note {note}"),
                        kind: EditorActionKind::QuickFix,
                        fixes: Some("unresolved-link"),
                        preferred: true,
                        action: EditorAction::CreateNote { path: note },
                    });
                }
            }
            Some(LinkResolutionProblem::Ambiguous(candidates)) => {
                let raw = &document.text[range.clone()];
                let target_start = raw
                    .find(candidate)
                    .map_or(range.start, |at| range.start + at);
                for choice in candidates {
                    let target = if is_markdown_path(&choice) && raw.starts_with("[[") {
                        choice.trim_end_matches(".md").to_string()
                    } else {
                        choice.clone()
                    };
                    actions.push(EditorCodeAction {
                        title: format!("Link to {choice}"),
                        kind: EditorActionKind::QuickFix,
                        fixes: Some("ambiguous-link"),
                        preferred: false,
                        action: EditorAction::Edit(replace(
                            target_start..target_start + candidate.len(),
                            target,
                        )),
                    });
                }
            }
            Some(LinkResolutionProblem::OutsideVault) => {}
            None => {
                let Some(target) = resolution.resolved_target_id else {
                    return Ok(());
                };
                let documents = resolver
                    .documents()
                    .iter()
                    .map(|document| document.path.clone())
                    .collect::<Vec<_>>();
                if let Some(converted) = vulcan_core::move_rewrite::convert_link_style(
                    link,
                    path,
                    &target,
                    &documents,
                    self.config().link_resolution,
                ) {
                    let title = if link.raw_text.starts_with("[[") {
                        "Convert to Markdown link"
                    } else {
                        "Convert to wikilink"
                    };
                    actions.push(EditorCodeAction {
                        title: title.to_string(),
                        kind: EditorActionKind::Refactor,
                        fixes: None,
                        preferred: false,
                        action: EditorAction::Edit(replace(link_range(link), converted)),
                    });
                }
            }
        }
        Ok(())
    }

    /// Plan toggling the task on 0-based `line` of `path`, from the open
    /// buffer when there is one (never touching disk) and from the file
    /// otherwise. Completing a recurring task inserts its next occurrence.
    pub fn plan_task_toggle(
        &self,
        path: &str,
        line: u32,
        today: &str,
    ) -> Result<EditorEditPlan, AppError> {
        let open = self.document(path);
        let source = match open {
            Some(document) => document.text.clone(),
            None => std::fs::read_to_string(self.paths().vault_root().join(path))
                .map_err(|error| AppError::operation_with_code("not_found", error))?,
        };
        let placement = RecurrencePlacement::from_config(
            self.config().tasks.recurrence_on_completion.as_deref(),
        );
        let statuses = &self.config().tasks.statuses;
        let (updated, toggle) = apply_inline_task_toggle(&source, line as usize, |text| {
            toggle_inline_task_line(text, statuses, today, placement)
        })
        .ok_or_else(|| {
            AppError::operation_with_code(
                "invalid_request",
                format!("line {} of {path} is not a task", line + 1),
            )
        })?;
        let mut plan = EditorEditPlan {
            action: "toggle_task",
            report: serde_json::json!({
                "path": path,
                "line": line,
                "completed": toggle.completed,
                "after": toggle.line,
                "next_occurrence": toggle.next_occurrence,
            }),
            disk: Vec::new(),
            disk_renames: BTreeMap::new(),
            open: Vec::new(),
            editor_rename: None,
            moved: None,
        };
        match open {
            Some(document) => plan.open.push(EditorDocumentEdit {
                path: path.to_string(),
                version: document.version,
                edits: text_edits(self, &source, &updated),
            }),
            None => plan.disk.push(OrdinaryWriteChange {
                path: path.to_string(),
                before: Some(source),
                after: Some(updated),
            }),
        }
        Ok(plan)
    }

    /// Plan creating an empty note at vault path `path`.
    pub fn plan_note_create(&self, path: &str) -> Result<EditorEditPlan, AppError> {
        let path = new_note_path(path).ok_or_else(|| {
            AppError::operation_with_code("invalid_request", format!("cannot create {path}"))
        })?;
        if self.paths().vault_root().join(&path).exists() || self.document(&path).is_some() {
            return Err(AppError::operation_with_code(
                "conflict",
                format!("{path} already exists"),
            ));
        }
        Ok(EditorEditPlan {
            action: "create_note",
            report: serde_json::json!({ "path": path }),
            disk: vec![OrdinaryWriteChange {
                path,
                before: None,
                after: Some(String::new()),
            }],
            disk_renames: BTreeMap::new(),
            open: Vec::new(),
            editor_rename: None,
            moved: None,
        })
    }
}

/// The vault path of a note created for an unresolved link target: the
/// target as a vault path with `.md` added. Targets that climb out of the
/// vault or name a non-note file get no note.
fn new_note_path(target: &str) -> Option<String> {
    let target = target.trim().trim_start_matches('/');
    if target.is_empty()
        || target
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return None;
    }
    let extension = std::path::Path::new(target)
        .extension()
        .and_then(|extension| extension.to_str());
    match extension {
        Some(extension) if extension.eq_ignore_ascii_case("md") => Some(target.to_string()),
        // `[[Meeting 2026.10.12]]` names a note, but `[[photo.png]]` does not.
        Some(extension)
            if extension.chars().all(|c| c.is_ascii_alphabetic()) && extension.len() <= 5 =>
        {
            None
        }
        _ => Some(format!("{target}.md")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    fn titles(actions: &[EditorCodeAction]) -> Vec<&str> {
        actions.iter().map(|action| action.title.as_str()).collect()
    }

    #[test]
    fn offers_fixes_for_unresolved_and_ambiguous_links() {
        let text = "[[Missing]] [[Same]] [[Alpha]]\n- [ ] Task 🔁 every day 📅 2026-10-12\n";
        let (_dir, mut session) =
            vault(&[("a/Same.md", "x"), ("b/Same.md", "x"), ("Alpha.md", "x")]);
        session.open("Note.md", 3, text.into());

        let actions = session
            .code_actions("Note.md", position_of(text, "Missing", 1))
            .unwrap();
        assert_eq!(titles(&actions), vec!["Create note Missing.md"]);
        assert!(actions[0].preferred);
        assert_eq!(actions[0].fixes, Some("unresolved-link"));

        let actions = session
            .code_actions("Note.md", position_of(text, "Same", 1))
            .unwrap();
        assert_eq!(
            titles(&actions),
            vec!["Link to a/Same.md", "Link to b/Same.md"]
        );
        let EditorAction::Edit(edit) = &actions[0].action else {
            panic!("expected an edit");
        };
        assert_eq!(edit.version, 3);
        assert_eq!(edit.edits[0].new_text, "a/Same");
        assert_eq!(edit.edits[0].range.start, position_of(text, "Same]]", 0));

        let actions = session
            .code_actions("Note.md", position_of(text, "Alpha", 1))
            .unwrap();
        assert_eq!(titles(&actions), vec!["Convert to Markdown link"]);
        let EditorAction::Edit(edit) = &actions[0].action else {
            panic!("expected an edit");
        };
        assert_eq!(edit.edits[0].new_text, "[Alpha](Alpha.md)");

        let actions = session
            .code_actions("Note.md", position_of(text, "Task", 0))
            .unwrap();
        assert_eq!(titles(&actions), vec!["Complete task"]);
        assert_eq!(
            actions[0].action,
            EditorAction::ToggleTask {
                path: "Note.md".into(),
                line: 1,
            }
        );
    }

    #[test]
    fn toggles_tasks_in_open_buffers_without_touching_disk() {
        let text = "# Tasks\n- [ ] Water 🔁 every week 📅 2026-10-12\n";
        let (dir, mut session) = vault(&[("Tasks.md", "on disk\n")]);
        session.open("Tasks.md", 5, text.into());
        let plan = session
            .plan_task_toggle("Tasks.md", 1, "2026-10-13")
            .unwrap();
        assert!(plan.disk.is_empty());
        assert_eq!(plan.open[0].version, 5);
        assert_eq!(plan.report["completed"], true);
        assert_eq!(
            plan.report["next_occurrence"],
            "- [ ] Water 🔁 every week 📅 2026-10-19"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Tasks.md")).unwrap(),
            "on disk\n"
        );
        assert!(session
            .plan_task_toggle("Tasks.md", 0, "2026-10-13")
            .is_err());
    }

    #[test]
    fn toggles_tasks_in_closed_files_on_disk() {
        let (dir, session) = vault(&[("Tasks.md", "- [x] Done ✅ 2026-10-01\n")]);
        let plan = session
            .plan_task_toggle("Tasks.md", 0, "2026-10-13")
            .unwrap();
        assert!(plan.open.is_empty());
        session
            .apply_disk_changes(&plan.disk, &plan.disk_renames)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Tasks.md")).unwrap(),
            "- [ ] Done\n"
        );
    }

    #[test]
    fn creates_notes_only_where_none_exists() {
        let (dir, session) = vault(&[("Alpha.md", "x")]);
        assert!(session.plan_note_create("Alpha").is_err());
        let plan = session.plan_note_create("ideas/New").unwrap();
        session
            .apply_disk_changes(&plan.disk, &plan.disk_renames)
            .unwrap();
        assert!(dir.path().join("ideas/New.md").is_file());
        assert_eq!(new_note_path("photo.png"), None);
        assert_eq!(new_note_path("../escape"), None);
        assert_eq!(
            new_note_path("Meeting 2026.10.12").as_deref(),
            Some("Meeting 2026.10.12.md")
        );
    }
}
