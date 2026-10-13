//! Code lenses and inlay hints: small facts shown inline in the editor.

use super::{EditorRange, EditorSession};
use crate::AppError;
use vulcan_core::editor::TextPosition;
use vulcan_core::tasks::{days_between, task_line_due_date};

/// A clickable annotation above a range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCodeLens {
    pub range: EditorRange,
    pub title: String,
    /// The number of notes linking here (not links), for a backlink lens.
    pub backlinks: usize,
}

/// A short label shown inside a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorInlayHint {
    pub position: TextPosition,
    pub label: String,
}

impl EditorSession {
    /// A backlink-count lens on the first line, when `[editor] code_lens`
    /// is on and the note is indexed.
    pub fn code_lenses(&self, path: &str) -> Result<Vec<EditorCodeLens>, AppError> {
        if !self.config().editor.code_lens {
            return Ok(Vec::new());
        }
        let Some(report) = self.backlinks_report(path)? else {
            return Ok(Vec::new());
        };
        let count = report
            .backlinks
            .iter()
            .map(|backlink| backlink.source_path.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let start = TextPosition { line: 0, column: 0 };
        Ok(vec![EditorCodeLens {
            range: EditorRange { start, end: start },
            title: format!("{count} backlink{}", if count == 1 { "" } else { "s" }),
            backlinks: count,
        }])
    }

    /// How far off each open task's due date is, at the end of its line,
    /// when `[editor] inlay_hints` is on.
    #[must_use]
    pub fn inlay_hints(&self, path: &str, today: &str) -> Vec<EditorInlayHint> {
        if !self.config().editor.inlay_hints {
            return Vec::new();
        }
        let Some(document) = self.document(path) else {
            return Vec::new();
        };
        let statuses = &self.config().tasks.statuses;
        let mut hints = Vec::new();
        for (index, line) in document.text.split('\n').enumerate() {
            let line = line.trim_end_matches('\r');
            let Some(status) = checkbox_status(line) else {
                continue;
            };
            let closed = statuses.completed.iter().any(|symbol| symbol == status)
                || statuses.cancelled.iter().any(|symbol| symbol == status);
            if closed {
                continue;
            }
            let Some(days) = task_line_due_date(line).and_then(|due| days_between(today, &due))
            else {
                continue;
            };
            let label = match days {
                0 => "due today".to_string(),
                1 => "due tomorrow".to_string(),
                -1 => "overdue by 1 day".to_string(),
                days if days < 0 => format!("overdue by {} days", -days),
                days => format!("due in {days} days"),
            };
            let offset_in_text = document
                .text
                .split('\n')
                .take(index)
                .map(|line| line.len() + 1)
                .sum::<usize>()
                + line.len();
            hints.push(EditorInlayHint {
                position: document.line_index.position(
                    &document.text,
                    offset_in_text,
                    self.encoding(),
                ),
                label,
            });
        }
        hints
    }
}

/// The status symbol of a Markdown task line, if it is one.
fn checkbox_status(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let trimmed = trimmed.trim_start_matches(|c: char| c == '>' || c.is_whitespace());
    let rest = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .or_else(|| {
            let digits = trimmed.find(|c: char| !c.is_ascii_digit())?;
            (digits > 0).then(|| &trimmed[digits..]).and_then(|after| {
                after
                    .strip_prefix(". ")
                    .or_else(|| after.strip_prefix(") "))
            })
        })?;
    let rest = rest.trim_start().strip_prefix('[')?;
    let end = rest.char_indices().nth(1).map(|(index, _)| index)?;
    rest[end..].starts_with(']').then_some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;

    #[test]
    fn hints_due_dates_of_open_tasks() {
        let text = "- [ ] today 📅 2026-10-13\n- [ ] later [due:: 2026-10-16]\n- [x] done 📅 2026-10-01\n- [ ] late 📅 2026-10-10\n1. [ ] numbered 📅 2026-10-14\nplain 📅 2026-10-14\n";
        let (_dir, mut session) = vault(&[("Tasks.md", text)]);
        session.open("Tasks.md", 1, text.into());
        let hints = session.inlay_hints("Tasks.md", "2026-10-13");
        let labels = hints
            .iter()
            .map(|hint| hint.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                "due today",
                "due in 3 days",
                "overdue by 3 days",
                "due tomorrow"
            ]
        );
        assert_eq!(hints[0].position, position_of(text, "\n- [ ] later", 0));
    }

    #[test]
    fn counts_backlinks_in_a_code_lens() {
        let (_dir, session) = vault(&[
            ("Home.md", "# Home\n"),
            ("A.md", "[[Home]]\n"),
            ("B.md", "[[Home]] and [[Home#Section]]\n"),
        ]);
        let lenses = session.code_lenses("Home.md").unwrap();
        assert_eq!(lenses.len(), 1);
        // Two notes link here; B links twice.
        assert_eq!(lenses[0].title, "2 backlinks");
        assert!(session.code_lenses("Missing.md").unwrap().is_empty());
    }
}
