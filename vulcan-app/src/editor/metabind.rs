//! Meta Bind in editors: code actions that set the value an INPUT field
//! binds to, and the `vulcan.metabind.set` plan behind them (toggles and
//! options edit the buffer directly; other inputs ask the editor to prompt
//! for a value and run the command with it), and code actions that press a
//! button (`vulcan.metabind.press`).

use super::actions::{EditorAction, EditorActionKind, EditorCodeAction};
use super::edits::{text_edits, EditorDocumentEdit, EditorEditPlan};
use super::EditorSession;
use crate::metabind::{
    press_metabind_button, ButtonSelector, MetaBindFileWrite, MetaBindPressOptions, MetaBindWriter,
};
use crate::AppError;
use std::collections::{BTreeMap, HashMap};
use vulcan_core::metabind::buttons::find_button;
use vulcan_core::metabind::{
    fields_in_source, inputs_bound_to, write_target_in_source, BindTarget, MetaBindDeclaration,
    MetaBindEnv, MetaBindOutcome, MetaBindStorage,
};
use vulcan_core::ordinary_write::OrdinaryWriteChange;
use vulcan_core::NoteRecord;

type NoIndex = HashMap<String, NoteRecord>;

/// A dry-run press writes nothing.
struct NoWrites;

impl MetaBindWriter for NoWrites {
    fn write(&mut self, _write: &MetaBindFileWrite) -> Result<(), AppError> {
        Ok(())
    }
}

fn press_action(
    path: &str,
    label: &str,
    button: Option<String>,
    line: Option<usize>,
) -> EditorCodeAction {
    EditorCodeAction {
        title: format!("Press {label}"),
        kind: EditorActionKind::Refactor,
        fixes: None,
        preferred: false,
        action: EditorAction::MetaBindPress {
            path: path.to_string(),
            button,
            line,
        },
    }
}

impl EditorSession {
    /// Actions for the Meta Bind input at `offset` in an open document.
    pub(super) fn metabind_actions(
        &self,
        path: &str,
        offset: usize,
        actions: &mut Vec<EditorCodeAction>,
    ) {
        let Some(document) = self.document(path) else {
            return;
        };
        let fields = fields_in_source::<std::hash::RandomState>(
            self.config(),
            None::<&NoIndex>,
            Some(path),
            &document.text,
            &MetaBindEnv {
                paths: Some(self.paths()),
                ..MetaBindEnv::with_clock()
            },
        );
        let Some(field) = fields
            .iter()
            .find(|field| field.byte_range.contains(&offset))
        else {
            return;
        };
        match &field.parsed {
            Some(MetaBindDeclaration::Button { button }) => {
                let label = button
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("button");
                actions.push(press_action(path, label, None, Some(field.line_number)));
                return;
            }
            Some(MetaBindDeclaration::Buttons { ids }) => {
                for id in ids {
                    actions.push(press_action(path, id, Some(id.clone()), None));
                }
                return;
            }
            _ => {}
        }
        let (
            Some(MetaBindDeclaration::Input(declaration)),
            MetaBindOutcome::Input {
                settings, value, ..
            },
        ) = (&field.parsed, &field.outcome)
        else {
            return;
        };
        let Some(target) = declaration.target.as_ref().filter(|target| {
            target.storage == MetaBindStorage::Frontmatter && target.file.is_none()
        }) else {
            return;
        };
        let choices = settings.choices();
        if choices.is_empty() {
            actions.push(EditorCodeAction {
                title: format!("Set {target}…"),
                kind: EditorActionKind::Refactor,
                fixes: None,
                preferred: false,
                action: EditorAction::MetaBindPrompt {
                    path: path.to_string(),
                    target: target.to_string(),
                    input_type: settings.input_type.clone(),
                    current: value.clone(),
                },
            });
            return;
        }
        for choice in choices.into_iter().filter(|choice| choice.value != *value) {
            let Ok(Some(updated)) =
                write_target_in_source(&document.text, path, target, &choice.value)
            else {
                continue;
            };
            let title = if settings.input_type == "toggle" {
                format!("Turn {target} {}", choice.label)
            } else {
                format!("Set {target} to {}", choice.label)
            };
            actions.push(EditorCodeAction {
                title,
                kind: EditorActionKind::Refactor,
                fixes: None,
                preferred: settings.input_type == "toggle",
                action: EditorAction::Edit(EditorDocumentEdit {
                    path: path.to_string(),
                    version: document.version,
                    edits: text_edits(self, &document.text, &updated),
                }),
            });
        }
    }

