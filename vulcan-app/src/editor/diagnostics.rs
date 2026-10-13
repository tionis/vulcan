//! Diagnostics for open documents: parse problems and link resolution.
//!
//! The categories match `vulcan doctor`, computed on the unsaved text with
//! open buffers counted as existing notes.

use super::{EditorRange, EditorSession};
use crate::AppError;
use std::ops::Range;
use vulcan_core::editor::LineIndex;
use vulcan_core::editor::{completion_context, link_range, CompletionContext};
use vulcan_core::resolver::LinkResolutionProblem;
use vulcan_core::ParsedDocument;
use vulcan_core::{LinkKind, ParseDiagnosticKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorDiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorDiagnostic {
    pub range: EditorRange,
    pub severity: EditorDiagnosticSeverity,
    /// A stable kebab-case code such as `unresolved-link`.
    pub code: &'static str,
    pub message: String,
    /// Candidate paths of an ambiguous link.
    pub candidates: Vec<String>,
}

impl EditorSession {
    /// Diagnostics for an open document, or `None` when it is not open.
    ///
    /// `cursor` is the editor's cursor, if known: a link still being typed
    /// there is not reported as unresolved.
    pub fn diagnostics(
        &self,
        path: &str,
        cursor: Option<vulcan_core::editor::TextPosition>,
    ) -> Result<Option<Vec<EditorDiagnostic>>, AppError> {
        let Some(document) = self.document(path) else {
            return Ok(None);
        };
        let text = &document.text;
        let typing = cursor
            .and_then(|cursor| self.offset(path, cursor))
            .and_then(|offset| completion_context(text, offset).map(|context| (offset, context)))
            .and_then(|(offset, context)| match context {
                CompletionContext::LinkTarget { range, .. }
                | CompletionContext::LinkHeading { range, .. }
                | CompletionContext::LinkBlock { range, .. } => Some(range.start..offset),
                _ => None,
            });
        self.text_diagnostics(
            path,
            text,
            &document.parsed,
            &document.line_index,
            typing.as_ref(),
        )
        .map(Some)
    }

    /// Diagnostics for closed notes the index reports problems in, read
    /// from disk. Open documents are left to [`Self::diagnostics`], and
    /// notes the permission profile cannot read are skipped.
    pub fn workspace_diagnostics(&self) -> Result<Vec<(String, Vec<EditorDiagnostic>)>, AppError> {
        if !self.has_cache() {
            return Ok(Vec::new());
        }
        let mut reports = Vec::new();
        for path in
            vulcan_core::documents_with_diagnostics(self.paths()).map_err(AppError::operation)?
        {
            if self.document(&path).is_some() {
                continue;
            }
            let Some(view) = self.view(&path) else {
                continue;
            };
            let diagnostics =
                self.text_diagnostics(&path, &view.text, &view.parsed, &view.line_index, None)?;
            if !diagnostics.is_empty() {
                reports.push((path, diagnostics));
            }
        }
        Ok(reports)
    }

    fn text_diagnostics(
        &self,
        path: &str,
        text: &str,
        parsed: &ParsedDocument,
        index: &LineIndex,
        typing: Option<&Range<usize>>,
    ) -> Result<Vec<EditorDiagnostic>, AppError> {
        let mut diagnostics = Vec::new();
        let report_unsupported = self.config().editor.diagnose_unsupported_syntax;
        for diagnostic in &parsed.diagnostics {
            if diagnostic.kind == ParseDiagnosticKind::UnsupportedSyntax && !report_unsupported {
                continue;
            }
            let (severity, code) = match diagnostic.kind {
                ParseDiagnosticKind::MalformedFrontmatter => {
                    (EditorDiagnosticSeverity::Error, "malformed-frontmatter")
                }
                ParseDiagnosticKind::ResourceLimit => {
                    (EditorDiagnosticSeverity::Warning, "resource-limit")
                }
                ParseDiagnosticKind::UnsupportedSyntax => {
                    (EditorDiagnosticSeverity::Information, "unsupported-syntax")
                }
                ParseDiagnosticKind::HtmlLink => (EditorDiagnosticSeverity::Hint, "html-link"),
                ParseDiagnosticKind::LinkInComment => {
                    (EditorDiagnosticSeverity::Hint, "link-in-comment")
                }
            };
            let range = diagnostic
                .byte_range
                .clone()
                .unwrap_or_else(|| 0..text.find('\n').unwrap_or(text.len()));
            diagnostics.push(EditorDiagnostic {
                range: self.range(text, index, range),
                severity,
                code,
                message: diagnostic.message.clone(),
                candidates: Vec::new(),
            });
        }

        if !self.has_cache() {
            // Without an index every link would look unresolved.
            diagnostics.sort_by_key(|diagnostic| diagnostic.range.start);
            return Ok(diagnostics);
        }
        let resolver = self.resolver()?;
        for link in &parsed.links {
            if matches!(link.link_kind, LinkKind::External) {
                continue;
            }
            let range = link_range(link);
            if typing.is_some_and(|typing| range.start <= typing.start && typing.end <= range.end) {
                continue;
            }
            let target = link.target_path_candidate.as_deref().unwrap_or_default();
            let resolution = resolver.resolve(path, link, self.config().link_resolution);
            let (code, message, candidates) = match resolution.problem {
                None => continue,
                Some(LinkResolutionProblem::Unresolved) => (
                    "unresolved-link",
                    format!("Unresolved link to `{target}`"),
                    Vec::new(),
                ),
                Some(LinkResolutionProblem::Ambiguous(candidates)) => (
                    "ambiguous-link",
                    format!(
                        "Ambiguous link `{target}` matches {} files",
                        candidates.len()
                    ),
                    candidates,
                ),
                Some(LinkResolutionProblem::OutsideVault) => (
                    "outside-vault-link",
                    format!("Link `{target}` points outside the vault"),
                    Vec::new(),
                ),
            };
            diagnostics.push(EditorDiagnostic {
                range: self.range(text, index, range),
                severity: EditorDiagnosticSeverity::Warning,
                code,
                message,
                candidates,
            });
        }
        diagnostics.sort_by_key(|diagnostic| diagnostic.range.start);
        Ok(diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use vulcan_core::editor::TextPosition;

    #[test]
    fn reports_parse_and_link_problems() {
        let (_dir, mut session) =
            vault(&[("a/Same.md", "x"), ("b/Same.md", "x"), ("Alpha.md", "x")]);
        let text = "---\na: [\n---\n[[Alpha]] [[Missing]] [[Same]] [[Draft]]\n";
        session.open("Note.md", 1, text.into());
        session.open("Draft.md", 1, String::new());
        let diagnostics = session.diagnostics("Note.md", None).unwrap().unwrap();
        let codes = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            vec!["malformed-frontmatter", "unresolved-link", "ambiguous-link"]
        );
        assert_eq!(
            diagnostics[1].range.start,
            position_of(text, "[[Missing", 0)
        );
        assert_eq!(diagnostics[2].candidates, vec!["a/Same.md", "b/Same.md"]);
        assert_eq!(session.diagnostics("Closed.md", None).unwrap(), None);
    }

    #[test]
    fn reports_closed_notes_from_disk() {
        let (_dir, mut session) = vault(&[
            ("Alpha.md", "x"),
            ("Broken.md", "ok\n[[Gone]]\n"),
            ("Open.md", "[[Gone]]\n"),
        ]);
        session.open("Open.md", 1, "[[Gone]]\n".into());
        let reports = session.workspace_diagnostics().unwrap();
        assert_eq!(reports.len(), 1, "{reports:?}");
        let (path, diagnostics) = &reports[0];
        assert_eq!(path, "Broken.md");
        assert_eq!(diagnostics[0].code, "unresolved-link");
        assert_eq!(
            diagnostics[0].range.start,
            TextPosition { line: 1, column: 0 }
        );
    }

    #[test]
    fn skips_the_link_being_typed() {
        let (_dir, mut session) = vault(&[("Alpha.md", "x")]);
        let text = "[[Alp]] and [[Gone]]\n";
        session.open("Note.md", 1, text.into());
        let cursor = position_of(text, "Alp", 3);
        let diagnostics = session
            .diagnostics("Note.md", Some(cursor))
            .unwrap()
            .unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].range.start, position_of(text, "[[Gone", 0));
        let elsewhere = TextPosition { line: 0, column: 0 };
        assert_eq!(
            session
                .diagnostics("Note.md", Some(elsewhere))
                .unwrap()
                .unwrap()
                .len(),
            2
        );
    }
}
