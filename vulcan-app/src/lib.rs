//! Reusable application-layer workflow services for Vulcan.
//!
//! `vulcan-core` owns vault semantics, parsing, indexing, config models, and
//! synchronous domain logic. `vulcan-app` composes those primitives into
//! reusable workflows that may touch the filesystem or coordinate multiple
//! core operations without pulling in CLI/TUI concerns.
//!
//! Depend on this crate when you are building a local integration, daemon, MCP
//! handler, or test harness that wants Vulcan's higher-level workflows without
//! terminal rendering or `clap` parsing. The app layer remains synchronous and
//! returns typed reports that callers can serialize as JSON, display in a CLI,
//! or expose through future daemon endpoints.
//!
//! Common entrypoints:
//!
//! - Full local workflows: `notes`, `scan`, `browse`, `config`, `trust`, and
//!   `plugins`.
//! - Static export/site generation: `export`, `site`, and `serve`.
//! - Task and template automation: `tasks` and `templates`.
//! - Agent skill commands and custom tools: `tools`, which handles discovery,
//!   schema validation, permission ceilings, execution, linting, and authoring
//!   reports.
//! - Optional web workflows: `web`, enabled by the `web` feature.
//!
//! `vulcan-app` should not contain TUI state, terminal styling, editor/browser
//! launching, or direct stdout/stderr rendering. Those concerns belong in
//! `vulcan-cli` or a future daemon transport adapter.

mod error;

pub mod browse;
pub mod config;
pub mod export;
pub mod notes;
pub mod plugins;
pub mod scan;
pub mod serve;
pub mod site;
pub mod tasks;
pub mod templates;
pub mod tools;
pub mod trust;
#[cfg(feature = "web")]
pub mod web;

pub use error::AppError;

#[cfg(test)]
mod contract_tests {
    use crate::browse::VaultStatusReport;
    use crate::notes::NoteAppendReport;
    use crate::tasks::TaskMutationReport;
    use serde_json::json;

    #[test]
    fn reusable_app_reports_keep_stable_json_contracts() {
        let append = NoteAppendReport {
            path: "Daily/2026-05-11.md".to_string(),
            mode: "after_heading".to_string(),
            created: true,
            heading: Some("Log".to_string()),
            period_type: Some("daily".to_string()),
            reference_date: Some("2026-05-11".to_string()),
            warnings: vec!["created missing note".to_string()],
            changed_paths: vec!["Daily/2026-05-11.md".to_string()],
            content: "# 2026-05-11\n\n## Log\n- hello\n".to_string(),
        };

        assert_eq!(
            serde_json::to_value(&append).expect("note append report serializes"),
            json!({
                "path": "Daily/2026-05-11.md",
                "mode": "after_heading",
                "created": true,
                "heading": "Log",
                "period_type": "daily",
                "reference_date": "2026-05-11",
                "warnings": ["created missing note"]
            })
        );

        let status = VaultStatusReport {
            vault_root: "/vault".to_string(),
            note_count: 12,
            attachment_count: 3,
            last_scan: Some("2026-05-11T12:00:00Z".to_string()),
            cache_bytes: 4096,
            git_branch: Some("main".to_string()),
            git_dirty: true,
            git_staged: 1,
            git_unstaged: 2,
            git_untracked: 3,
            graph_confidence: None,
        };

        assert_eq!(
            serde_json::to_value(&status).expect("status report serializes"),
            json!({
                "vault_root": "/vault",
                "note_count": 12,
                "attachment_count": 3,
                "last_scan": "2026-05-11T12:00:00Z",
                "cache_bytes": 4096,
                "git_branch": "main",
                "git_dirty": true,
                "git_staged": 1,
                "git_unstaged": 2,
                "git_untracked": 3,
                "graph_confidence": null
            })
        );

        let mutation = TaskMutationReport {
            action: "complete".to_string(),
            dry_run: false,
            path: "Tasks/Call Alex.md".to_string(),
            moved_from: None,
            moved_to: None,
            changes: Vec::new(),
            changed_paths: vec!["Tasks/Call Alex.md".to_string()],
        };

        assert_eq!(
            serde_json::to_value(&mutation).expect("task mutation report serializes"),
            json!({
                "action": "complete",
                "dry_run": false,
                "path": "Tasks/Call Alex.md",
                "moved_from": null,
                "moved_to": null,
                "changes": []
            })
        );
    }
}
