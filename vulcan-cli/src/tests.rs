use super::*;
use crate::commands::completions::{
    completion_command_path_literal, generate_bash_dynamic_completions,
    generate_fish_dynamic_completions, generate_zsh_dynamic_completions,
};
use crate::commands::docs::{describe_cli, CliArgDescribe, CliCommandDescribe};
use clap::Parser;
use serde_yaml::Value as YamlValue;
use std::fs;
use std::process::Command as ProcessCommand;
use tempfile::TempDir;
use vulcan_core::expression::functions::parse_date_like_string;

const CLI_TEST_STACK_BYTES: &str = "8388608";

extern "C" fn configure_cli_test_thread_stack() {
    std::env::set_var("RUST_MIN_STACK", CLI_TEST_STACK_BYTES);
}

#[used]
#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ),
    link_section = ".init_array"
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    link_section = "__DATA,__mod_init_func"
)]
#[cfg_attr(target_os = "windows", link_section = ".CRT$XCU")]
static CONFIGURE_CLI_TEST_THREAD_STACK: extern "C" fn() = configure_cli_test_thread_stack;

fn run_git(vault_root: &Path, args: &[&str]) {
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(vault_root)
        .args(args)
        .status()
        .expect("git should launch");
    assert!(status.success(), "git command failed: {args:?}");
}

fn init_git_repo(vault_root: &Path) {
    run_git(vault_root, &["-c", "init.defaultBranch=main", "init"]);
    run_git(vault_root, &["config", "user.name", "Vulcan Test"]);
    run_git(vault_root, &["config", "user.email", "vulcan@example.com"]);
}

fn git_head_summary(vault_root: &Path) -> String {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(vault_root)
        .args(["log", "-1", "--pretty=%s"])
        .output()
        .expect("git log should launch");
    assert!(output.status.success(), "git log should succeed");
    String::from_utf8(output.stdout)
        .expect("git stdout should be utf8")
        .trim()
        .to_string()
}

#[test]
fn formats_eta_compactly_for_progress_reporting() {
    assert_eq!(format_eta(0, 12.0), "0s");
    assert_eq!(format_eta(5, 10.0), "<1s");
    assert_eq!(format_eta(120, 10.0), "12.0s");
    assert_eq!(format_duration(Duration::from_secs(125)), "2m 5s");
}

#[test]
fn query_ast_rendering_is_hidden_by_default() {
    assert!(!should_render_query_ast(OutputFormat::Human, false, false));
    assert!(!should_render_query_ast(
        OutputFormat::Markdown,
        false,
        false
    ));
    assert!(!should_render_query_ast(OutputFormat::Json, false, false));
}

#[test]
fn query_ast_rendering_requires_explicit_diagnostics() {
    assert!(should_render_query_ast(OutputFormat::Human, true, false));
    assert!(should_render_query_ast(OutputFormat::Json, true, false));
    assert!(should_render_query_ast(OutputFormat::Human, false, true));
    assert!(should_render_query_ast(OutputFormat::Markdown, false, true));
    assert!(!should_render_query_ast(OutputFormat::Json, false, true));
}

#[test]
fn parses_defaults_for_doctor_command() {
    let cli = Cli::try_parse_from(["vulcan", "doctor"]).expect("cli should parse");

    assert_eq!(cli.vault, PathBuf::from("."));
    assert_eq!(cli.output, OutputFormat::Human);
    assert_eq!(cli.fields, None);
    assert_eq!(cli.limit, None);
    assert_eq!(cli.offset, 0);
    assert!(!cli.verbose);
    assert_eq!(
        cli.command,
        Command::Doctor {
            fix: false,
            dry_run: false,
            fail_on_issues: false,
        }
    );
}

#[test]
fn parses_dataview_inline_command() {
    let cli = Cli::try_parse_from(["vulcan", "dataview", "inline", "Dashboard"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Dataview {
            command: DataviewCommand::Inline {
                file: "Dashboard".to_string(),
            },
        }
    );
}

#[test]
fn parses_dataview_query_command() {
    let cli = Cli::try_parse_from(["vulcan", "dataview", "query", "TABLE status FROM #tag"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Dataview {
            command: DataviewCommand::Query {
                dql: "TABLE status FROM #tag".to_string(),
            },
        }
    );
}

#[test]
fn parses_dataview_query_js_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "dataview",
        "query-js",
        "dv.current()",
        "--file",
        "Dashboard",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Dataview {
            command: DataviewCommand::QueryJs {
                js: "dv.current()".to_string(),
                file: Some("Dashboard".to_string()),
            },
        }
    );
}

#[test]
fn parses_dataview_eval_command() {
    let cli = Cli::try_parse_from(["vulcan", "dataview", "eval", "Dashboard", "--block", "1"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Dataview {
            command: DataviewCommand::Eval {
                file: "Dashboard".to_string(),
                block: Some(1),
            },
        }
    );
}

#[test]
fn parses_tasks_query_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "tasks", "query", "not done"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Query {
                query: "not done".to_string(),
            },
        }
    );
}

#[test]
fn parses_tasks_add_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tasks",
        "add",
        "Buy groceries tomorrow @home",
        "--status",
        "open",
        "--priority",
        "high",
        "--due",
        "2026-04-10",
        "--scheduled",
        "2026-04-09",
        "--context",
        "@errands",
        "--project",
        "Website",
        "--tag",
        "shopping",
        "--template",
        "task",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Add {
                text: "Buy groceries tomorrow @home".to_string(),
                no_nlp: false,
                status: Some("open".to_string()),
                priority: Some("high".to_string()),
                due: Some("2026-04-10".to_string()),
                scheduled: Some("2026-04-09".to_string()),
                contexts: vec!["@errands".to_string()],
                projects: vec!["Website".to_string()],
                tags: vec!["shopping".to_string()],
                template: Some("task".to_string()),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_tasks_show_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "tasks", "show", "Write Docs"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Show {
                task: "Write Docs".to_string(),
            },
        }
    );
}

#[test]
fn parses_tasks_edit_command() {
    let cli = Cli::try_parse_from(["vulcan", "tasks", "edit", "Write Docs", "--no-commit"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Edit {
                task: "Write Docs".to_string(),
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_tasks_set_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tasks",
        "set",
        "Write Docs",
        "due",
        "2026-04-12",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Set {
                task: "Write Docs".to_string(),
                property: "due".to_string(),
                value: "2026-04-12".to_string(),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_tasks_complete_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tasks",
        "complete",
        "Write Docs",
        "--date",
        "2026-04-10",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Complete {
                task: "Write Docs".to_string(),
                date: Some("2026-04-10".to_string()),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_tasks_archive_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tasks",
        "archive",
        "Prep Outline",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Archive {
                task: "Prep Outline".to_string(),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_tasks_convert_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tasks",
        "convert",
        "Notes/Idea.md",
        "--line",
        "12",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Convert {
                file: "Notes/Idea.md".to_string(),
                line: Some(12),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_tasks_create_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tasks",
        "create",
        "Call Alice tomorrow @desk",
        "--in",
        "Inbox",
        "--due",
        "2026-04-12",
        "--priority",
        "high",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Create {
                text: "Call Alice tomorrow @desk".to_string(),
                note: Some("Inbox".to_string()),
                due: Some("2026-04-12".to_string()),
                priority: Some("high".to_string()),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_tasks_reschedule_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tasks",
        "reschedule",
        "Inbox:3",
        "--due",
        "2026-04-12",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Reschedule {
                task: "Inbox:3".to_string(),
                due: "2026-04-12".to_string(),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_tasks_eval_command() {
    let cli = Cli::try_parse_from(["vulcan", "tasks", "eval", "Dashboard", "--block", "1"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Eval {
                file: "Dashboard".to_string(),
                block: Some(1),
            },
        }
    );
}

#[test]
fn parses_tasks_list_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tasks",
        "list",
        "--filter",
        "completed",
        "--source",
        "file",
        "--status",
        "in-progress",
        "--priority",
        "high",
        "--due-before",
        "2026-04-11",
        "--due-after",
        "2026-04-01",
        "--project",
        "[[Projects/Website]]",
        "--context",
        "@desk",
        "--group-by",
        "source",
        "--sort-by",
        "due",
        "--include-archived",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::List {
                filter: Some("completed".to_string()),
                source: Some(TasksListSourceArg::Tasknotes),
                status: Some("in-progress".to_string()),
                priority: Some("high".to_string()),
                due_before: Some("2026-04-11".to_string()),
                due_after: Some("2026-04-01".to_string()),
                project: Some("[[Projects/Website]]".to_string()),
                context: Some("@desk".to_string()),
                group_by: Some("source".to_string()),
                sort_by: Some("due".to_string()),
                include_archived: true,
            },
        }
    );
}

#[test]
fn parses_tasks_next_command() {
    let cli = Cli::try_parse_from(["vulcan", "tasks", "next", "5", "--from", "2026-03-29"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Next {
                count: 5,
                from: Some("2026-03-29".to_string()),
            },
        }
    );
}

#[test]
fn parses_tasks_blocked_command() {
    let cli = Cli::try_parse_from(["vulcan", "tasks", "blocked"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Blocked,
        }
    );
}

#[test]
fn parses_tasks_graph_command() {
    let cli = Cli::try_parse_from(["vulcan", "tasks", "graph"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Graph,
        }
    );
}

#[test]
fn parses_tasks_track_start_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tasks",
        "track",
        "start",
        "Write Docs",
        "--description",
        "Deep work",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Track {
                command: TasksTrackCommand::Start {
                    task: "Write Docs".to_string(),
                    description: Some("Deep work".to_string()),
                    dry_run: true,
                    no_commit: true,
                },
            },
        }
    );
}

#[test]
fn parses_tasks_track_summary_command() {
    let cli = Cli::try_parse_from(["vulcan", "tasks", "track", "summary", "--period", "month"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Track {
                command: TasksTrackCommand::Summary {
                    period: TasksTrackSummaryPeriodArg::Month,
                },
            },
        }
    );
}

#[test]
fn parses_tasks_pomodoro_start_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tasks",
        "pomodoro",
        "start",
        "Write Docs",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Pomodoro {
                command: TasksPomodoroCommand::Start {
                    task: "Write Docs".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            },
        }
    );
}

#[test]
fn parses_tasks_pomodoro_status_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "tasks", "pomodoro", "status"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Pomodoro {
                command: TasksPomodoroCommand::Status,
            },
        }
    );
}

#[test]
fn parses_tasks_due_command() {
    let cli = Cli::try_parse_from(["vulcan", "tasks", "due", "--within", "30d"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Due {
                within: "30d".to_string(),
            },
        }
    );
}

#[test]
fn parses_tasks_reminders_command() {
    let cli = Cli::try_parse_from(["vulcan", "tasks", "reminders", "--upcoming", "12h"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::Reminders {
                upcoming: "12h".to_string(),
            },
        }
    );
}

#[test]
fn parses_tasks_view_show_command() {
    let cli = Cli::try_parse_from(["vulcan", "tasks", "view", "show", "Tasks"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::View {
                command: TasksViewCommand::Show {
                    name: "Tasks".to_string(),
                    export: ExportArgs::default(),
                },
            },
        }
    );
}

#[test]
fn parses_tasks_view_list_command() {
    let cli = Cli::try_parse_from(["vulcan", "tasks", "view", "list"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::View {
                command: TasksViewCommand::List,
            },
        }
    );
}

#[test]
fn parses_kanban_list_command() {
    let cli = Cli::try_parse_from(["vulcan", "kanban", "list"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Kanban {
            command: KanbanCommand::List,
        }
    );
}

#[test]
fn parses_kanban_show_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "kanban",
        "show",
        "Board",
        "--verbose",
        "--include-archive",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Kanban {
            command: KanbanCommand::Show {
                board: "Board".to_string(),
                verbose: true,
                include_archive: true,
            },
        }
    );
}

#[test]
fn parses_kanban_cards_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "kanban",
        "cards",
        "Board",
        "--column",
        "Todo",
        "--status",
        "IN_PROGRESS",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Kanban {
            command: KanbanCommand::Cards {
                board: "Board".to_string(),
                column: Some("Todo".to_string()),
                status: Some("IN_PROGRESS".to_string()),
            },
        }
    );
}

