//! Planning template insertion and periodic notes for editors.
//!
//! Both follow the open/closed routing of every editor mutation: an open
//! buffer is edited through the editor, never on disk.

use super::edits::{text_edits, EditorDocumentEdit, EditorEditPlan};
use super::EditorSession;
use crate::templates::{
    find_frontmatter_block, load_named_template, prepare_template_insertion,
    render_loaded_template_with_filter, render_note_from_parts, LoadedTemplateRenderRequest,
    TemplateEngineKind, TemplateInsertMode, TemplateRunMode,
};
use crate::AppError;
use std::collections::{BTreeMap, HashMap};
use vulcan_core::editor::TextPosition;
use vulcan_core::ordinary_write::OrdinaryWriteChange;

/// Where an inserted template goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorTemplatePlacement {
    Append,
    Prepend,
    /// At a position in the document body; template frontmatter still merges
    /// into the note's frontmatter.
    At(TextPosition),
}

impl EditorSession {
    /// Plan inserting the named template into `path`, rendered against the
    /// document's current text (the open buffer, else the file).
    ///
    /// Templates render with mutations disabled: a template that creates or
    /// moves files (`tp.file.create_new`, `tp.file.move`) fails here and
    /// should be run with `vulcan template insert` instead.
    pub fn plan_template_insert(
        &self,
        path: &str,
        template: &str,
        placement: EditorTemplatePlacement,
    ) -> Result<EditorEditPlan, AppError> {
        let open = self.document(path);
        let source = match open {
            Some(document) => document.text.clone(),
            None => std::fs::read_to_string(self.paths().vault_root().join(path))
                .map_err(|error| AppError::operation_with_code("not_found", error))?,
        };
        let loaded = load_named_template(self.paths(), self.config(), template)?;
        let read_filter = self.guard().map(vulcan_core::PermissionGuard::read_filter);
        let rendered = render_loaded_template_with_filter(
            self.paths(),
            self.config(),
            &loaded,
            &LoadedTemplateRenderRequest {
                target_path: path,
                target_contents: Some(&source),
                engine: TemplateEngineKind::default(),
                vars: &HashMap::new(),
                allow_mutations: false,
                run_mode: TemplateRunMode::Append,
                reference_date: None,
            },
            read_filter.as_ref(),
        )?;
        if rendered.target_path != path {
            return Err(AppError::operation_with_code(
                "invalid_request",
                "the template moves its note; run `vulcan template insert` instead",
            ));
        }
        let prepared =
            prepare_template_insertion(&source, &rendered.content).map_err(AppError::operation)?;
        let updated = match placement {
            EditorTemplatePlacement::Append => crate::templates::apply_template_insertion_mode(
                &prepared,
                TemplateInsertMode::Append,
            ),
            EditorTemplatePlacement::Prepend => crate::templates::apply_template_insertion_mode(
                &prepared,
                TemplateInsertMode::Prepend,
            ),
            EditorTemplatePlacement::At(position) => {
                let offset = self.offset(path, position).unwrap_or(source.len());
                let body_start = find_frontmatter_block(&source).map_or(0, |(_, _, end)| end);
                let at = offset
                    .saturating_sub(body_start)
                    .min(prepared.target_body.len());
                let mut body = prepared.target_body.clone();
                body.insert_str(at, &prepared.template_body);
                render_note_from_parts(prepared.merged_frontmatter.as_ref(), &body)
            }
        }
        .map_err(AppError::operation)?;

        let mut plan = empty_plan(
            "template_insert",
            serde_json::json!({
                "template": loaded.template.name,
                "note": path,
                "engine": rendered.engine.as_str(),
                "warnings": rendered.warnings,
                "diagnostics": rendered.diagnostics,
            }),
        );
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

    /// Plan opening a periodic note (`daily`, `weekly`, ...) for `date`
    /// (default today): its path, and its creation from the configured
    /// template when it does not exist yet.
    pub fn plan_periodic_open(
        &self,
        period_type: &str,
        date: Option<&str>,
    ) -> Result<EditorEditPlan, AppError> {
        let target = crate::periodic::resolve_periodic_target(
            &self.config().periodic,
            period_type,
            date,
            true,
        )?;
        let absolute = self.paths().vault_root().join(&target.path);
        let exists = absolute.is_file() || self.document(&target.path).is_some();
        if !exists && absolute.exists() {
            return Err(AppError::operation_with_code(
                "conflict",
                format!("{} exists but is not a note file", target.path),
            ));
        }
        let mut warnings = Vec::new();
        let mut plan = empty_plan("periodic_open", serde_json::Value::Null);
        if !exists {
            let contents = crate::notes::render_periodic_note_contents(
                self.paths(),
                period_type,
                &target.path,
                &mut warnings,
                self.permission_profile(),
            )?;
            plan.disk.push(OrdinaryWriteChange {
                path: target.path.clone(),
                before: None,
                after: Some(contents),
            });
        }
        plan.report = serde_json::json!({
            "period_type": target.period_type,
            "reference_date": target.reference_date,
            "start_date": target.start_date,
            "end_date": target.end_date,
            "path": target.path,
            "created": !exists,
            "warnings": warnings,
        });
        Ok(plan)
    }
}

fn empty_plan(action: &'static str, report: serde_json::Value) -> EditorEditPlan {
    EditorEditPlan {
        action,
        report,
        disk: Vec::new(),
        disk_renames: BTreeMap::new(),
        open: Vec::new(),
        editor_rename: None,
        moved: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn inserts_templates_into_open_buffers_at_the_cursor() {
        let text = "---\ntags: [a]\n---\n# Note\n\nbefore after\n";
        let (dir, mut session) = vault(&[
            ("Note.md", "on disk\n"),
            (
                ".vulcan/templates/Snippet.md",
                "---\ntags: [b]\n---\n**{{title}}**",
            ),
        ]);
        session.open("Note.md", 2, text.into());
        let plan = session
            .plan_template_insert(
                "Note.md",
                "Snippet",
                EditorTemplatePlacement::At(position_of(text, "after", 0)),
            )
            .unwrap();
        assert!(plan.disk.is_empty());
        assert_eq!(plan.open[0].version, 2);
        let edited = plan.open[0]
            .edits
            .iter()
            .map(|edit| edit.new_text.as_str())
            .collect::<String>();
        assert!(edited.contains("**Note**"), "{edited}");
        assert!(edited.contains("- b"), "frontmatter merges: {edited}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Note.md")).unwrap(),
            "on disk\n"
        );
        assert!(session
            .plan_template_insert("Note.md", "Missing", EditorTemplatePlacement::Append)
            .is_err());
    }