    /// Plans pressing a button in the note at `path` (open or on disk):
    /// its edits, applied to open documents in the editor. Buttons with
    /// JavaScript actions write through the vault API, so they are pressed
    /// with `vulcan metabind press` instead.
    pub fn plan_metabind_press(
        &self,
        path: &str,
        selector: &ButtonSelector,
    ) -> Result<EditorEditPlan, AppError> {
        let open = self.document(path);
        let source = match open {
            Some(document) => document.text.clone(),
            None => std::fs::read_to_string(self.paths().vault_root().join(path))
                .map_err(|error| AppError::operation_with_code("not_found", error))?,
        };
        let invalid = |message: String| AppError::operation_with_code("invalid_request", message);
        let parsed = vulcan_core::parse_document(&source, self.config());
        let button =
            find_button(&parsed, &source, &self.config().metabind, selector).map_err(invalid)?;
        let scripted = button.actions().map_err(invalid)?.iter().any(|action| {
            matches!(
                action.get("type").and_then(serde_json::Value::as_str),
                Some("js" | "inlineJS")
            )
        });
        if scripted {
            return Err(invalid(
                "this button runs JavaScript; press it with `vulcan metabind press`".to_string(),
            ));
        }
        let report = press_metabind_button(
            self.paths(),
            self.config(),
            path,
            &source,
            selector,
            MetaBindPressOptions {
                dry_run: true,
                permission_profile: None,
            },
            &mut NoWrites,
        )
        .map_err(|error| AppError::operation_with_code("invalid_request", error))?;
        let mut plan = EditorEditPlan {
            action: "metabind_press",
            report: serde_json::to_value(&report).map_err(AppError::operation)?,
            disk: Vec::new(),
            disk_renames: BTreeMap::new(),
            open: Vec::new(),
            editor_rename: None,
            moved: None,
        };
        for write in report.writes {
            match self.document(&write.path) {
                Some(document) if !write.created => plan.open.push(EditorDocumentEdit {
                    path: write.path.clone(),
                    version: document.version,
                    edits: text_edits(self, &document.text, &write.after),
                }),
                _ => plan.disk.push(OrdinaryWriteChange {
                    path: write.path,
                    before: (!write.created).then_some(write.before),
                    after: Some(write.after),
                }),
            }
        }
        Ok(plan)
    }