#[test]
fn parses_kanban_archive_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "kanban",
        "archive",
        "Board",
        "build-release",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Kanban {
            command: KanbanCommand::Archive {
                board: "Board".to_string(),
                card: "build-release".to_string(),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_kanban_move_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "kanban",
        "move",
        "Board",
        "build-release",
        "Done",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Kanban {
            command: KanbanCommand::Move {
                board: "Board".to_string(),
                card: "build-release".to_string(),
                target_column: "Done".to_string(),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_kanban_add_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "kanban",
        "add",
        "Board",
        "Todo",
        "Build release",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Kanban {
            command: KanbanCommand::Add {
                board: "Board".to_string(),
                column: "Todo".to_string(),
                text: "Build release".to_string(),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_daily_append_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "daily",
        "append",
        "Called Alice",
        "--heading",
        "## Log",
        "--date",
        "2026-04-03",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Daily {
            command: DailyCommand::Append {
                text: "Called Alice".to_string(),
                heading: Some("## Log".to_string()),
                date: Some("2026-04-03".to_string()),
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_today_command() {
    let cli = Cli::try_parse_from(["vulcan", "today", "--no-edit", "--no-commit"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Today {
            no_edit: true,
            no_commit: true,
        }
    );
}

#[test]
fn parses_note_get_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "note",
        "get",
        "Dashboard",
        "--heading",
        "Tasks",
        "--match",
        "TODO",
        "--context",
        "1",
        "--raw",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Note {
            command: NoteCommand::Get {
                note: "Dashboard".to_string(),
                mode: NoteGetMode::Markdown,
                section_id: None,
                heading: Some("Tasks".to_string()),
                block_ref: None,
                lines: None,
                match_pattern: Some("TODO".to_string()),
                context: 1,
                no_frontmatter: false,
                raw: true,
            },
        }
    );
}

#[test]
fn parses_note_get_html_mode_command() {
    let cli = Cli::try_parse_from(["vulcan", "note", "get", "Dashboard", "--mode", "html"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Note {
            command: NoteCommand::Get {
                note: "Dashboard".to_string(),
                mode: NoteGetMode::Html,
                section_id: None,
                heading: None,
                block_ref: None,
                lines: None,
                match_pattern: None,
                context: 0,
                no_frontmatter: false,
                raw: false,
            },
        }
    );
}

#[test]
fn parses_note_outline_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "note", "outline", "Dashboard"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Note {
            command: NoteCommand::Outline {
                note: "Dashboard".to_string(),
                section_id: None,
                depth: None,
            },
        }
    );
}

#[test]
fn parses_note_checkbox_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "note",
        "checkbox",
        "Dashboard",
        "--section",
        "dashboard/tasks@9",
        "--index",
        "2",
        "--state",
        "unchecked",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Note {
            command: NoteCommand::Checkbox {
                note: "Dashboard".to_string(),
                section_id: Some("dashboard/tasks@9".to_string()),
                heading: None,
                block_ref: None,
                lines: None,
                line: None,
                index: Some(2),
                state: NoteCheckboxState::Unchecked,
                check: false,
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_note_append_periodic_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "note",
        "append",
        "- {{VALUE:title|case:slug}}",
        "--periodic",
        "daily",
        "--date",
        "2026-04-03",
        "--prepend",
        "--var",
        "title=Release Planning",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Note {
            command: NoteCommand::Append {
                note_or_text: "- {{VALUE:title|case:slug}}".to_string(),
                text: None,
                heading: None,
                prepend: true,
                append: false,
                periodic: Some(NoteAppendPeriodicArg::Daily),
                date: Some("2026-04-03".to_string()),
                vars: vec!["title=Release Planning".to_string()],
                check: false,
                no_commit: false,
            },
        }
    );
}

#[test]
fn parses_note_patch_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "note",
        "patch",
        "Dashboard",
        "--heading",
        "Tasks",
        "--lines",
        "2-4",
        "--find",
        "/TODO \\d+/",
        "--replace",
        "DONE",
        "--all",
        "--check",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Note {
            command: NoteCommand::Patch {
                note: "Dashboard".to_string(),
                section_id: None,
                heading: Some("Tasks".to_string()),
                block_ref: None,
                lines: Some("2-4".to_string()),
                find: "/TODO \\d+/".to_string(),
                replace: "DONE".to_string(),
                all: true,
                check: true,
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_note_delete_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "note",
        "delete",
        "Dashboard",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Note {
            command: NoteCommand::Delete {
                note: "Dashboard".to_string(),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_note_rename_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "note",
        "rename",
        "Dashboard",
        "Archive/Dashboard",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Note {
            command: NoteCommand::Rename {
                note: "Dashboard".to_string(),
                new_name: "Archive/Dashboard".to_string(),
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_note_info_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "note", "info", "Dashboard"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Note {
            command: NoteCommand::Info {
                note: "Dashboard".to_string(),
            },
        }
    );
}

#[test]
fn parses_note_history_command() {
    let cli = Cli::try_parse_from(["vulcan", "note", "history", "Dashboard", "--limit", "5"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Note {
            command: NoteCommand::History {
                note: "Dashboard".to_string(),
                limit: 5,
            },
        }
    );
}

#[test]
fn parses_daily_export_ics_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "daily",
        "export-ics",
        "--month",
        "--path",
        "journal.ics",
        "--calendar-name",
        "Journal",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Daily {
            command: DailyCommand::ExportIcs {
                from: None,
                to: None,
                week: false,
                month: true,
                path: Some(PathBuf::from("journal.ics")),
                calendar_name: Some("Journal".to_string()),
            },
        }
    );
}

#[test]
fn parses_status_command() {
    let cli = Cli::try_parse_from(["vulcan", "status"]).expect("cli should parse");

    assert_eq!(cli.command, Command::Status);
}

#[test]
fn parses_git_status_command() {
    let cli = Cli::try_parse_from(["vulcan", "git", "status"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Git {
            command: GitCommand::Status,
        }
    );
}

#[test]
fn parses_git_log_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "git", "log", "--limit", "5"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Git {
            command: GitCommand::Log { limit: 5 },
        }
    );
}

#[test]
fn parses_git_diff_command() {
    let cli = Cli::try_parse_from(["vulcan", "git", "diff", "Home.md"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Git {
            command: GitCommand::Diff {
                path: Some("Home.md".to_string()),
            },
        }
    );
}

#[test]
fn parses_git_commit_command() {
    let cli = Cli::try_parse_from(["vulcan", "git", "commit", "-m", "Update notes"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Git {
            command: GitCommand::Commit {
                message: "Update notes".to_string(),
            },
        }
    );
}

#[test]
fn parses_git_blame_command() {
    let cli = Cli::try_parse_from(["vulcan", "git", "blame", "Home.md"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Git {
            command: GitCommand::Blame {
                path: "Home.md".to_string(),
            },
        }
    );
}

#[test]
fn parses_web_search_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "web",
        "search",
        "release notes",
        "--backend",
        "ollama",
        "--limit",
        "5",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Web {
            command: WebCommand::Search {
                query: "release notes".to_string(),
                backend: Some(SearchBackendArg::Ollama),
                limit: 5,
            },
        }
    );
}

#[test]
fn parses_web_fetch_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "web",
        "fetch",
        "https://example.com",
        "--mode",
        "raw",
        "--save",
        "page.bin",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Web {
            command: WebCommand::Fetch {
                url: "https://example.com".to_string(),
                mode: WebFetchMode::Raw,
                save: Some(PathBuf::from("page.bin")),
            },
        }
    );
}

#[test]
fn parses_weekly_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "periodic",
        "weekly",
        "2026-04-03",
        "--no-edit",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Periodic {
            command: Some(PeriodicSubcommand::Weekly {
                args: PeriodicOpenArgs {
                    date: Some("2026-04-03".to_string()),
                    no_edit: true,
                    no_commit: true,
                },
            }),
            period_type: None,
            date: None,
            no_edit: false,
            no_commit: false,
        }
    );
}

#[test]
fn removed_top_level_aliases_no_longer_parse() {
    for command in ["weekly", "monthly", "batch", "cluster", "related", "notes"] {
        assert!(
            Cli::try_parse_from(["vulcan", command]).is_err(),
            "{command} should not parse as a top-level compatibility alias"
        );
    }
    assert!(Cli::try_parse_from(["vulcan", "saved", "search", "weekly", "dashboard"]).is_err());
    assert!(Cli::try_parse_from(["vulcan", "saved", "notes", "active"]).is_err());
    assert!(Cli::try_parse_from(["vulcan", "saved", "bases", "base", "Dash.base"]).is_err());
}

#[test]
fn parses_periodic_gaps_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "periodic",
        "gaps",
        "--type",
        "daily",
        "--from",
        "2026-04-01",
        "--to",
        "2026-04-07",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Periodic {
            command: Some(PeriodicSubcommand::Gaps {
                period_type: Some("daily".to_string()),
                from: Some("2026-04-01".to_string()),
                to: Some("2026-04-07".to_string()),
            }),
            period_type: None,
            date: None,
            no_edit: false,
            no_commit: false,
        }
    );
}

#[test]
fn parses_config_import_tasks_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "config", "import", "tasks"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Import(ConfigImportSelection {
                command: Some(ConfigImportCommand::Tasks),
                all: false,
                list: false,
                args: ConfigImportArgs {
                    dry_run: false,
                    apply: false,
                    target: ConfigTargetArg::Shared,
                    no_commit: false,
                },
            }),
        }
    );
}

#[test]
fn parses_config_show_command() {
    let cli = Cli::try_parse_from(["vulcan", "config", "show", "periodic.daily"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Show {
                section: Some("periodic.daily".to_string()),
            },
        }
    );
}

#[test]
fn parses_config_get_command() {
    let cli = Cli::try_parse_from(["vulcan", "config", "get", "periodic.daily.template"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Get {
                key: "periodic.daily.template".to_string(),
            },
        }
    );
}

#[test]
fn parses_config_edit_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "config", "edit", "--no-commit"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Edit { no_commit: true },
        }
    );
}

#[test]
fn parses_config_set_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "config",
        "set",
        "periodic.daily.template",
        "Templates/Daily",
        "--dry-run",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Set {
                key: "periodic.daily.template".to_string(),
                value: "Templates/Daily".to_string(),
                target: ConfigTargetArg::Shared,
                dry_run: true,
                no_commit: true,
            },
        }
    );
}

#[test]
fn parses_tags_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "tags",
        "--count",
        "--sort",
        "name",
        "--where",
        "status = active",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Tags {
            count: true,
            sort: TagSortArg::Name,
            filters: vec!["status = active".to_string()],
        }
    );
}

#[test]
fn parses_properties_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "properties",
        "--count",
        "--type",
        "--sort",
        "name",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Properties {
            count: true,
            r#type: true,
            sort: PropertySortArg::Name,
        }
    );
}

#[test]
fn parses_config_import_periodic_notes_command() {
    let cli = Cli::try_parse_from(["vulcan", "config", "import", "periodic-notes"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Import(ConfigImportSelection {
                command: Some(ConfigImportCommand::PeriodicNotes),
                all: false,
                list: false,
                args: ConfigImportArgs {
                    dry_run: false,
                    apply: false,
                    target: ConfigTargetArg::Shared,
                    no_commit: false,
                },
            }),
        }
    );
}

#[test]
fn parses_config_import_tasknotes_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "config", "import", "tasknotes"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Import(ConfigImportSelection {
                command: Some(ConfigImportCommand::TaskNotes),
                all: false,
                list: false,
                args: ConfigImportArgs {
                    dry_run: false,
                    apply: false,
                    target: ConfigTargetArg::Shared,
                    no_commit: false,
                },
            }),
        }
    );
}

#[test]
fn parses_config_import_templater_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "config", "import", "templater"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Import(ConfigImportSelection {
                command: Some(ConfigImportCommand::Templater),
                all: false,
                list: false,
                args: ConfigImportArgs {
                    dry_run: false,
                    apply: false,
                    target: ConfigTargetArg::Shared,
                    no_commit: false,
                },
            }),
        }
    );
}

#[test]
fn parses_config_import_quickadd_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "config", "import", "quickadd"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Import(ConfigImportSelection {
                command: Some(ConfigImportCommand::Quickadd),
                all: false,
                list: false,
                args: ConfigImportArgs {
                    dry_run: false,
                    apply: false,
                    target: ConfigTargetArg::Shared,
                    no_commit: false,
                },
            }),
        }
    );
}

#[test]
fn parses_templater_template_preview_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "template",
        "preview",
        "daily",
        "--path",
        "Journal/Today",
        "--engine",
        "templater",
        "--var",
        "project=Vulcan",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Template {
            command: Some(TemplateSubcommand::Preview {
                template: "daily".to_string(),
                path: Some("Journal/Today".to_string()),
                render: TemplateRenderArgs {
                    engine: TemplateEngineArg::Templater,
                    vars: vec!["project=Vulcan".to_string()],
                },
            }),
            name: None,
            list: false,
            path: None,
            render: TemplateRenderArgs {
                engine: TemplateEngineArg::Auto,
                vars: Vec::new(),
            },
            no_commit: false,
        }
    );
}

#[test]
fn parses_config_import_kanban_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "config", "import", "kanban"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Import(ConfigImportSelection {
                command: Some(ConfigImportCommand::Kanban),
                all: false,
                list: false,
                args: ConfigImportArgs {
                    dry_run: false,
                    apply: false,
                    target: ConfigTargetArg::Shared,
                    no_commit: false,
                },
            }),
        }
    );
}

#[test]
fn parses_config_import_core_command_with_shared_flags() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "config",
        "import",
        "core",
        "--preview",
        "--target",
        "local",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Import(ConfigImportSelection {
                command: Some(ConfigImportCommand::Core),
                all: false,
                list: false,
                args: ConfigImportArgs {
                    dry_run: true,
                    apply: false,
                    target: ConfigTargetArg::Local,
                    no_commit: true,
                },
            }),
        }
    );
}

#[test]
fn parses_config_import_apply_command() {
    let cli = Cli::try_parse_from(["vulcan", "config", "import", "tasks", "--apply"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Import(ConfigImportSelection {
                command: Some(ConfigImportCommand::Tasks),
                all: false,
                list: false,
                args: ConfigImportArgs {
                    dry_run: false,
                    apply: true,
                    target: ConfigTargetArg::Shared,
                    no_commit: false,
                },
            }),
        }
    );
}

#[test]
fn parses_config_import_dataview_command() {
    let cli =
        Cli::try_parse_from(["vulcan", "config", "import", "dataview"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Import(ConfigImportSelection {
                command: Some(ConfigImportCommand::Dataview),
                all: false,
                list: false,
                args: ConfigImportArgs {
                    dry_run: false,
                    apply: false,
                    target: ConfigTargetArg::Shared,
                    no_commit: false,
                },
            }),
        }
    );
}

#[test]
fn parses_config_import_all_command() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "config",
        "import",
        "--all",
        "--dry-run",
        "--target",
        "local",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Import(ConfigImportSelection {
                command: None,
                all: true,
                list: false,
                args: ConfigImportArgs {
                    dry_run: true,
                    apply: false,
                    target: ConfigTargetArg::Local,
                    no_commit: false,
                },
            }),
        }
    );
}

