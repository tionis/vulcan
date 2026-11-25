//! Meta Bind inputs in editors: code actions that set the value an INPUT
//! field binds to, and the `vulcan.metabind.set` plan behind them. Toggles
//! and options edit the buffer directly; other inputs ask the editor to
//! prompt for a value and run the command with it.

use super::actions::{EditorAction, EditorActionKind, EditorCodeAction};
use super::edits::{text_edits, EditorDocumentEdit, EditorEditPlan};
use super::EditorSession;
use crate::AppError;
use std::collections::{BTreeMap, HashMap};
use vulcan_core::metabind::{
    fields_in_source, inputs_bound_to, write_target_in_source, BindTarget, MetaBindDeclaration,
    MetaBindOutcome, MetaBindStorage,
};
use vulcan_core::ordinary_write::OrdinaryWriteChange;
use vulcan_core::NoteRecord;

type NoIndex = HashMap<String, NoteRecord>;

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
        );
        let Some(field) = fields
            .iter()
            .find(|field| field.byte_range.contains(&offset))
        else {
            return;
        };
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