    #[test]
    fn appends_templates_to_closed_files_on_disk() {
        let (dir, session) = vault(&[
            ("Note.md", "# Note\n"),
            (".vulcan/templates/Footer.md", "Footer for {{title}}"),
        ]);
        let plan = session
            .plan_template_insert("Note.md", "Footer", EditorTemplatePlacement::Append)
            .unwrap();
        session
            .apply_disk_changes(&plan.disk, &plan.disk_renames)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Note.md")).unwrap(),
            "# Note\n\nFooter for Note\n"
        );
    }

    #[test]
    fn opens_existing_periodic_notes_and_plans_missing_ones() {
        let (dir, session) = vault(&[("Journal/2026-10-12.md", "# Monday\n")]);
        std::fs::write(
            dir.path().join(".vulcan/config.toml"),
            "[periodic.daily]\nfolder = \"Journal\"\nformat = \"YYYY-MM-DD\"\n",
        )
        .unwrap();
        let session = EditorSession::new(session.paths().clone(), session.encoding());
        let existing = session
            .plan_periodic_open("daily", Some("2026-10-12"))
            .unwrap();
        assert_eq!(existing.report["path"], "Journal/2026-10-12.md");
        assert_eq!(existing.report["created"], false);
        assert!(existing.disk.is_empty());

        let missing = session
            .plan_periodic_open("daily", Some("2026-10-13"))
            .unwrap();
        assert_eq!(missing.report["created"], true);
        assert_eq!(missing.disk[0].path, "Journal/2026-10-13.md");
        assert!(session.plan_periodic_open("fortnightly", None).is_err());
    }
}