    /// Plans writing `value` to `target` in the note at `path` (open or on
    /// disk), checked against every input in the note bound to `target`.
    pub fn plan_metabind_set(
        &self,
        path: &str,
        target: &str,
        value: &str,
    ) -> Result<EditorEditPlan, AppError> {
        let open = self.document(path);
        let source = match open {
            Some(document) => document.text.clone(),
            None => std::fs::read_to_string(self.paths().vault_root().join(path))
                .map_err(|error| AppError::operation_with_code("not_found", error))?,
        };
        let invalid = |message: String| AppError::operation_with_code("invalid_request", message);
        let target = BindTarget::parse(target).map_err(invalid)?;
        if target.storage != MetaBindStorage::Frontmatter || target.file.is_some() {
            return Err(invalid(format!(
                "editors set this note's frontmatter targets; use `vulcan metabind set` for `{target}`"
            )));
        }
        let fields = fields_in_source::<std::hash::RandomState>(
            self.config(),
            None::<&NoIndex>,
            Some(path),
            &source,
            &MetaBindEnv {
                paths: Some(self.paths()),
                ..MetaBindEnv::with_clock()
            },
        );
        let inputs = inputs_bound_to(&fields, &target);
        let first = inputs
            .first()
            .ok_or_else(|| invalid(format!("no input in {path} is bound to `{target}`")))?;
        let parsed = first.parse_value(value).map_err(invalid)?;
        for input in &inputs[1..] {
            input.parse_value(value).map_err(invalid)?;
        }
        let updated = write_target_in_source(&source, path, &target, &parsed)
            .map_err(invalid)?
            .unwrap_or_else(|| source.clone());
        let mut plan = EditorEditPlan {
            action: "metabind_set",
            report: serde_json::json!({
                "path": path,
                "target": target.to_string(),
                "value": parsed,
                "changed": updated != source,
            }),
            disk: Vec::new(),
            disk_renames: BTreeMap::new(),
            open: Vec::new(),
            editor_rename: None,
            moved: None,
        };
        if updated != source {
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
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn buttons_offer_a_press_that_edits_the_open_document() {
        let text = concat!(
            "---\ncount: 1\n---\n",
            "```meta-bind-button\nlabel: Bump\nid: bump\naction:\n",
            "  type: updateMetadata\n  bindTarget: count\n  value: 2\n```\n",
            "`BUTTON[bump]`\n",
        );
        let (_dir, mut session) = vault(&[]);
        session.open("Note.md", 4, text.into());
        let actions = session
            .code_actions("Note.md", position_of(text, "label", 0))
            .unwrap();
        assert_eq!(actions[0].title, "Press Bump");
        assert_eq!(
            actions[0].action,
            EditorAction::MetaBindPress {
                path: "Note.md".into(),
                button: None,
                line: Some(4),
            }
        );
        let actions = session
            .code_actions("Note.md", position_of(text, "BUTTON", 0))
            .unwrap();
        assert_eq!(actions[0].title, "Press bump");

        let plan = session
            .plan_metabind_press("Note.md", &ButtonSelector::Line(4))
            .unwrap();
        assert!(plan.disk.is_empty());
        assert_eq!(plan.open[0].version, 4);
        assert_eq!(plan.open[0].edits[0].new_text, "2");

        let scripted =
            "```meta-bind-button\nlabel: JS\nid: js\naction:\n  type: inlineJS\n  code: 1\n```\n";
        session.open("Script.md", 1, scripted.into());
        let error = session
            .plan_metabind_press("Script.md", &ButtonSelector::Id("js".into()))
            .unwrap_err();
        assert!(
            error.to_string().contains("vulcan metabind press"),
            "{error}"
        );
    }

    #[test]
    fn inputs_offer_values_and_prompts() {
        let text = concat!(
            "---\ndone: false\nclass: rogue\nhp: 3\n---\n",
            "`INPUT[toggle:done]` `INPUT[inlineSelect(option(rogue, Rogue), option(mage, Mage)):class]` ",
            "`INPUT[number:hp]`\n",
        );
        let (_dir, mut session) = vault(&[]);
        session.open("Note.md", 2, text.into());

        let actions = session
            .code_actions("Note.md", position_of(text, "toggle", 0))
            .unwrap();
        assert_eq!(actions[0].title, "Turn done on");
        let EditorAction::Edit(edit) = &actions[0].action else {
            panic!("expected an edit");
        };
        assert_eq!(edit.version, 2);
        // The minimal edit turns `false` into `true`, keeping the shared `e`.
        assert_eq!(edit.edits[0].new_text, "tru");
        assert_eq!(edit.edits[0].range.start.line, 1);

        let actions = session
            .code_actions("Note.md", position_of(text, "inlineSelect", 0))
            .unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Set class to Mage");

        let actions = session
            .code_actions("Note.md", position_of(text, "number", 0))
            .unwrap();
        assert_eq!(
            actions[0].action,
            EditorAction::MetaBindPrompt {
                path: "Note.md".into(),
                target: "hp".into(),
                input_type: "number".into(),
                current: serde_json::json!(3),
            }
        );

        let plan = session.plan_metabind_set("Note.md", "hp", "12").unwrap();
        assert!(
            plan.disk.is_empty(),
            "open buffers are edited in the editor"
        );
        assert_eq!(plan.open[0].edits[0].new_text, "12");
        assert!(session.plan_metabind_set("Note.md", "hp", "lots").is_err());
    }
}