#[test]
fn parses_init_import_flags() {
    let cli = Cli::try_parse_from(["vulcan", "init", "--import"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Init(InitArgs {
            import: true,
            no_import: false,
            agent_files: false,
            example_tool: false,
        })
    );
}

#[test]
fn parses_init_agent_files_flag() {
    let cli = Cli::try_parse_from(["vulcan", "init", "--agent-files"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Init(InitArgs {
            import: false,
            no_import: false,
            agent_files: true,
            example_tool: false,
        })
    );
}

#[test]
fn parses_init_agent_files_with_example_tool_flag() {
    let cli = Cli::try_parse_from(["vulcan", "init", "--agent-files", "--example-tool"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Init(InitArgs {
            import: false,
            no_import: false,
            agent_files: true,
            example_tool: true,
        })
    );
}

#[test]
fn parses_agent_install_overwrite_flag() {
    let cli = Cli::try_parse_from(["vulcan", "agent", "install", "--overwrite"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Agent {
            command: AgentCommand::Install(AgentInstallArgs {
                overwrite: true,
                example_tool: false,
            })
        }
    );
}

#[test]
fn parses_agent_install_example_tool_flag() {
    let cli = Cli::try_parse_from(["vulcan", "agent", "install", "--example-tool"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Agent {
            command: AgentCommand::Install(AgentInstallArgs {
                overwrite: false,
                example_tool: true,
            })
        }
    );
}

#[test]
fn parses_agent_print_config_runtime_flag() {
    let cli = Cli::try_parse_from(["vulcan", "agent", "print-config", "--runtime", "codex"])
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Agent {
            command: AgentCommand::PrintConfig(AgentPrintConfigArgs {
                runtime: AgentRuntimeArg::Codex,
            })
        }
    );
}

#[test]
fn parses_agent_import_flags() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "agent",
        "import",
        "--apply",
        "--symlink",
        "--overwrite",
        "--no-commit",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Agent {
            command: AgentCommand::Import(AgentImportArgs {
                apply: true,
                symlink: true,
                overwrite: true,
                no_commit: true,
            })
        }
    );
}

#[test]
fn parses_skill_list_command() {
    let cli = Cli::try_parse_from(["vulcan", "skill", "list"]).expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Skill {
            command: SkillCommand::List,
        }
    );
}

#[test]
fn parses_skill_get_command() {
    let cli = Cli::try_parse_from(["vulcan", "skill", "get", "note-operations"]).expect("parse");

    assert_eq!(
        cli.command,
        Command::Skill {
            command: SkillCommand::Get {
                name: "note-operations".to_string(),
            },
        }
    );
}

#[test]
fn parses_skill_run_and_init_commands() {
    let run = Cli::try_parse_from([
        "vulcan",
        "skill",
        "run",
        "daily-review",
        "prepare-day",
        "--input-json",
        "{\"date\":\"2026-05-08\"}",
    ])
    .expect("skill run should parse");
    let init = Cli::try_parse_from([
        "vulcan",
        "skill",
        "init",
        "daily-review",
        "--starter-command",
        "prepare-day",
        "--dry-run",
    ])
    .expect("skill init should parse");

    assert_eq!(
        run.command,
        Command::Skill {
            command: SkillCommand::Run {
                skill: "daily-review".to_string(),
                command: "prepare-day".to_string(),
                input_json: Some("{\"date\":\"2026-05-08\"}".to_string()),
                input_file: None,
                input_args: Vec::new(),
                input_json_args: Vec::new(),
                input_file_args: Vec::new(),
                input_json_file_args: Vec::new(),
            },
        }
    );
    let exec = Cli::try_parse_from([
        "vulcan",
        "skill",
        "exec",
        ".agents/skills/daily-review/scripts/prepare-day.js",
        "--input-json",
        "{\"date\":\"2026-05-08\"}",
    ])
    .expect("skill exec should parse");
    assert_eq!(
        exec.command,
        Command::Skill {
            command: SkillCommand::Exec {
                script: PathBuf::from(".agents/skills/daily-review/scripts/prepare-day.js"),
                input_json: Some("{\"date\":\"2026-05-08\"}".to_string()),
                input_file: None,
                input_args: Vec::new(),
                input_json_args: Vec::new(),
                input_file_args: Vec::new(),
                input_json_file_args: Vec::new(),
            },
        }
    );
    assert_eq!(
        init.command,
        Command::Skill {
            command: SkillCommand::Init {
                name: "daily-review".to_string(),
                description: None,
                starter_command: Some("prepare-day".to_string()),
                dry_run: true,
                overwrite: false,
            },
        }
    );
}

#[test]
fn config_import_dry_run_does_not_write_target_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let plugin_dir = temp_dir
        .path()
        .join(".obsidian/plugins/obsidian-tasks-plugin");
    fs::create_dir_all(&plugin_dir).expect("tasks plugin dir should be created");
    fs::write(
        plugin_dir.join("data.json"),
        r##"{
              "globalFilter": "#task",
              "globalQuery": "not done",
              "removeGlobalFilter": true,
              "setCreatedDate": false
            }"##,
    )
    .expect("tasks config should be written");

    run_from([
        "vulcan",
        "--vault",
        temp_dir.path().to_str().expect("vault path should be utf8"),
        "config",
        "import",
        "tasks",
        "--dry-run",
    ])
    .expect("config import dry run should succeed");

    assert!(!temp_dir.path().join(".vulcan/config.toml").exists());
}

#[test]
fn config_import_target_local_writes_local_config_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    fs::create_dir_all(temp_dir.path().join(".vulcan")).expect(".vulcan dir should exist");
    let obsidian_dir = temp_dir.path().join(".obsidian");
    fs::create_dir_all(&obsidian_dir).expect("obsidian dir should be created");
    fs::write(
        obsidian_dir.join("app.json"),
        r#"{
              "useMarkdownLinks": true,
              "strictLineBreaks": true
            }"#,
    )
    .expect("core app config should be written");

    run_from([
        "vulcan",
        "--vault",
        temp_dir.path().to_str().expect("vault path should be utf8"),
        "config",
        "import",
        "core",
        "--target",
        "local",
    ])
    .expect("core import should succeed");

    let local_config = fs::read_to_string(temp_dir.path().join(".vulcan/config.local.toml"))
        .expect("local config should exist");
    assert!(local_config.contains("[links]"));
    assert!(local_config.contains("style = \"markdown\""));
    assert!(local_config.contains("strict_line_breaks = true"));
    assert!(!temp_dir.path().join(".vulcan/config.toml").exists());
}

#[test]
fn config_import_dry_run_target_local_does_not_write_local_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let obsidian_dir = temp_dir.path().join(".obsidian");
    fs::create_dir_all(&obsidian_dir).expect("obsidian dir should be created");
    fs::write(
        obsidian_dir.join("templates.json"),
        r#"{
              "dateFormat": "DD/MM/YYYY",
              "timeFormat": "HH:mm"
            }"#,
    )
    .expect("templates config should be written");

    run_from([
        "vulcan",
        "--vault",
        temp_dir.path().to_str().expect("vault path should be utf8"),
        "config",
        "import",
        "core",
        "--dry-run",
        "--target",
        "local",
    ])
    .expect("core dry run should succeed");

    assert!(!temp_dir.path().join(".vulcan/config.local.toml").exists());
}

#[test]
fn edit_new_auto_commit_creates_git_commit() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    init_git_repo(temp_dir.path());
    fs::create_dir_all(temp_dir.path().join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        temp_dir.path().join(".vulcan/config.toml"),
        "[git]\nauto_commit = true\n",
    )
    .expect("config should be written");

    let original_editor = std::env::var_os("EDITOR");
    std::env::set_var("EDITOR", "true");

    let result = run_from([
        "vulcan",
        "--vault",
        temp_dir.path().to_str().expect("temp dir should be utf8"),
        "edit",
        "--new",
        "Notes/Idea.md",
    ]);

    match original_editor {
        Some(value) => std::env::set_var("EDITOR", value),
        None => std::env::remove_var("EDITOR"),
    }

    result.expect("edit should succeed");
    assert!(temp_dir.path().join("Notes/Idea.md").exists());
    assert_eq!(
        git_head_summary(temp_dir.path()),
        "vulcan edit: Notes/Idea.md"
    );
}

fn write_bases_create_fixture(vault_root: &Path, with_template: bool) {
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    if with_template {
        fs::create_dir_all(vault_root.join(".vulcan/templates"))
            .expect("template dir should exist");
        fs::write(
            vault_root.join(".vulcan/templates/Project.md"),
            concat!(
                "---\n",
                "owner: Template Owner\n",
                "tags:\n",
                "  - base\n",
                "---\n",
                "# {{title}}\n\n",
                "Template body.\n",
            ),
        )
        .expect("template should be written");
    }
    fs::write(
        vault_root.join("release.base"),
        if with_template {
            concat!(
                "create_template: Project\n",
                "filters:\n",
                "  - 'file.folder = \"Projects\"'\n",
                "views:\n",
                "  - name: Inbox\n",
                "    type: table\n",
                "    filters:\n",
                "      - 'status = todo'\n",
                "      - 'priority = 2'\n",
            )
        } else {
            concat!(
                "filters:\n",
                "  - 'file.folder = \"Projects\"'\n",
                "views:\n",
                "  - name: Inbox\n",
                "    type: table\n",
                "    filters:\n",
                "      - 'status = todo'\n",
            )
        },
    )
    .expect("base file should be written");
}

#[test]
fn bases_create_dry_run_does_not_write_note() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    write_bases_create_fixture(temp_dir.path(), false);
    let paths = VaultPaths::new(temp_dir.path());

    let report = create_note_from_bases_view(&paths, "release.base", 0, None, true)
        .expect("bases create should succeed");

    assert_eq!(report.path, "Projects/Untitled.md");
    assert!(!temp_dir.path().join("Projects/Untitled.md").exists());
}

#[test]
fn bases_create_writes_template_with_derived_frontmatter() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    write_bases_create_fixture(temp_dir.path(), true);
    let paths = VaultPaths::new(temp_dir.path());

    let report = create_note_from_bases_view(&paths, "release.base", 0, Some("Launch Plan"), false)
        .expect("bases create should succeed");

    assert_eq!(report.path, "Projects/Launch Plan.md");
    let source = fs::read_to_string(temp_dir.path().join(&report.path))
        .expect("created note should be readable");
    let (frontmatter, body) =
        parse_frontmatter_document(&source, false).expect("created note should parse");
    let frontmatter = YamlValue::Mapping(frontmatter.expect("frontmatter should exist"));

    assert_eq!(frontmatter["status"], YamlValue::String("todo".to_string()));
    assert_eq!(frontmatter["priority"], YamlValue::Number(2_i64.into()));
    assert_eq!(
        frontmatter["owner"],
        YamlValue::String("Template Owner".to_string())
    );
    assert_eq!(
        frontmatter["tags"],
        serde_yaml::from_str::<YamlValue>("- base\n").expect("tag yaml should parse")
    );
    assert!(body.contains("# Launch Plan"));
    assert!(body.contains("Template body."));
}

#[test]
fn bases_create_auto_commit_creates_git_commit() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    init_git_repo(temp_dir.path());
    write_bases_create_fixture(temp_dir.path(), false);
    fs::write(
        temp_dir.path().join(".vulcan/config.toml"),
        "[git]\nauto_commit = true\n",
    )
    .expect("config should be written");

    run_from([
        "vulcan",
        "--vault",
        temp_dir.path().to_str().expect("temp dir should be utf8"),
        "bases",
        "create",
        "release.base",
        "--title",
        "Launch Plan",
    ])
    .expect("bases create should succeed");

    assert!(temp_dir.path().join("Projects/Launch Plan.md").exists());
    assert_eq!(
        git_head_summary(temp_dir.path()),
        "vulcan bases-create: Projects/Launch Plan.md"
    );
}

#[test]
fn diff_command_uses_git_head_for_modified_note() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    fs::write(temp_dir.path().join("Home.md"), "# Home\n").expect("note should be written");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(paths.vulcan_dir()).expect(".vulcan dir should exist");
    vulcan_core::scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");
    init_git_repo(temp_dir.path());
    run_git(temp_dir.path(), &["add", "Home.md"]);
    run_git(temp_dir.path(), &["commit", "-m", "Initial"]);

    fs::write(temp_dir.path().join("Home.md"), "# Home\nUpdated\n")
        .expect("note should be updated");

    let report = run_diff_command(&paths, Some("Home"), None, false).expect("diff should succeed");

    assert_eq!(report.path, "Home.md");
    assert_eq!(report.source, "git_head");
    assert_eq!(report.status, "changed");
    assert!(report.changed);
    assert!(report
        .diff
        .as_deref()
        .is_some_and(|diff| diff.contains("+Updated")));
}

#[test]
fn append_under_heading_inserts_before_next_peer_heading() {
    let rendered = append_under_heading(
        "# Notes\n\n## Inbox\n\n- earlier\n\n## Later\n\ncontent\n",
        "## Inbox",
        "- new item",
    );

    assert!(rendered.contains("## Inbox\n\n- earlier\n\n- new item\n\n## Later"));
}

#[test]
fn render_template_contents_supports_obsidian_format_strings() {
    let timestamp = test_template_timestamp(2026, 3, 26, 17, 4, 5);
    let variables = template_variables_for_path("Journal/Today.md", timestamp);
    let config = TemplatesConfig {
        date_format: "dddd, MMMM Do YYYY".to_string(),
        time_format: "hh:mm A".to_string(),
        ..TemplatesConfig::default()
    };

    let rendered = render_template_contents(
            "Date {{date}}\nTime {{time}}\nAlt {{time:YYYY-MM-DD}}\nWeekday {{date:dd}} {{date:ddd}} {{date:dddd}}\nStamp {{datetime}}\n",
            &variables,
            &config,
        );

    assert!(rendered.contains("Date Thursday, March 26th 2026"));
    assert!(rendered.contains("Time 05:04 PM"));
    assert!(rendered.contains("Alt 2026-03-26"));
    assert!(rendered.contains("Weekday Th Thu Thursday"));
    assert!(rendered.contains(&format!("Stamp {}", variables.datetime)));
}

#[test]
fn render_template_contents_preserves_datetime_and_uuid_variables() {
    let timestamp = test_template_timestamp(2026, 3, 26, 17, 4, 5);
    let mut variables = template_variables_for_path("Journal/Today.md", timestamp);
    variables.uuid = "00000000-0000-0000-0000-000000000000".to_string();
    let config = TemplatesConfig::default();

    let rendered = render_template_contents(
        "{{datetime}}\n{{uuid}}\n{{date}}\n{{time}}\n",
        &variables,
        &config,
    );

    assert!(rendered.contains("2026-03-26T17:04:05Z"));
    assert!(rendered.contains("00000000-0000-0000-0000-000000000000"));
    assert!(rendered.contains("2026-03-26"));
    assert!(rendered.contains("17:04"));
}

