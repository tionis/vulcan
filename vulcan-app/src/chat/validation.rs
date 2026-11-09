//! Proactive mdbase validation for chat users: list what fails a vault's
//! schemas, and word the problems that are new since the last check.

use super::{ChatReply, ChatVault};
use crate::mdbase::build_mdbase_validate_report;
use crate::AppError;
use vulcan_core::mdbase::{MdbaseDiagnostic, MdbaseDiagnosticLevel};
use vulcan_core::PermissionGuard;

/// The most problems one alert lists; the rest are counted.
const MAX_LISTED: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatValidationCheck {
    /// Every current problem, one line each, sorted. Store these and pass
    /// them back as `reported` so each problem is announced once.
    pub problems: Vec<String>,
    /// A message about problems missing from `reported`, if any.
    pub alert: Option<ChatReply>,
}

/// Validate `vault` as its user may read it. A vault without `mdbase.yaml`
/// has no schemas and so no problems.
pub fn chat_validation_check(
    vault: &ChatVault<'_>,
    vault_name: &str,
    reported: &[String],
) -> Result<ChatValidationCheck, AppError> {
    if !vault.paths.vault_root().join("mdbase.yaml").is_file() {
        return Ok(ChatValidationCheck {
            problems: Vec::new(),
            alert: None,
        });
    }
    let report = build_mdbase_validate_report(vault.paths, None, Some(&vault.guard.read_filter()))?;
    let mut problems = report
        .diagnostics
        .iter()
        .chain(report.records.iter().flat_map(|record| &record.diagnostics))
        .filter(|diagnostic| diagnostic.severity == MdbaseDiagnosticLevel::Error)
        .map(problem_line)
        .collect::<Vec<_>>();
    problems.sort();
    problems.dedup();
    let new = problems
        .iter()
        .filter(|problem| !reported.contains(problem))
        .collect::<Vec<_>>();
    let alert = (!new.is_empty()).then(|| {
        let mut lines = vec![format!(
            "⚠️ Notes in {vault_name} that fail their mdbase schema:"
        )];
        lines.extend(
            new.iter()
                .take(MAX_LISTED)
                .map(|problem| format!("• {problem}")),
        );
        if new.len() > MAX_LISTED {
            lines.push(format!("…and {} more.", new.len() - MAX_LISTED));
        }
        lines.push(
            "Fix the frontmatter, or run `vulcan mdbase validate` in that vault for the full list."
                .to_string(),
        );
        ChatReply::text(lines.join("\n"))
    });
    Ok(ChatValidationCheck { problems, alert })
}

fn problem_line(diagnostic: &MdbaseDiagnostic) -> String {
    let field = diagnostic
        .field
        .as_deref()
        .map(|field| field.trim_start_matches('/'))
        .filter(|field| !field.is_empty())
        .map(|field| format!(" ({field})"))
        .unwrap_or_default();
    match diagnostic.path.as_deref().filter(|path| !path.is_empty()) {
        Some(path) => format!("{path}{field}: {}", diagnostic.message),
        None => format!("{}{field}", diagnostic.message),
    }
}