#[test]
fn template_command_lists_obsidian_templates_with_sources_and_conflicts() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
    fs::create_dir_all(temp_dir.path().join(".obsidian")).expect("obsidian dir");
    fs::create_dir_all(temp_dir.path().join("Shared Templates")).expect("shared templates dir");
    fs::write(
        temp_dir.path().join(".obsidian/templates.json"),
        r#"{"folder":"Shared Templates"}"#,
    )
    .expect("templates config should be written");
    fs::write(
        paths.vulcan_dir().join("templates").join("daily.md"),
        "# Vulcan\n",
    )
    .expect("vulcan template should be written");
    fs::write(
        temp_dir.path().join("Shared Templates").join("daily.md"),
        "# Obsidian\n",
    )
    .expect("obsidian daily template should be written");
    fs::write(
        temp_dir.path().join("Shared Templates").join("meeting.md"),
        "# Meeting\n",
    )
    .expect("obsidian meeting template should be written");

    let result = run_template_command(
        &paths,
        None,
        true,
        None,
        TemplateEngineArg::Auto,
        &[],
        false,
        false,
        false,
    )
    .expect("template list should succeed");
    let TemplateCommandResult::List(report) = result else {
        panic!("template command should list templates");
    };

    assert_eq!(
        report.templates,
        vec![
            TemplateSummary {
                name: "daily.md".to_string(),
                source: "vulcan".to_string(),
                path: ".vulcan/templates/daily.md".to_string(),
            },
            TemplateSummary {
                name: "meeting.md".to_string(),
                source: "obsidian".to_string(),
                path: "Shared Templates/meeting.md".to_string(),
            },
        ]
    );
    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].contains("daily.md"));
    assert!(report.warnings[0].contains(".vulcan/templates/daily.md"));
}

#[test]
fn template_command_lists_templater_templates_with_sources() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
    fs::create_dir_all(temp_dir.path().join(".obsidian/plugins/templater-obsidian"))
        .expect("templater dir");
    fs::create_dir_all(temp_dir.path().join("Templater")).expect("templater templates dir");
    fs::write(
        temp_dir
            .path()
            .join(".obsidian/plugins/templater-obsidian/data.json"),
        r#"{"templates_folder":"Templater"}"#,
    )
    .expect("templater config should be written");
    fs::write(
        temp_dir.path().join("Templater").join("daily.md"),
        "# Templater\n",
    )
    .expect("templater template should be written");

    let result = run_template_command(
        &paths,
        None,
        true,
        None,
        TemplateEngineArg::Auto,
        &[],
        false,
        false,
        false,
    )
    .expect("template list should succeed");
    let TemplateCommandResult::List(report) = result else {
        panic!("template command should list templates");
    };

    assert_eq!(
        report.templates,
        vec![TemplateSummary {
            name: "daily.md".to_string(),
            source: "templater".to_string(),
            path: "Templater/daily.md".to_string(),
        }]
    );
    assert!(report.warnings.is_empty());
}

#[test]
fn template_command_prefers_vulcan_template_over_obsidian_conflict() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
    fs::create_dir_all(temp_dir.path().join(".obsidian")).expect("obsidian dir");
    fs::create_dir_all(temp_dir.path().join("Shared Templates")).expect("shared templates dir");
    fs::write(
        temp_dir.path().join(".obsidian/templates.json"),
        r#"{"folder":"Shared Templates"}"#,
    )
    .expect("templates config should be written");
    fs::write(
        paths.vulcan_dir().join("templates").join("daily.md"),
        "# Vulcan {{title}}\n",
    )
    .expect("vulcan template should be written");
    fs::write(
        temp_dir.path().join("Shared Templates").join("daily.md"),
        "# Obsidian {{title}}\n",
    )
    .expect("obsidian template should be written");

    let result = run_template_command(
        &paths,
        Some("daily"),
        false,
        Some("Journal/Today"),
        TemplateEngineArg::Auto,
        &[],
        false,
        false,
        false,
    )
    .expect("template command should succeed");

    let TemplateCommandResult::Create(report) = result else {
        panic!("template command should create a note");
    };
    assert_eq!(report.template, "daily.md");
    assert_eq!(report.template_source, "vulcan");
    assert_eq!(report.warnings.len(), 1);

    let contents = fs::read_to_string(temp_dir.path().join("Journal/Today.md"))
        .expect("created note should be readable");
    assert!(contents.contains("# Vulcan Today"));
    assert!(!contents.contains("# Obsidian Today"));
}

#[test]
fn prepare_template_insertion_merges_frontmatter_without_overwriting_existing_values() {
    let timestamp = test_template_timestamp(2026, 3, 26, 17, 4, 5);
    let variables = template_variables_for_path("Projects/Alpha.md", timestamp);
    let rendered_template = render_template_contents(
            "---\nstatus: backlog\ncreated: \"{{date}}\"\ntags:\n- team\n- release\n---\n\n## Template Section\n",
            &variables,
            &TemplatesConfig::default(),
        );

    let prepared = prepare_template_insertion(
        "---\nstatus: done\ntags:\n- team\n- shipped\nowner: Alice\n---\n# Existing\n",
        &rendered_template,
    )
    .expect("template insertion should be prepared");

    let merged_frontmatter = prepared
        .merged_frontmatter
        .expect("merged frontmatter should be present");
    let merged = YamlValue::Mapping(merged_frontmatter);
    assert_eq!(merged["status"], YamlValue::String("done".to_string()));
    assert_eq!(merged["owner"], YamlValue::String("Alice".to_string()));
    assert_eq!(
        merged["created"],
        YamlValue::String("2026-03-26".to_string())
    );
    assert_eq!(
        merged["tags"],
        serde_yaml::from_str::<YamlValue>("- team\n- shipped\n- release\n")
            .expect("tags should parse")
    );
    assert_eq!(prepared.target_body, "# Existing\n");
    assert_eq!(prepared.template_body, "\n## Template Section\n");
}

#[test]
fn prepare_template_insertion_uses_template_frontmatter_when_target_has_none() {
    let prepared =
        prepare_template_insertion("# Existing\n", "---\nstatus: backlog\n---\nTemplate body\n")
            .expect("template insertion should be prepared");

    let rendered =
        render_note_from_parts(prepared.merged_frontmatter.as_ref(), &prepared.target_body)
            .expect("note should render");
    assert!(rendered.starts_with("---\nstatus: backlog\n---\n# Existing\n"));
    assert_eq!(prepared.template_body, "Template body\n");
}

#[test]
fn template_command_creates_note_and_renders_variables() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
    fs::write(
        paths.vulcan_dir().join("templates").join("daily.md"),
        "# {{title}}\n\nCreated {{date}} {{time}}\nID {{uuid}}\n",
    )
    .expect("template should be written");

    let result = run_template_command(
        &paths,
        Some("daily"),
        false,
        Some("Journal/Today"),
        TemplateEngineArg::Auto,
        &[],
        false,
        false,
        false,
    )
    .expect("template command should succeed");

    let TemplateCommandResult::Create(report) = result else {
        panic!("template command should create a note");
    };
    assert_eq!(report.path, "Journal/Today.md");
    let contents = fs::read_to_string(temp_dir.path().join("Journal/Today.md"))
        .expect("created note should be readable");
    assert!(contents.contains("# Today"));
    assert!(contents.contains("Created "));
    assert!(contents.contains("ID "));
}

#[test]
fn template_insert_command_prepends_and_merges_frontmatter() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
    fs::write(
        temp_dir.path().join("Home.md"),
        "---\nstatus: done\ntags:\n- team\n- shipped\nowner: Alice\n---\n# Existing\n",
    )
    .expect("target note should be written");
    fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "---\nstatus: backlog\ncreated: \"{{date}}\"\ntags:\n- team\n- release\n---\n\n## Inserted\n",
        )
        .expect("template should be written");
    vulcan_core::scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");

    let report = run_template_insert_command(
        &paths,
        "daily",
        Some("Home"),
        TemplateInsertMode::Prepend,
        TemplateEngineArg::Auto,
        &[],
        false,
        false,
        false,
    )
    .expect("template insert should succeed");

    assert_eq!(report.note, "Home.md");
    assert_eq!(report.mode, "prepend");
    let updated =
        fs::read_to_string(temp_dir.path().join("Home.md")).expect("updated note should exist");
    let (frontmatter, body) =
        parse_frontmatter_document(&updated, false).expect("updated note should parse");
    let frontmatter = YamlValue::Mapping(frontmatter.expect("frontmatter should exist"));
    assert_eq!(frontmatter["status"], YamlValue::String("done".to_string()));
    assert_eq!(frontmatter["owner"], YamlValue::String("Alice".to_string()));
    assert_eq!(
        frontmatter["created"],
        YamlValue::String(TemplateTimestamp::current().default_date_string())
    );
    assert_eq!(
        frontmatter["tags"],
        serde_yaml::from_str::<YamlValue>("- team\n- shipped\n- release\n")
            .expect("tags should parse"),
    );
    assert_eq!(body, "\n## Inserted\n\n# Existing\n");
}

#[test]
fn template_insert_command_appends_and_auto_commits() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
    fs::write(temp_dir.path().join("Home.md"), "# Existing\n")
        .expect("target note should be written");
    fs::write(
        paths.vulcan_dir().join("templates").join("daily.md"),
        "## Inserted\n",
    )
    .expect("template should be written");
    fs::write(paths.config_file(), "[git]\nauto_commit = true\n")
        .expect("config should be written");
    vulcan_core::scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");
    init_git_repo(temp_dir.path());
    run_git(temp_dir.path(), &["add", "Home.md", ".vulcan/config.toml"]);
    run_git(temp_dir.path(), &["commit", "-m", "Initial"]);

    let report = run_template_insert_command(
        &paths,
        "daily",
        Some("Home"),
        TemplateInsertMode::Append,
        TemplateEngineArg::Auto,
        &[],
        false,
        false,
        false,
    )
    .expect("template insert should succeed");

    assert_eq!(report.note, "Home.md");
    assert_eq!(report.mode, "append");
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Home.md")).expect("updated note should exist"),
        "# Existing\n\n## Inserted\n",
    );
    assert_eq!(
        git_head_summary(temp_dir.path()),
        "vulcan template insert: Home.md"
    );
}

fn test_template_timestamp(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
) -> TemplateTimestamp {
    let timestamp = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z");
    TemplateTimestamp::from_millis(
        parse_date_like_string(&timestamp).expect("fixed template timestamp should parse"),
    )
}

#[test]
#[allow(clippy::too_many_lines)]
fn parses_links_and_backlinks_commands() {
    let rebuild =
        Cli::try_parse_from(["vulcan", "rebuild", "--dry-run"]).expect("cli should parse");
    let repair =
        Cli::try_parse_from(["vulcan", "repair", "fts", "--dry-run"]).expect("cli should parse");
    let watch =
        Cli::try_parse_from(["vulcan", "watch", "--debounce-ms", "125"]).expect("cli should parse");
    let serve = Cli::try_parse_from([
        "vulcan",
        "serve",
        "--bind",
        "127.0.0.1:4000",
        "--no-watch",
        "--debounce-ms",
        "100",
        "--auth-token",
        "secret",
    ])
    .expect("cli should parse");
    let doctor =
        Cli::try_parse_from(["vulcan", "doctor", "--fix", "--dry-run"]).expect("cli should parse");
    let doctor_fail =
        Cli::try_parse_from(["vulcan", "doctor", "--fail-on-issues"]).expect("cli should parse");
    let graph_path =
        Cli::try_parse_from(["vulcan", "graph", "path", "Home", "Bob"]).expect("cli should parse");
    let graph_moc = Cli::try_parse_from(["vulcan", "graph", "moc"]).expect("cli should parse");
    let graph_trends = Cli::try_parse_from(["vulcan", "graph", "trends", "--limit", "7"])
        .expect("cli should parse");
    let cache_verify = Cli::try_parse_from(["vulcan", "cache", "verify", "--fail-on-errors"])
        .expect("cli should parse");
    let cache_vacuum =
        Cli::try_parse_from(["vulcan", "cache", "vacuum", "--dry-run"]).expect("cli should parse");
    let export_search_index = Cli::try_parse_from(["vulcan", "export", "search-index", "--pretty"])
        .expect("cli should parse");
    let export_profiles =
        Cli::try_parse_from(["vulcan", "export", "profile", "list"]).expect("cli should parse");
    let export_profile_run =
        Cli::try_parse_from(["vulcan", "export", "profile", "run", "team-book"])
            .expect("cli should parse");
    let export_profile_serve = Cli::try_parse_from([
        "vulcan",
        "export",
        "profile",
        "serve",
        "public-bundle",
        "--port",
        "4174",
    ])
    .expect("cli should parse");
    let export_profile_show =
        Cli::try_parse_from(["vulcan", "export", "profile", "show", "team-book"])
            .expect("cli should parse");
    let export_profile_create = Cli::try_parse_from([
        "vulcan",
        "export",
        "profile",
        "create",
        "team-book",
        "--format",
        "epub",
        "from notes",
        "-o",
        "exports/team.epub",
        "--title",
        "Team Notes",
        "--backlinks",
    ])
    .expect("cli should parse");
    let export_profile_set = Cli::try_parse_from([
        "vulcan",
        "export",
        "profile",
        "set",
        "team-book",
        "--backlinks",
        "--frontmatter",
    ])
    .expect("cli should parse");
    let export_profile_rule_add = Cli::try_parse_from([
        "vulcan",
        "export",
        "profile",
        "rule",
        "add",
        "team-book",
        "from notes where file.path starts_with \"People/\"",
        "--exclude-callout",
        "secret gm",
    ])
    .expect("cli should parse");
    let export_profile_rule_move = Cli::try_parse_from([
        "vulcan",
        "export",
        "profile",
        "rule",
        "move",
        "team-book",
        "2",
        "--before",
        "1",
    ])
    .expect("cli should parse");
    let export_profile_delete = Cli::try_parse_from([
        "vulcan",
        "export",
        "profile",
        "delete",
        "team-book",
        "--dry-run",
    ])
    .expect("cli should parse");
    let export_epub = Cli::try_parse_from([
        "vulcan",
        "export",
        "epub",
        "from notes",
        "-o",
        "exports/book.epub",
        "--title",
        "Team Notes",
        "--author",
        "Vulcan",
        "--backlinks",
        "--exclude-callout",
        "secret gm",
    ])
    .expect("cli should parse");
    let links = Cli::try_parse_from(["vulcan", "links", "Home"]).expect("cli should parse");
    let links_picker = Cli::try_parse_from(["vulcan", "links"]).expect("cli should parse");
    let backlinks =
        Cli::try_parse_from(["vulcan", "backlinks", "Projects/Alpha"]).expect("cli should parse");
    let related_picker =
        Cli::try_parse_from(["vulcan", "vectors", "related"]).expect("cli should parse");
    let search = Cli::try_parse_from([
        "vulcan",
        "search",
        "dashboard",
        "--where",
        "reviewed = true",
        "--tag",
        "index",
        "--path-prefix",
        "People/",
        "--has-property",
        "status",
        "--context-size",
        "24",
        "--fuzzy",
        "--explain",
    ])
    .expect("cli should parse");
    let notes = Cli::try_parse_from([
        "vulcan",
        "query",
        "--where",
        "status = done",
        "--where",
        "estimate > 2",
        "--sort",
        "due",
        "--desc",
    ])
    .expect("cli should parse");
    let bases =
        Cli::try_parse_from(["vulcan", "bases", "eval", "release.base"]).expect("cli should parse");
    let bases_create = Cli::try_parse_from([
        "vulcan",
        "bases",
        "create",
        "release.base",
        "--title",
        "Launch Plan",
        "--dry-run",
    ])
    .expect("cli should parse");
    let bases_tui =
        Cli::try_parse_from(["vulcan", "bases", "tui", "release.base"]).expect("cli should parse");
    let suggest_mentions =
        Cli::try_parse_from(["vulcan", "suggest", "mentions", "Home"]).expect("cli should parse");
    let suggest_duplicates =
        Cli::try_parse_from(["vulcan", "suggest", "duplicates"]).expect("cli should parse");
    let diff = Cli::try_parse_from(["vulcan", "note", "diff", "Home"]).expect("cli should parse");
    let inbox = Cli::try_parse_from(["vulcan", "inbox", "idea"]).expect("cli should parse");
    let template = Cli::try_parse_from(["vulcan", "template", "daily", "--path", "Notes/Day"])
        .expect("cli should parse");
    let template_insert =
        Cli::try_parse_from(["vulcan", "template", "insert", "daily", "Home", "--prepend"])
            .expect("cli should parse");
    let open = Cli::try_parse_from(["vulcan", "open", "Home"]).expect("cli should parse");
    let link_mentions = Cli::try_parse_from(["vulcan", "link-mentions", "Home", "--dry-run"])
        .expect("cli should parse");
    let rewrite = Cli::try_parse_from([
        "vulcan",
        "rewrite",
        "--where",
        "reviewed = true",
        "--find",
        "release",
        "--replace",
        "launch",
        "--dry-run",
    ])
    .expect("cli should parse");
    let vectors =
        Cli::try_parse_from(["vulcan", "vectors", "index", "--dry-run"]).expect("cli should parse");
    let vector_repair = Cli::try_parse_from(["vulcan", "vectors", "repair", "--dry-run"])
        .expect("cli should parse");
    let vector_rebuild = Cli::try_parse_from(["vulcan", "vectors", "rebuild", "--dry-run"])
        .expect("cli should parse");
    let vector_queue =
        Cli::try_parse_from(["vulcan", "vectors", "queue", "status"]).expect("cli should parse");
    let vector_related =
        Cli::try_parse_from(["vulcan", "vectors", "related", "Home"]).expect("cli should parse");
    let duplicates =
        Cli::try_parse_from(["vulcan", "vectors", "duplicates"]).expect("cli should parse");
    let cluster = Cli::try_parse_from([
        "vulcan",
        "vectors",
        "cluster",
        "--clusters",
        "3",
        "--dry-run",
    ])
    .expect("cli should parse");
    let related =
        Cli::try_parse_from(["vulcan", "vectors", "related", "Home"]).expect("cli should parse");
    let browse = Cli::try_parse_from(["vulcan", "browse"]).expect("cli should parse");
    let refreshed_browse = Cli::try_parse_from(["vulcan", "--refresh", "background", "browse"])
        .expect("cli should parse");
    let edit = Cli::try_parse_from(["vulcan", "edit", "Home"]).expect("cli should parse");
    let edit_new =
        Cli::try_parse_from(["vulcan", "edit", "--new", "Notes/Idea"]).expect("cli should parse");
    let move_command = Cli::try_parse_from([
        "vulcan",
        "move",
        "Projects/Alpha.md",
        "Archive/Alpha.md",
        "--dry-run",
    ])
    .expect("cli should parse");
    let completions =
        Cli::try_parse_from(["vulcan", "completions", "bash"]).expect("cli should parse");
    let saved_search = Cli::try_parse_from([
        "vulcan",
        "--fields",
        "document_path,rank",
        "--limit",
        "5",
        "saved",
        "create",
        "search",
        "weekly",
        "dashboard",
        "--where",
        "reviewed = true",
        "--raw-query",
        "--fuzzy",
        "--description",
        "weekly dashboard",
        "--export",
        "csv",
        "--export-path",
        "exports/weekly.csv",
    ])
    .expect("cli should parse");
    let saved_run = Cli::try_parse_from([
        "vulcan",
        "saved",
        "run",
        "weekly",
        "--export",
        "jsonl",
        "--export-path",
        "exports/weekly.jsonl",
    ])
    .expect("cli should parse");
    let checkpoint_create = Cli::try_parse_from(["vulcan", "checkpoint", "create", "weekly"])
        .expect("cli should parse");
    let checkpoint_list =
        Cli::try_parse_from(["vulcan", "checkpoint", "list"]).expect("cli should parse");
    let changes = Cli::try_parse_from(["vulcan", "changes", "--checkpoint", "weekly"])
        .expect("cli should parse");
    let today = Cli::try_parse_from(["vulcan", "today", "--no-edit"]).expect("cli should parse");
    let automation_list =
        Cli::try_parse_from(["vulcan", "automation", "list"]).expect("cli should parse");
    let automation = Cli::try_parse_from([
        "vulcan",
        "automation",
        "run",
        "--scan",
        "--doctor",
        "--verify-cache",
        "--repair-fts",
        "--all-reports",
        "--fail-on-issues",
    ])
    .expect("cli should parse");

    assert_eq!(rebuild.command, Command::Rebuild { dry_run: true });
    assert_eq!(
        repair.command,
        Command::Repair {
            command: RepairCommand::Fts { dry_run: true }
        }
    );
    assert_eq!(
        watch.command,
        Command::Watch {
            debounce_ms: 125,
            no_commit: false,
        }
    );
    assert_eq!(
        serve.command,
        Command::Serve {
            bind: "127.0.0.1:4000".to_string(),
            no_watch: true,
            debounce_ms: 100,
            auth_token: Some("secret".to_string()),
        }
    );
    assert_eq!(
        doctor.command,
        Command::Doctor {
            fix: true,
            dry_run: true,
            fail_on_issues: false,
        }
    );
    assert_eq!(
        doctor_fail.command,
        Command::Doctor {
            fix: false,
            dry_run: false,
            fail_on_issues: true,
        }
    );
    assert_eq!(
        graph_path.command,
        Command::Graph {
            command: GraphCommand::Path {
                from: Some("Home".to_string()),
                to: Some("Bob".to_string()),
            }
        }
    );
    assert_eq!(
        graph_moc.command,
        Command::Graph {
            command: GraphCommand::Moc {
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        graph_trends.command,
        Command::Graph {
            command: GraphCommand::Trends {
                limit: 7,
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        cache_verify.command,
        Command::Cache {
            command: CacheCommand::Verify {
                fail_on_errors: true,
            }
        }
    );
    assert_eq!(
        cache_vacuum.command,
        Command::Cache {
            command: CacheCommand::Vacuum { dry_run: true }
        }
    );
    assert_eq!(
        export_search_index.command,
        Command::Export {
            command: ExportCommand::SearchIndex {
                path: None,
                pretty: true,
            },
        }
    );
    assert_eq!(
        export_profiles.command,
        Command::Export {
            command: ExportCommand::Profile {
                command: ExportProfileCommand::List,
            },
        }
    );
    assert_eq!(
        export_profile_run.command,
        Command::Export {
            command: ExportCommand::Profile {
                command: ExportProfileCommand::Run {
                    name: "team-book".to_string(),
                },
            },
        }
    );
    assert_eq!(
        export_profile_serve.command,
        Command::Export {
            command: ExportCommand::Profile {
                command: ExportProfileCommand::Serve {
                    name: "public-bundle".to_string(),
                    port: 4174,
                    debounce_ms: 100,
                },
            },
        }
    );
    assert_eq!(
        export_profile_show.command,
        Command::Export {
            command: ExportCommand::Profile {
                command: ExportProfileCommand::Show {
                    name: "team-book".to_string(),
                },
            },
        }
    );
    assert_eq!(
        export_profile_create.command,
        Command::Export {
            command: ExportCommand::Profile {
                command: ExportProfileCommand::Create {
                    name: "team-book".to_string(),
                    format: ExportProfileFormatArg::Epub,
                    query: Some("from notes".to_string()),
                    query_json: None,
                    path: PathBuf::from("exports/team.epub"),
                    site_profile: None,
                    title: Some("Team Notes".to_string()),
                    author: None,
                    toc: None,
                    backlinks: true,
                    frontmatter: false,
                    pretty: false,
                    graph_format: None,
                    replace: false,
                    dry_run: false,
                    no_commit: false,
                },
            },
        }
    );
    assert_eq!(
        export_profile_set.command,
        Command::Export {
            command: ExportCommand::Profile {
                command: ExportProfileCommand::Set {
                    name: "team-book".to_string(),
                    format: None,
                    query: None,
                    query_json: None,
                    clear_query: false,
                    path: None,
                    clear_path: false,
                    site_profile: None,
                    clear_site_profile: false,
                    title: None,
                    clear_title: false,
                    author: None,
                    clear_author: false,
                    toc: None,
                    clear_toc: false,
                    backlinks: true,
                    no_backlinks: false,
                    frontmatter: true,
                    no_frontmatter: false,
                    pretty: false,
                    no_pretty: false,
                    graph_format: None,
                    clear_graph_format: false,
                    dry_run: false,
                    no_commit: false,
                },
            },
        }
    );
    assert_eq!(
        export_profile_rule_add.command,
        Command::Export {
            command: ExportCommand::Profile {
                command: ExportProfileCommand::Rule {
                    command: ExportProfileRuleCommand::Add {
                        profile: "team-book".to_string(),
                        before: None,
                        query: Some(
                            "from notes where file.path starts_with \"People/\"".to_string()
                        ),
                        query_json: None,
                        transforms: Box::new(ExportTransformArgs {
                            exclude_callouts: vec!["secret gm".to_string()],
                            exclude_headings: vec![],
                            exclude_frontmatter_keys: vec![],
                            exclude_inline_fields: vec![],
                            replace_rules: vec![],
                        }),
                        dry_run: false,
                        no_commit: false,
                    },
                },
            },
        }
    );
    assert_eq!(
        export_profile_rule_move.command,
        Command::Export {
            command: ExportCommand::Profile {
                command: ExportProfileCommand::Rule {
                    command: ExportProfileRuleCommand::Move {
                        profile: "team-book".to_string(),
                        index: 2,
                        before: Some(1),
                        after: None,
                        last: false,
                        dry_run: false,
                        no_commit: false,
                    },
                },
            },
        }
    );
    assert_eq!(
        export_profile_delete.command,
        Command::Export {
            command: ExportCommand::Profile {
                command: ExportProfileCommand::Delete {
                    name: "team-book".to_string(),
                    dry_run: true,
                    no_commit: false,
                },
            },
        }
    );
    assert_eq!(
        export_epub.command,
        Command::Export {
            command: ExportCommand::Epub {
                query: ExportQueryArgs {
                    query: Some("from notes".to_string()),
                    query_json: None,
                },
                path: PathBuf::from("exports/book.epub"),
                title: Some("Team Notes".to_string()),
                author: Some("Vulcan".to_string()),
                toc: EpubTocStyle::Tree,
                backlinks: true,
                frontmatter: false,
                transforms: ExportTransformArgs {
                    exclude_callouts: vec!["secret gm".to_string()],
                    exclude_headings: vec![],
                    exclude_frontmatter_keys: vec![],
                    exclude_inline_fields: vec![],
                    replace_rules: vec![],
                },
            },
        }
    );

    assert_eq!(
        links.command,
        Command::Links {
            note: Some("Home".to_string()),
            export: ExportArgs::default(),
        }
    );
    assert_eq!(
        links_picker.command,
        Command::Links {
            note: None,
            export: ExportArgs::default(),
        }
    );
    assert_eq!(
        backlinks.command,
        Command::Backlinks {
            note: Some("Projects/Alpha".to_string()),
            export: ExportArgs::default(),
        }
    );
    assert_eq!(
        search.command,
        Command::Search {
            query: Some("dashboard".to_string()),
            regex: None,
            filters: vec!["reviewed = true".to_string()],
            mode: SearchMode::Keyword,
            tag: Some("index".to_string()),
            path_prefix: Some("People/".to_string()),
            has_property: Some("status".to_string()),
            sort: None,
            match_case: false,
            context_size: 24,
            raw_query: false,
            fuzzy: true,
            explain: true,
            exit_code: false,
            export: ExportArgs::default(),
        }
    );
    assert_eq!(
        notes.command,
        Command::Query {
            dsl: None,
            json: None,
            filters: vec!["status = done".to_string(), "estimate > 2".to_string()],
            sort: Some("due".to_string()),
            desc: true,
            list_fields: false,
            engine: QueryEngineArg::Auto,
            format: QueryFormatArg::Table,
            glob: None,
            explain: false,
            exit_code: false,
            export: ExportArgs::default(),
        }
    );
    assert_eq!(
        bases.command,
        Command::Bases {
            command: BasesCommand::Eval {
                file: "release.base".to_string(),
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        bases_create.command,
        Command::Bases {
            command: BasesCommand::Create {
                file: "release.base".to_string(),
                title: Some("Launch Plan".to_string()),
                dry_run: true,
                no_commit: false,
            },
        }
    );
    assert_eq!(
        bases_tui.command,
        Command::Bases {
            command: BasesCommand::Tui {
                file: "release.base".to_string(),
            },
        }
    );
    assert_eq!(
        suggest_mentions.command,
        Command::Suggest {
            command: SuggestCommand::Mentions {
                note: Some("Home".to_string()),
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        suggest_duplicates.command,
        Command::Suggest {
            command: SuggestCommand::Duplicates {
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        diff.command,
        Command::Note {
            command: NoteCommand::Diff {
                note: "Home".to_string(),
                since: None,
            },
        }
    );
    assert_eq!(
        inbox.command,
        Command::Inbox {
            text: Some("idea".to_string()),
            file: None,
            no_commit: false,
        }
    );
    assert_eq!(
        template.command,
        Command::Template {
            command: None,
            name: Some("daily".to_string()),
            list: false,
            path: Some("Notes/Day".to_string()),
            render: TemplateRenderArgs {
                engine: TemplateEngineArg::Auto,
                vars: Vec::new(),
            },
            no_commit: false,
        }
    );
    assert_eq!(
        template_insert.command,
        Command::Template {
            command: Some(TemplateSubcommand::Insert {
                template: "daily".to_string(),
                note: Some("Home".to_string()),
                prepend: true,
                append: false,
                render: TemplateRenderArgs {
                    engine: TemplateEngineArg::Auto,
                    vars: Vec::new(),
                },
                no_commit: false,
            }),
            name: None,
            list: false,
            path: None,
            render: TemplateRenderArgs {
                engine: TemplateEngineArg::Auto,
                vars: Vec::new(),
            },
            no_commit: false,
        }
    );
    assert_eq!(
        open.command,
        Command::Open {
            note: Some("Home".to_string())
        }
    );
    assert_eq!(
        link_mentions.command,
        Command::LinkMentions {
            note: Some("Home".to_string()),
            dry_run: true,
            no_commit: false,
        }
    );
    assert_eq!(
        rewrite.command,
        Command::Rewrite {
            filters: vec!["reviewed = true".to_string()],
            stdin: false,
            find: "release".to_string(),
            replace: "launch".to_string(),
            dry_run: true,
            no_commit: false,
        }
    );
    assert_eq!(
        vectors.command,
        Command::Vectors {
            command: VectorsCommand::Index { dry_run: true },
        }
    );
    assert_eq!(
        vector_repair.command,
        Command::Vectors {
            command: VectorsCommand::Repair { dry_run: true },
        }
    );
    assert_eq!(
        vector_rebuild.command,
        Command::Vectors {
            command: VectorsCommand::Rebuild { dry_run: true },
        }
    );
    assert_eq!(
        vector_queue.command,
        Command::Vectors {
            command: VectorsCommand::Queue {
                command: VectorQueueCommand::Status,
            },
        }
    );
    assert_eq!(
        vector_related.command,
        Command::Vectors {
            command: VectorsCommand::Related {
                note: Some("Home".to_string()),
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        duplicates.command,
        Command::Vectors {
            command: VectorsCommand::Duplicates {
                threshold: 0.95,
                limit: 50,
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        cluster.command,
        Command::Vectors {
            command: VectorsCommand::Cluster {
                clusters: 3,
                dry_run: true,
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        related.command,
        Command::Vectors {
            command: VectorsCommand::Related {
                note: Some("Home".to_string()),
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(browse.command, Command::Browse { no_commit: false });
    assert_eq!(browse.refresh, None);
    assert_eq!(refreshed_browse.refresh, Some(RefreshMode::Background));
    assert_eq!(
        refreshed_browse.command,
        Command::Browse { no_commit: false }
    );
    assert_eq!(
        related_picker.command,
        Command::Vectors {
            command: VectorsCommand::Related {
                note: None,
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        edit.command,
        Command::Edit {
            note: Some("Home".to_string()),
            new: false,
            no_commit: false,
        }
    );
    assert_eq!(
        edit_new.command,
        Command::Edit {
            note: Some("Notes/Idea".to_string()),
            new: true,
            no_commit: false,
        }
    );
    assert_eq!(
        move_command.command,
        Command::Move {
            source: "Projects/Alpha.md".to_string(),
            dest: "Archive/Alpha.md".to_string(),
            dry_run: true,
            no_commit: false,
        }
    );
    assert_eq!(
        Cli::try_parse_from(["vulcan", "rename-property", "status", "phase", "--dry-run"])
            .expect("cli should parse")
            .command,
        Command::RenameProperty {
            old: "status".to_string(),
            new: "phase".to_string(),
            dry_run: true,
            no_commit: false,
        }
    );
    assert_eq!(
        Cli::try_parse_from(["vulcan", "merge-tags", "project", "initiative", "--dry-run"])
            .expect("cli should parse")
            .command,
        Command::MergeTags {
            source: "project".to_string(),
            dest: "initiative".to_string(),
            dry_run: true,
            no_commit: false,
        }
    );
    assert_eq!(
        Cli::try_parse_from([
            "vulcan",
            "rename-alias",
            "Home",
            "Start",
            "Landing",
            "--dry-run"
        ])
        .expect("cli should parse")
        .command,
        Command::RenameAlias {
            note: "Home".to_string(),
            old: "Start".to_string(),
            new: "Landing".to_string(),
            dry_run: true,
            no_commit: false,
        }
    );
    assert_eq!(
        Cli::try_parse_from([
            "vulcan",
            "rename-heading",
            "Projects/Alpha",
            "Status",
            "Progress",
            "--dry-run"
        ])
        .expect("cli should parse")
        .command,
        Command::RenameHeading {
            note: "Projects/Alpha".to_string(),
            old: "Status".to_string(),
            new: "Progress".to_string(),
            dry_run: true,
            no_commit: false,
        }
    );
    assert_eq!(
        Cli::try_parse_from([
            "vulcan",
            "rename-block-ref",
            "Projects/Alpha",
            "alpha-status",
            "alpha-progress",
            "--dry-run"
        ])
        .expect("cli should parse")
        .command,
        Command::RenameBlockRef {
            note: "Projects/Alpha".to_string(),
            old: "alpha-status".to_string(),
            new: "alpha-progress".to_string(),
            dry_run: true,
            no_commit: false,
        }
    );
    assert_eq!(
        completions.command,
        Command::Completions {
            shell: clap_complete::Shell::Bash
        }
    );
    assert_eq!(
        saved_search.command,
        Command::Saved {
            command: SavedCommand::Create {
                command: SavedCreateCommand::Search {
                    name: "weekly".to_string(),
                    query: "dashboard".to_string(),
                    filters: vec!["reviewed = true".to_string()],
                    mode: SearchMode::Keyword,
                    tag: None,
                    path_prefix: None,
                    has_property: None,
                    sort: None,
                    match_case: false,
                    context_size: 18,
                    raw_query: true,
                    fuzzy: true,
                    description: Some("weekly dashboard".to_string()),
                    export: ExportArgs {
                        export: Some(ExportFormat::Csv),
                        export_path: Some(PathBuf::from("exports/weekly.csv")),
                    },
                },
            },
        }
    );
    assert_eq!(
        saved_run.command,
        Command::Saved {
            command: SavedCommand::Run {
                name: "weekly".to_string(),
                export: ExportArgs {
                    export: Some(ExportFormat::Jsonl),
                    export_path: Some(PathBuf::from("exports/weekly.jsonl")),
                },
            },
        }
    );
    assert_eq!(
        checkpoint_create.command,
        Command::Checkpoint {
            command: CheckpointCommand::Create {
                name: "weekly".to_string(),
            },
        }
    );
    assert_eq!(
        checkpoint_list.command,
        Command::Checkpoint {
            command: CheckpointCommand::List {
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        changes.command,
        Command::Changes {
            checkpoint: Some("weekly".to_string()),
            export: ExportArgs::default(),
        }
    );
    assert_eq!(
        today.command,
        Command::Today {
            no_edit: true,
            no_commit: false,
        }
    );
    assert_eq!(
        automation_list.command,
        Command::Automation {
            command: AutomationCommand::List,
        }
    );
    assert_eq!(
        automation.command,
        Command::Automation {
            command: AutomationCommand::Run {
                reports: Vec::new(),
                all_reports: true,
                scan: true,
                doctor: true,
                doctor_fix: false,
                verify_cache: true,
                repair_fts: true,
                fail_on_issues: true,
            }
        }
    );
}

#[test]
fn parses_site_build_and_serve_commands() {
    let build = Cli::try_parse_from([
        "vulcan",
        "site",
        "build",
        "--profile",
        "public",
        "--output-dir",
        "dist",
        "--clean",
        "--watch",
        "--strict",
        "--fail-on-warning",
        "--debounce-ms",
        "75",
    ])
    .expect("cli should parse");
    let serve = Cli::try_parse_from([
        "vulcan",
        "site",
        "serve",
        "--profile",
        "public",
        "--output-dir",
        "dist",
        "--port",
        "43110",
        "--watch",
        "--strict",
        "--fail-on-warning",
        "--debounce-ms",
        "60",
    ])
    .expect("cli should parse");

    assert_eq!(
        build.command,
        Command::Site {
            command: SiteCommand::Build {
                profile: Some("public".to_string()),
                output_dir: Some(PathBuf::from("dist")),
                clean: true,
                dry_run: false,
                watch: true,
                strict: true,
                fail_on_warning: true,
                debounce_ms: 75,
            },
        }
    );
    assert_eq!(
        serve.command,
        Command::Site {
            command: SiteCommand::Serve {
                profile: Some("public".to_string()),
                output_dir: Some(PathBuf::from("dist")),
                port: 43110,
                watch: true,
                strict: true,
                fail_on_warning: true,
                debounce_ms: 60,
            },
        }
    );
}

#[test]
fn parses_global_flags_and_scan_options() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "--vault",
        "/tmp/vault",
        "--output",
        "json",
        "--fields",
        "source_path,raw_text",
        "--limit",
        "10",
        "--offset",
        "2",
        "--verbose",
        "scan",
        "--full",
    ])
    .expect("cli should parse");

    assert_eq!(cli.vault, PathBuf::from("/tmp/vault"));
    assert_eq!(cli.output, OutputFormat::Json);
    assert_eq!(
        cli.fields,
        Some(vec!["source_path".to_string(), "raw_text".to_string()])
    );
    assert_eq!(cli.limit, Some(10));
    assert_eq!(cli.offset, 2);
    assert!(cli.verbose);
    assert_eq!(
        cli.command,
        Command::Scan {
            full: true,
            no_commit: false,
        }
    );
}

#[test]
fn parses_query_format_and_glob_flags() {
    let cli = Cli::try_parse_from([
        "vulcan",
        "query",
        "--format",
        "paths",
        "--glob",
        "Projects/**",
        "from notes where file.name matches \"^2026-\"",
    ])
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Command::Query {
            dsl: Some("from notes where file.name matches \"^2026-\"".to_string()),
            json: None,
            filters: Vec::new(),
            sort: None,
            desc: false,
            list_fields: false,
            engine: QueryEngineArg::Auto,
            format: QueryFormatArg::Paths,
            glob: Some("Projects/**".to_string()),
            explain: false,
            exit_code: false,
            export: ExportArgs::default(),
        }
    );
}

#[test]
fn legacy_notes_confusion_hint_points_note_where_to_query() {
    let hint = detect_command_confusion(&[
        OsString::from("vulcan"),
        OsString::from("note"),
        OsString::from("--where"),
        OsString::from("status = done"),
    ])
    .expect("note --where should produce a hint");

    assert!(hint.contains("vulcan query --where"));
    assert!(!hint.contains("vulcan notes --where"));
}

#[test]
fn parses_ls_and_refactor_group_commands() {
    let ls = Cli::try_parse_from([
        "vulcan",
        "ls",
        "--where",
        "status = done",
        "--tag",
        "project",
        "--format",
        "detail",
    ])
    .expect("ls should parse");
    let refactor = Cli::try_parse_from([
        "vulcan",
        "refactor",
        "rename-property",
        "status",
        "phase",
        "--dry-run",
    ])
    .expect("refactor should parse");

    assert_eq!(
        ls.command,
        Command::Ls {
            filters: vec!["status = done".to_string()],
            glob: None,
            tag: Some("project".to_string()),
            format: QueryFormatArg::Detail,
            export: ExportArgs::default(),
        }
    );
    assert_eq!(
        refactor.command,
        Command::Refactor {
            command: RefactorCommand::RenameProperty {
                old: "status".to_string(),
                new: "phase".to_string(),
                dry_run: true,
                no_commit: false,
            },
        }
    );
}

#[test]
fn parses_help_and_describe_format_commands() {
    let help = Cli::try_parse_from(["vulcan", "help", "note", "get", "--output", "json"])
        .expect("help should parse");
    let describe = Cli::try_parse_from([
        "vulcan",
        "describe",
        "--format",
        "openai-tools",
        "--tool-pack",
        "notes-read,search",
        "--tool-pack",
        "web",
        "--tool-pack-mode",
        "adaptive",
    ])
    .expect("describe should parse");
    let mcp = Cli::try_parse_from([
        "vulcan",
        "--permissions",
        "readonly",
        "mcp",
        "--tool-pack",
        "notes-read,notes-manage",
        "--tool-pack",
        "index",
        "--tool-pack-mode",
        "adaptive",
        "--transport",
        "http",
        "--bind",
        "127.0.0.1:9123",
        "--endpoint",
        "/custom-mcp",
        "--auth-token",
        "secret-token",
    ])
    .expect("mcp should parse");

    assert_eq!(
        help.command,
        Command::Help {
            search: None,
            topic: vec!["note".to_string(), "get".to_string()],
        }
    );
    assert_eq!(
        describe.command,
        Command::Describe {
            format: DescribeFormatArg::OpenaiTools,
            tool_pack: vec![
                McpToolPackArg::NotesRead,
                McpToolPackArg::Search,
                McpToolPackArg::Web,
            ],
            tool_pack_mode: McpToolPackModeArg::Adaptive,
        }
    );
    assert_eq!(
        mcp.command,
        Command::Mcp {
            tool_pack: vec![
                McpToolPackArg::NotesRead,
                McpToolPackArg::NotesManage,
                McpToolPackArg::Index,
            ],
            tool_pack_mode: McpToolPackModeArg::Adaptive,
            transport: McpTransportArg::Http,
            request_timeout: "120s".to_string(),
            bind: "127.0.0.1:9123".to_string(),
            endpoint: "/custom-mcp".to_string(),
            auth_token: Some("secret-token".to_string()),
            public_url: None,
            oauth_issuer: None,
            oauth_audience: Vec::new(),
            oauth_jwks_url: None,
            oauth_allowed_sub: Vec::new(),
            oauth_allowed_email: Vec::new(),
            oauth_local_client_id: None,
            oauth_local_client_secret: None,
            oauth_local_approval_token: None,
            oauth_local_subject: Some("local-user".to_string()),
            oauth_local_email: None,
            oauth_dcr: false,
            oauth_dcr_allowed_redirect_host: Vec::new(),
            oauth_indieauth_authorization_endpoint: None,
            oauth_indieauth_token_endpoint: None,
            oauth_indieauth_client_id: None,
            oauth_indieauth_redirect_uri: None,
            oauth_indieauth_me: None,
            oauth_local_user: Vec::new(),
        }
    );
    assert_eq!(mcp.permissions.as_deref(), Some("readonly"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn parses_tool_commands() {
    let list = Cli::try_parse_from(["vulcan", "tool", "list"]).expect("tool list should parse");
    let show = Cli::try_parse_from(["vulcan", "tool", "show", "skill_meeting_summarize"])
        .expect("tool show should parse");
    let help = Cli::try_parse_from(["vulcan", "tool", "help", "meeting-summary"])
        .expect("tool help should parse");
    let test = Cli::try_parse_from([
        "vulcan",
        "tool",
        "test",
        "meeting-summary",
        "--example",
        "smoke",
        "--update-expected",
        "--profile",
        "daily-wiki-agent",
    ])
    .expect("tool test should parse");
    let compat = Cli::try_parse_from([
        "vulcan",
        "tool",
        "compat",
        "meeting-summary",
        "--surface",
        "cli,mcp",
    ])
    .expect("tool compat should parse");
    let types = Cli::try_parse_from(["vulcan", "tool", "types", "meeting-summary"])
        .expect("tool types should parse");
    let ci = Cli::try_parse_from([
        "vulcan",
        "tool",
        "ci",
        "--profile",
        "daily-wiki-agent",
        "--surface",
        "cli,mcp",
    ])
    .expect("tool ci should parse");
    let run = Cli::try_parse_from([
        "vulcan",
        "tool",
        "run",
        "skill_meeting_summarize",
        "--input-json",
        "{\"note\":\"Meetings/Weekly.md\"}",
    ])
    .expect("tool run should parse");

    assert_eq!(
        list.command,
        Command::Tool {
            command: ToolCommand::List,
        }
    );
    assert_eq!(
        show.command,
        Command::Tool {
            command: ToolCommand::Show {
                name: "skill_meeting_summarize".to_string(),
            },
        }
    );
    assert_eq!(
        help.command,
        Command::Tool {
            command: ToolCommand::Help {
                name: "meeting-summary".to_string(),
            },
        }
    );
    assert_eq!(
        test.command,
        Command::Tool {
            command: ToolCommand::Test {
                name: Some("meeting-summary".to_string()),
                all: false,
                example: Some("smoke".to_string()),
                update_expected: true,
                profile: Some("daily-wiki-agent".to_string()),
            },
        }
    );
    assert_eq!(
        compat.command,
        Command::Tool {
            command: ToolCommand::Compat {
                name: "meeting-summary".to_string(),
                surface: vec!["cli".to_string(), "mcp".to_string()],
            },
        }
    );
    assert_eq!(
        types.command,
        Command::Tool {
            command: ToolCommand::Types {
                name: Some("meeting-summary".to_string()),
                all: false,
            },
        }
    );
    assert_eq!(
        ci.command,
        Command::Tool {
            command: ToolCommand::Ci {
                profile: Some("daily-wiki-agent".to_string()),
                surface: vec!["cli".to_string(), "mcp".to_string()],
            },
        }
    );
    assert_eq!(
        run.command,
        Command::Tool {
            command: ToolCommand::Run {
                name: "skill_meeting_summarize".to_string(),
                input_json: Some("{\"note\":\"Meetings/Weekly.md\"}".to_string()),
                input_file: None,
                args: Vec::new(),
            },
        }
    );
}

#[test]
fn parses_tool_test_all_command() {
    let test_all = Cli::try_parse_from(["vulcan", "tool", "test", "--all"])
        .expect("tool test --all should parse");
    let types_all = Cli::try_parse_from(["vulcan", "tool", "types", "--all"])
        .expect("tool types --all should parse");

    assert_eq!(
        test_all.command,
        Command::Tool {
            command: ToolCommand::Test {
                name: None,
                all: true,
                example: None,
                update_expected: false,
                profile: None,
            },
        }
    );
    assert_eq!(
        types_all.command,
        Command::Tool {
            command: ToolCommand::Types {
                name: None,
                all: true,
            },
        }
    );
}

#[test]
fn parses_tool_authoring_commands() {
    let init = Cli::try_parse_from([
        "vulcan",
        "tool",
        "init",
        "meeting-summary",
        "--description",
        "Summarize meetings",
        "--command",
        "summarize",
        "--template",
        "reader",
        "--dry-run",
    ])
    .expect("tool init should parse");
    let lint = Cli::try_parse_from([
        "vulcan",
        "tool",
        "lint",
        "meeting-summary",
        "--strict",
        "--fix",
    ])
    .expect("tool lint should parse");

    assert_eq!(
        init.command,
        Command::Tool {
            command: ToolCommand::Init {
                name: "meeting-summary".to_string(),
                description: Some("Summarize meetings".to_string()),
                command: "summarize".to_string(),
                template: ToolInitTemplateArg::Reader,
                dry_run: true,
                overwrite: false,
            },
        }
    );
    assert_eq!(
        lint.command,
        Command::Tool {
            command: ToolCommand::Lint {
                name: Some("meeting-summary".to_string()),
                strict: true,
                fix: true,
            },
        }
    );
}

#[test]
fn json_schema_typescript_supports_common_schema_composition() {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["mode", "payload"],
        "properties": {
            "mode": { "const": "append" },
            "payload": {
                "anyOf": [
                    { "type": "string" },
                    { "type": "array", "items": { "type": ["string", "null"] } }
                ]
            },
            "labels": {
                "type": "object",
                "additionalProperties": { "type": "number" }
            },
            "status": { "enum": ["open", "done", null] }
        },
        "additionalProperties": false
    });

    let typescript = vulcan_app::tools::json_schema_to_typescript(&schema, 0);

    assert!(typescript.contains("mode: \"append\";"));
    assert!(typescript.contains("payload: string | (string | null)[];"));
    assert!(typescript.contains("labels?: Record<string, number>;"));
    assert!(typescript.contains("status?: \"open\" | \"done\" | null;"));
    assert!(!typescript.contains("[key: string]"));
}

#[test]
fn tool_registry_entry_converts_to_openai_and_mcp_shapes() {
    let entry = ToolRegistryEntry {
        name: "demo_tool".to_string(),
        title: "Demo Tool".to_string(),
        description: "Run a demo operation.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "note": { "type": "string" }
            },
            "required": ["note"],
            "additionalProperties": false,
        }),
        output_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "ok": { "type": "boolean" }
            },
            "required": ["ok"],
            "additionalProperties": false,
        })),
        annotations: McpToolAnnotations {
            read_only_hint: true,
            destructive_hint: false,
            idempotent_hint: true,
            open_world_hint: false,
        },
        tool_packs: vec!["custom".to_string()],
        examples: vec!["demo_tool {\"note\":\"Projects/Alpha.md\"}".to_string()],
    };

    let openai = entry.clone().into_openai_definition();
    assert_eq!(openai.function.name, "demo_tool");
    assert_eq!(openai.function.description, "Run a demo operation.");
    assert_eq!(openai.function.parameters["type"], "object");
    assert_eq!(
        openai.function.examples,
        vec!["demo_tool {\"note\":\"Projects/Alpha.md\"}".to_string()]
    );

    let mcp = entry.to_mcp_definition();
    assert_eq!(mcp.name, "demo_tool");
    assert_eq!(mcp.title, "Demo Tool");
    assert!(mcp.annotations.read_only_hint);
    assert_eq!(mcp.tool_packs, vec!["custom".to_string()]);
    assert_eq!(
        mcp.output_schema.as_ref().expect("output schema")["type"],
        "object"
    );

    let item = entry.to_mcp_list_item();
    assert_eq!(item["name"], "demo_tool");
    assert_eq!(item["title"], "Demo Tool");
    assert_eq!(item["annotations"]["readOnlyHint"], true);
    assert_eq!(item["toolPacks"], serde_json::json!(["custom"]));
    assert!(item.get("examples").is_none());
}

#[test]
fn parses_index_note_and_run_commands() {
    let index = Cli::try_parse_from(["vulcan", "index", "scan", "--full"])
        .expect("index scan should parse");
    let note_links = Cli::try_parse_from(["vulcan", "note", "links", "Dashboard"])
        .expect("note links should parse");
    let run = Cli::try_parse_from(["vulcan", "run", "demo", "--script", "--timeout", "45s"])
        .expect("run should parse");

    assert_eq!(
        index.command,
        Command::Index {
            command: IndexCommand::Scan {
                full: true,
                no_commit: false,
            },
        }
    );
    assert_eq!(
        note_links.command,
        Command::Note {
            command: NoteCommand::Links {
                note: Some("Dashboard".to_string()),
                export: ExportArgs::default(),
            },
        }
    );
    assert_eq!(
        run.command,
        Command::Run {
            script: Some("demo".to_string()),
            script_mode: true,
            eval: vec![],
            eval_file: None,
            timeout: Some("45s".to_string()),
            sandbox: None,
            no_startup: false,
        }
    );
}

#[test]
fn resolves_relative_vault_path_against_current_directory() {
    let current_dir = std::env::current_dir().expect("cwd should be available");
    let resolved = resolve_vault_root(&PathBuf::from("tests/fixtures/vaults/basic"))
        .expect("path resolution should succeed");

    assert_eq!(resolved, current_dir.join("tests/fixtures/vaults/basic"));
}

#[test]
fn resolves_tilde_prefixed_vault_path_against_home_directory() {
    let Some(home_dir) = current_home_dir() else {
        return;
    };
    let relative = PathBuf::from("vulcan/tests/fixtures/vaults/basic");
    let resolved = resolve_vault_root(&PathBuf::from("~/vulcan/tests/fixtures/vaults/basic"))
        .expect("path resolution should succeed");

    assert_eq!(resolved, home_dir.join(relative));
}

#[cfg(windows)]
fn completion_test_bash_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for key in [
        "ProgramW6432",
        "PROGRAMFILES",
        "ProgramFiles",
        "ProgramFiles(x86)",
    ] {
        let Some(base) = std::env::var_os(key).map(PathBuf::from) else {
            continue;
        };
        candidates.push(base.join("Git").join("bin").join("bash.exe"));
        candidates.push(base.join("Git").join("usr").join("bin").join("bash.exe"));
    }
    if let Ok(output) = ProcessCommand::new("git").arg("--exec-path").output() {
        if output.status.success() {
            let exec_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
            for ancestor in exec_path.ancestors().take(5) {
                candidates.push(ancestor.join("bin").join("bash.exe"));
                candidates.push(ancestor.join("usr").join("bin").join("bash.exe"));
            }
        }
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

#[cfg(not(windows))]
fn completion_test_bash_path() -> PathBuf {
    PathBuf::from("bash")
}

#[test]
fn bash_dynamic_completions_forward_global_vault_flag() {
    #[cfg(windows)]
    let Some(bash_path) = completion_test_bash_path() else {
        return;
    };
    #[cfg(not(windows))]
    let bash_path = completion_test_bash_path();
    let dynamic = generate_bash_dynamic_completions().replacen(
        &format!("local cmd=\"{}\"", completion_command_path_literal()),
        "local cmd=\"$tmpdir/vulcan\"",
        1,
    );
    let script = format!(
        r#"
set -uo pipefail
tmpdir="$(mktemp -d)"
record_path="$tmpdir/args.txt"
cat > "$tmpdir/vulcan" <<'EOF'
#!/bin/sh
set -eu
: > "$RECORD_PATH"
for arg in "$@"; do
    printf '%s\n' "$arg" >> "$RECORD_PATH"
done
printf 'Home.md\n'
EOF
chmod +x "$tmpdir/vulcan"
export RECORD_PATH="$record_path"
{dynamic}
COMP_WORDS=(vulcan --vault "/tmp/test vault" note get Ho)
COMP_CWORD=5
COMPREPLY=()
__vulcan_dynamic_complete note
for reply in "${{COMPREPLY[@]}}"; do
    printf 'REPLY:%s\n' "$reply"
done
while IFS= read -r recorded_arg; do
    printf 'ARG:%s\n' "$recorded_arg"
done < "$record_path"
"#
    );

    let output = ProcessCommand::new(&bash_path)
        .arg("-lc")
        .arg(script)
        .output()
        .expect("bash should run generated completion helper");
    assert!(
        output.status.success(),
        "bash helper should succeed (shell: {:?}, status: {:?})\nstdout:\n{}\nstderr:\n{}",
        bash_path,
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let reply_lines: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("REPLY:"))
        .collect();
    let recorded_args: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("ARG:"))
        .collect();
    assert_eq!(reply_lines, vec!["Home.md"]);
    assert_eq!(
        recorded_args,
        vec!["--vault", "/tmp/test vault", "complete", "note", "Ho"]
    );
}

#[test]
fn bash_dynamic_completions_complete_custom_tool_names_and_flags() {
    #[cfg(windows)]
    let Some(bash_path) = completion_test_bash_path() else {
        return;
    };
    #[cfg(not(windows))]
    let bash_path = completion_test_bash_path();
    let dynamic = generate_bash_dynamic_completions().replace(
        &format!("local cmd=\"{}\"", completion_command_path_literal()),
        "local cmd=\"$tmpdir/vulcan\"",
    );
    let script = format!(
        r#"
set -uo pipefail
tmpdir="$(mktemp -d)"
cat > "$tmpdir/vulcan" <<'EOF'
#!/bin/sh
set -eu
while [ "$#" -gt 0 ]; do
    if [ "$1" = "complete" ]; then
        context="$2"
        case "$context" in
            custom-tool)
                printf 'conversation-export\n'
                ;;
            custom-tool-flag:conversation-export)
                printf -- '--assistant\n'
                ;;
            custom-tool-value:conversation-export:source)
                printf 'codex\n'
                ;;
        esac
        exit 0
    fi
    shift
done
EOF
chmod +x "$tmpdir/vulcan"
_vulcan() {{ COMPREPLY=(); }}
{dynamic}
COMP_WORDS=(vulcan --vault /tmp/vault tool run con)
COMP_CWORD=5
COMPREPLY=()
__vulcan_dynamic_dispatch vulcan con run
for reply in "${{COMPREPLY[@]}}"; do
    printf 'NAME:%s\n' "$reply"
done
COMP_WORDS=(vulcan --vault /tmp/vault tool run conversation-export --ass)
COMP_CWORD=6
COMPREPLY=()
__vulcan_dynamic_dispatch vulcan --ass conversation-export
for reply in "${{COMPREPLY[@]}}"; do
    printf 'FLAG:%s\n' "$reply"
done
COMP_WORDS=(vulcan --vault /tmp/vault tool run conversation-export --source co)
COMP_CWORD=7
COMPREPLY=()
__vulcan_dynamic_dispatch vulcan co --source
for reply in "${{COMPREPLY[@]}}"; do
    printf 'VALUE:%s\n' "$reply"
done
"#
    );

    let output = ProcessCommand::new(&bash_path)
        .arg("-lc")
        .arg(script)
        .output()
        .expect("bash should run generated completion helper");
    assert!(
        output.status.success(),
        "bash helper should succeed (shell: {:?}, status: {:?})\nstdout:\n{}\nstderr:\n{}",
        bash_path,
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("NAME:conversation-export"));
    assert!(stdout.contains("FLAG:--assistant"));
    assert!(stdout.contains("VALUE:codex"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn dynamic_completion_scripts_replay_leading_global_args() {
    let fish = generate_fish_dynamic_completions();
    assert!(
        fish.contains("function __fish_vulcan_completion_prefix_args"),
        "fish completions should define a helper that collects global args"
    );
    assert!(
        fish.contains("set -l prefix (commandline -ct)"),
        "fish completions should capture the current token for prefix-aware completion"
    );
    assert!(
        fish.contains("set -l cmd \""),
        "fish completions should pin the generating vulcan binary path"
    );
    assert!(
            fish.contains("$cmd $args complete note \"$prefix\""),
            "fish note completions should replay collected args and the current token into vulcan complete"
        );
    assert!(
        fish.contains("function __fish_vulcan_dynamic_complete_note"),
        "fish completions should define a dedicated note helper"
    );
    assert!(
        fish.contains("(__fish_vulcan_dynamic_complete_note)"),
        "fish note completion should use the dedicated note helper"
    );
    assert!(
        fish.contains("function __fish_vulcan_complete_vault_path_arg"),
        "fish completions should define a dedicated vault-path helper"
    );
    assert!(
        fish.contains("(__fish_vulcan_complete_vault_path_arg)"),
        "fish path completions should use the dedicated vault-path helper"
    );
    assert!(
        fish.contains("$cmd $args complete custom-tool \"$prefix\""),
        "fish custom tool completion should replay into vulcan complete"
    );
    assert!(
        fish.contains("$cmd $args complete custom-tool-flag:$tool \"$prefix\""),
        "fish custom tool flag completion should include the selected tool"
    );
    assert!(
        fish.contains("$cmd $args complete custom-tool-value:$tool:$flag \"$prefix\""),
        "fish custom tool value completion should include the selected tool and flag"
    );

    let bash = generate_bash_dynamic_completions();
    assert!(
        bash.contains("__vulcan_completion_prefix_args"),
        "bash completions should define a helper that collects global args"
    );
    assert!(
        bash.contains("local cmd=\""),
        "bash completions should pin the generating vulcan binary path"
    );
    assert!(
        bash.contains(
            "\"$cmd\" \"${args[@]}\" complete \"$context\" \"${COMP_WORDS[COMP_CWORD]}\""
        ),
        "bash completions should replay collected args and the current token into vulcan complete"
    );
    assert!(
        bash.contains("while IFS= read -r arg; do"),
        "bash completions should collect forwarded args without relying on mapfile"
    );
    assert!(
        bash.contains("COMPREPLY+=(\"$candidate\")"),
        "bash completions should append exact completion candidates without compgen word-splitting"
    );
    assert!(
        !bash.contains("mapfile"),
        "bash completions should remain compatible with Bash 3.2 on macOS"
    );
    assert!(
        bash.contains("--vault=*"),
        "bash completions should preserve inline --vault assignments"
    );
    assert!(
        bash.contains("complete -F __vulcan_dynamic_dispatch"),
        "bash completions should install a wrapper for dynamic tool run completions"
    );
    assert!(
        bash.contains("__vulcan_dynamic_candidates custom-tool"),
        "bash custom tool completion should call vulcan complete"
    );
    assert!(
        bash.contains("__vulcan_dynamic_candidates \"custom-tool-flag:$tool\""),
        "bash custom tool flag completion should include the selected tool"
    );
    assert!(
        bash.contains("__vulcan_dynamic_candidates \"custom-tool-value:$tool:$flag\""),
        "bash custom tool value completion should include the selected tool and flag"
    );

    let zsh = generate_zsh_dynamic_completions();
    assert!(
        zsh.contains("_vulcan_completion_prefix_args"),
        "zsh completions should define a helper that collects global args"
    );
    assert!(
        zsh.contains("local cmd=\""),
        "zsh completions should pin the generating vulcan binary path"
    );
    assert!(
            zsh.contains("\"$cmd\" \"${args[@]}\" complete note \"${words[CURRENT]}\""),
            "zsh note completion should replay collected args and the current token into vulcan complete"
        );
    assert!(
        zsh.contains("--vault=*"),
        "zsh completions should preserve inline --vault assignments"
    );
    assert!(
        zsh.contains("complete custom-tool \"${words[CURRENT]}\""),
        "zsh custom tool completion should call vulcan complete"
    );
    assert!(
        zsh.contains("complete custom-tool-flag:$tool \"${words[CURRENT]}\""),
        "zsh custom tool flag completion should include the selected tool"
    );
    assert!(
        zsh.contains("custom-tool-value:$tool:$flag"),
        "zsh custom tool value completion should include the selected tool and flag"
    );
    assert!(
        zsh.contains("functions -c _vulcan _vulcan_static"),
        "zsh completions should wrap the generated completer for dynamic tool run completions"
    );
}

#[test]
fn vault_path_completion_lists_entries_relative_to_prefix() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    fs::create_dir_all(temp_dir.path().join("Projects")).expect("projects dir should exist");
    fs::create_dir_all(temp_dir.path().join(".vulcan")).expect("internal dir should exist");
    fs::write(temp_dir.path().join("Home.md"), "# Home\n").expect("note should write");
    fs::write(temp_dir.path().join("Projects/Alpha.md"), "# Alpha\n")
        .expect("nested note should write");
    let paths = VaultPaths::new(temp_dir.path().to_path_buf());

    assert_eq!(
        collect_complete_candidates(&paths, "vault-path", Some("H")),
        vec!["Home.md".to_string()]
    );
    assert_eq!(
        collect_complete_candidates(&paths, "vault-path", Some("Projects/")),
        vec!["Projects/Alpha.md".to_string()]
    );
    assert!(
        !collect_complete_candidates(&paths, "vault-path", Some(""))
            .contains(&".vulcan/".to_string()),
        "internal state directories should be hidden from vault path completions"
    );
}

#[test]
fn describe_report_lists_core_commands() {
    let report = describe_cli();

    assert_eq!(report.name, "vulcan");
    let index = report
        .commands
        .iter()
        .find(|command| command.name == "index")
        .expect("index command should be described");
    assert_eq!(
        index.about.as_deref(),
        Some("Initialize, scan, rebuild, repair, watch, and serve index state")
    );
    let completions = report
        .commands
        .iter()
        .find(|command| command.name == "completions")
        .expect("completions command should be described");
    assert_eq!(
        completions.about.as_deref(),
        Some("Generate shell completion scripts")
    );
    assert!(report.commands.iter().any(|command| command.name == "help"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "index"));
    assert!(report.commands.iter().any(|command| command.name == "note"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "kanban"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "refactor"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "config"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "browse"));
    assert!(report.commands.iter().any(|command| command.name == "edit"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "graph"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "dataview"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "cache"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "saved"));
    assert!(report.commands.iter().any(|command| command.name == "run"));
    assert!(report
        .commands
        .iter()
        .all(|command| command.name != "suggest"));
    assert!(report
        .commands
        .iter()
        .all(|command| command.name != "rewrite"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "checkpoint"));
    assert!(report
        .commands
        .iter()
        .any(|command| command.name == "changes"));
    assert!(report
        .commands
        .iter()
        .all(|command| command.name != "batch"));
    assert!(report
        .commands
        .iter()
        .all(|command| command.name != "related"));
    assert!(report
        .commands
        .iter()
        .all(|command| command.name != "cluster"));
    assert!(report
        .commands
        .iter()
        .all(|command| command.name != "weekly"));
    assert!(report
        .commands
        .iter()
        .all(|command| command.name != "monthly"));
}

fn collect_option_help_gaps(
    command_path: &mut Vec<String>,
    command: &CliCommandDescribe,
    gaps: &mut Vec<String>,
) {
    command_path.push(command.name.clone());
    for option in &command.options {
        if option_help_is_blank(option) {
            gaps.push(format!("{} --{}", command_path.join(" "), option.id));
        }
    }
    for subcommand in &command.subcommands {
        collect_option_help_gaps(command_path, subcommand, gaps);
    }
    command_path.pop();
}

fn option_help_is_blank(option: &CliArgDescribe) -> bool {
    match option.help.as_deref() {
        Some(help) => help.trim().is_empty(),
        None => true,
    }
}

#[test]
fn describe_report_options_have_help_text() {
    let report = describe_cli();
    let mut gaps = Vec::new();
    for option in &report.global_options {
        if option_help_is_blank(option) {
            gaps.push(format!("global --{}", option.id));
        }
    }
    for command in &report.commands {
        collect_option_help_gaps(&mut Vec::new(), command, &mut gaps);
    }

    assert!(
        gaps.is_empty(),
        "every described CLI option should include help text; missing: {gaps:?}"
    );
}

#[test]
fn help_overview_lists_current_top_level_surfaces() {
    let report = crate::help::help_overview();

    for expected in [
        "`mcp`",
        "`describe`",
        "`completions`",
        "`status`",
        "`plugin`",
        "`today`",
    ] {
        assert!(
            report.body.contains(expected),
            "help overview should mention {expected}"
        );
    }
}

#[test]
fn resolve_template_file_matches_by_bare_name() {
    // Build a minimal set of candidates
    let candidates = vec![
        TemplateCandidate {
            name: "daily.md".to_string(),
            display_path: ".vulcan/templates/daily.md".to_string(),
            source: "vulcan",
            absolute_path: PathBuf::from(".vulcan/templates/daily.md"),
            warning: None,
        },
        TemplateCandidate {
            name: "weekly.md".to_string(),
            display_path: ".vulcan/templates/weekly.md".to_string(),
            source: "vulcan",
            absolute_path: PathBuf::from(".vulcan/templates/weekly.md"),
            warning: None,
        },
    ];

    // Match by bare name (no extension)
    let paths = VaultPaths::new(PathBuf::from("/tmp/fake-vault"));
    let result = resolve_template_file(&paths, &candidates, "daily");
    assert!(result.is_ok(), "should match by bare name");
    assert_eq!(result.unwrap().name, "daily.md");
}

#[test]
fn resolve_template_file_matches_by_display_path_with_directory() {
    // Simulate a template whose display_path includes a directory component, as happens
    // when the Templater/Obsidian folder is a subdirectory like
    // "00-09 Management & Meta/05 Templates".
    let candidates = vec![TemplateCandidate {
        name: "daily.md".to_string(),
        display_path: "00-09 Management & Meta/05 Templates/daily.md".to_string(),
        source: "templater",
        absolute_path: PathBuf::from("00-09 Management & Meta/05 Templates/daily.md"),
        warning: None,
    }];

    let paths = VaultPaths::new(PathBuf::from("/tmp/fake-vault"));

    // Match by full directory path without extension (what periodic config provides)
    let r1 = resolve_template_file(
        &paths,
        &candidates,
        "00-09 Management & Meta/05 Templates/daily",
    );
    assert!(r1.is_ok(), "should match by display_path without .md");

    // Match by full directory path with extension
    let r2 = resolve_template_file(
        &paths,
        &candidates,
        "00-09 Management & Meta/05 Templates/daily.md",
    );
    assert!(r2.is_ok(), "should match by display_path with .md");

    // Match by bare name still works
    let r3 = resolve_template_file(&paths, &candidates, "daily");
    assert!(r3.is_ok(), "bare name should still match");
}

#[test]
fn list_templates_in_directory_scans_subdirectories() {
    use std::fs;

    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let root = tmp.path();

    // Create a nested template
    let sub = root.join("subdir");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("nested.md"), "# Nested").unwrap();
    // Also a top-level one
    fs::write(root.join("top.md"), "# Top").unwrap();
    // Non-markdown file should be ignored
    fs::write(root.join("ignored.txt"), "ignore me").unwrap();

    let templates =
        list_templates_in_directory(root, "Templates", "test").expect("should list templates");

    assert_eq!(templates.len(), 2, "should find both .md files");
    let names: Vec<&str> = templates.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"nested.md"), "nested.md should be found");
    assert!(names.contains(&"top.md"), "top.md should be found");

    let nested = templates.iter().find(|t| t.name == "nested.md").unwrap();
    assert!(
        nested.display_path.contains("subdir"),
        "display_path should include subdir"
    );
}
