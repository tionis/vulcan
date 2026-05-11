use super::{
    apply_task_add, apply_task_archive, apply_task_complete, apply_task_convert, apply_task_create,
    apply_task_pomodoro_start, apply_task_pomodoro_stop, apply_task_reschedule, apply_task_set,
    apply_task_track_start, apply_task_track_stop, build_task_due_report,
    build_task_pomodoro_status_report, build_task_reminders_report, build_task_show_report,
    build_task_track_log_report, build_task_track_status_report, build_task_track_summary_report,
    build_tasks_blocked_report, build_tasks_eval_report, build_tasks_graph_report,
    build_tasks_list_report, build_tasks_next_report, build_tasks_view_list_report,
    build_tasks_view_report, current_utc_date_string, process_due_tasknote_auto_archives,
    TaskAddRequest, TaskArchiveRequest, TaskCompleteRequest, TaskConvertRequest, TaskCreateRequest,
    TaskEvalRequest, TaskListRequest, TaskPomodoroStartRequest, TaskPomodoroStopRequest,
    TaskRescheduleRequest, TaskSetRequest, TaskTrackStartRequest, TaskTrackStopRequest,
    TaskTrackSummaryPeriod,
};
use crate::templates::render_note_from_parts;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use std::fs;
use tempfile::tempdir;
use vulcan_core::{
    initialize_vulcan_dir, load_vault_config, scan_vault_with_progress, ScanMode, VaultPaths,
};

#[test]
fn process_due_tasknote_auto_archives_moves_completed_tasks() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::write(
        paths.config_file(),
        concat!(
            "tasknotes.default_status = \"open\"\n",
            "tasknotes.default_priority = \"normal\"\n",
            "tasknotes.archive_folder = \"Archive/Tasks\"\n\n",
            "[[tasknotes.statuses]]\n",
            "id = \"open\"\n",
            "value = \"open\"\n",
            "label = \"Open\"\n",
            "color = \"#808080\"\n",
            "isCompleted = false\n",
            "order = 1\n",
            "autoArchive = false\n",
            "autoArchiveDelay = 5\n\n",
            "[[tasknotes.statuses]]\n",
            "id = \"done\"\n",
            "value = \"done\"\n",
            "label = \"Done\"\n",
            "color = \"#16a34a\"\n",
            "isCompleted = true\n",
            "order = 2\n",
            "autoArchive = true\n",
            "autoArchiveDelay = 0\n",
        ),
    )
    .expect("config should write");
    let config = load_vault_config(&paths).config;
    let completed_key = config.tasknotes.field_mapping.completed_date.clone();
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Done.md",
        "Done",
        "done",
        &[(
            completed_key.as_str(),
            YamlValue::String("2026-04-01T09:00:00Z".to_string()),
        )],
        "",
    )
    .expect("seed task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan should succeed");

    let changed_paths =
        process_due_tasknote_auto_archives(&paths, None).expect("auto archive should succeed");

    assert_eq!(
        changed_paths,
        vec![
            "Archive/Tasks/Done.md".to_string(),
            "Tasks/Done.md".to_string(),
        ]
    );
    assert!(!paths.vault_root().join("Tasks/Done.md").exists());
    assert!(paths.vault_root().join("Archive/Tasks/Done.md").exists());
}

#[test]
fn apply_task_set_marks_completed_tasks_with_completed_date() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    let config = load_vault_config(&paths).config;
    seed_tasknote(&paths, &config, "Tasks/Alpha.md", "Alpha", "open", &[], "").expect("seed task");

    let report = apply_task_set(
        &paths,
        &TaskSetRequest {
            task: "Tasks/Alpha".to_string(),
            property: "status".to_string(),
            value: first_completed_status_for_test(&config),
            dry_run: false,
        },
    )
    .expect("set report");

    assert_eq!(report.action, "set");
    assert_eq!(report.path, "Tasks/Alpha.md");
    assert_eq!(report.changed_paths, vec!["Tasks/Alpha.md".to_string()]);

    let rendered = fs::read_to_string(temp_dir.path().join("Tasks/Alpha.md"))
        .expect("updated task")
        .replace("\r\n", "\n");
    assert!(rendered.contains(&format!(
        "{}: {}",
        config.tasknotes.field_mapping.completed_date,
        current_utc_date_string()
    )));
}

#[test]
fn apply_task_complete_updates_recurring_instance_lists() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    let config = load_vault_config(&paths).config;
    let recurrence_key = config.tasknotes.field_mapping.recurrence.clone();
    let skipped_key = config.tasknotes.field_mapping.skipped_instances.clone();
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Recurring.md",
        "Recurring",
        "open",
        &[
            (
                recurrence_key.as_str(),
                YamlValue::String("every day".to_string()),
            ),
            (
                skipped_key.as_str(),
                YamlValue::Sequence(vec![YamlValue::String("2026-04-21".to_string())]),
            ),
        ],
        "",
    )
    .expect("seed recurring task");

    let report = apply_task_complete(
        &paths,
        &TaskCompleteRequest {
            task: "Tasks/Recurring".to_string(),
            date: Some("2026-04-21".to_string()),
            dry_run: false,
        },
    )
    .expect("complete report");

    assert_eq!(report.action, "complete");
    assert_eq!(report.path, "Tasks/Recurring.md");

    let rendered = fs::read_to_string(temp_dir.path().join("Tasks/Recurring.md"))
        .expect("updated recurring task")
        .replace("\r\n", "\n");
    assert!(rendered.contains(&format!(
        "{}:\n- 2026-04-21",
        config.tasknotes.field_mapping.complete_instances
    )));
    assert!(!rendered.contains(&format!(
        "{}:\n- 2026-04-21",
        config.tasknotes.field_mapping.skipped_instances
    )));
}

#[test]
fn apply_task_reschedule_updates_inline_task_due_marker() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::write(temp_dir.path().join("Inbox.md"), "- [ ] Call Alice\n").expect("seed note");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = apply_task_reschedule(
        &paths,
        &TaskRescheduleRequest {
            task: "Inbox.md:1".to_string(),
            due: "2026-04-20".to_string(),
            dry_run: false,
        },
    )
    .expect("reschedule report");

    assert_eq!(report.action, "reschedule");
    assert_eq!(report.path, "Inbox.md");
    assert_eq!(report.changed_paths, vec!["Inbox.md".to_string()]);
    let rendered = fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("updated note");
    assert!(rendered.contains("- [ ] Call Alice 🗓️ 2026-04-20"));
}

#[test]
fn apply_task_reschedule_dry_run_reports_inline_changed_path() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::write(temp_dir.path().join("Inbox.md"), "- [ ] Call Alice\n").expect("seed note");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = apply_task_reschedule(
        &paths,
        &TaskRescheduleRequest {
            task: "Inbox.md:1".to_string(),
            due: "2026-04-20".to_string(),
            dry_run: true,
        },
    )
    .expect("reschedule report");

    assert_eq!(report.changed_paths, vec!["Inbox.md".to_string()]);
    let rendered = fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("source note");
    assert_eq!(rendered, "- [ ] Call Alice\n");
}

#[test]
fn apply_task_complete_updates_inline_task_checkbox_and_date() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::write(temp_dir.path().join("Inbox.md"), "- [ ] Call Alice\n").expect("seed note");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = apply_task_complete(
        &paths,
        &TaskCompleteRequest {
            task: "Inbox.md:1".to_string(),
            date: Some("2026-04-20".to_string()),
            dry_run: false,
        },
    )
    .expect("complete report");

    assert_eq!(report.action, "complete");
    assert_eq!(report.path, "Inbox.md");
    let rendered = fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("updated note");
    assert!(rendered.contains("- [x] Call Alice ✅ 2026-04-20"));
}

#[test]
fn apply_task_complete_dry_run_reports_inline_changed_path() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::write(temp_dir.path().join("Inbox.md"), "- [ ] Call Alice\n").expect("seed note");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = apply_task_complete(
        &paths,
        &TaskCompleteRequest {
            task: "Inbox.md:1".to_string(),
            date: Some("2026-04-20".to_string()),
            dry_run: true,
        },
    )
    .expect("complete report");

    assert_eq!(report.changed_paths, vec!["Inbox.md".to_string()]);
    let rendered = fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("source note");
    assert_eq!(rendered, "- [ ] Call Alice\n");
}

#[test]
fn apply_task_add_creates_tasknote_from_natural_language_input() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    let config = load_vault_config(&paths).config;

    let report = apply_task_add(
        &paths,
        &TaskAddRequest {
            text: "Review launch plan tomorrow @work #shipit".to_string(),
            no_nlp: false,
            status: None,
            priority: None,
            due: None,
            scheduled: None,
            contexts: Vec::new(),
            projects: Vec::new(),
            tags: Vec::new(),
            template: None,
            dry_run: false,
        },
    )
    .expect("add report");

    assert_eq!(report.action, "add");
    assert_eq!(report.title, "Review launch plan");
    let expected_path = format!(
        "{}/Review launch plan.md",
        config.tasknotes.tasks_folder.trim_end_matches('/')
    );
    assert_eq!(report.path, expected_path);
    assert_eq!(report.changed_paths, vec![report.path.clone()]);

    let rendered = fs::read_to_string(temp_dir.path().join(&report.path))
        .expect("created task")
        .replace("\r\n", "\n");
    assert!(rendered.contains("title: Review launch plan"));
    assert!(rendered.contains("@work"));
    assert!(rendered.contains("shipit"));
}

#[test]
fn apply_task_add_dry_run_reports_changed_path() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");

    let report = apply_task_add(
        &paths,
        &TaskAddRequest {
            text: "Review launch plan tomorrow".to_string(),
            no_nlp: false,
            status: None,
            priority: None,
            due: None,
            scheduled: None,
            contexts: Vec::new(),
            projects: Vec::new(),
            tags: Vec::new(),
            template: None,
            dry_run: true,
        },
    )
    .expect("add report");

    assert_eq!(report.changed_paths, vec![report.path.clone()]);
    assert!(!temp_dir.path().join(&report.path).exists());
}

#[test]
fn apply_task_create_appends_inline_task_to_target_note() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::write(temp_dir.path().join("Inbox.md"), "# Tasks\n").expect("seed inbox");

    let report = apply_task_create(
        &paths,
        &TaskCreateRequest {
            text: "Call Alice".to_string(),
            note: Some("Inbox".to_string()),
            due: Some("2026-04-20".to_string()),
            priority: Some("high".to_string()),
            dry_run: false,
        },
    )
    .expect("create report");

    assert_eq!(report.action, "create");
    assert_eq!(report.path, "Inbox.md");
    assert_eq!(report.line_number, 3);
    let rendered = fs::read_to_string(temp_dir.path().join("Inbox.md"))
        .expect("updated inbox")
        .replace("\r\n", "\n");
    assert!(rendered.contains("- [ ] Call Alice 🗓️ 2026-04-20 🔺"));
}

#[test]
fn apply_task_create_dry_run_reports_changed_path() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::write(temp_dir.path().join("Inbox.md"), "# Tasks\n").expect("seed inbox");

    let report = apply_task_create(
        &paths,
        &TaskCreateRequest {
            text: "Call Alice".to_string(),
            note: Some("Inbox".to_string()),
            due: None,
            priority: None,
            dry_run: true,
        },
    )
    .expect("create report");

    assert_eq!(report.changed_paths, vec!["Inbox.md".to_string()]);
    let rendered = fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("original inbox");
    assert_eq!(rendered, "# Tasks\n");
}

#[test]
fn apply_task_convert_note_promotes_existing_note_to_tasknote() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::create_dir_all(temp_dir.path().join("Ideas")).expect("ideas dir");
    fs::write(temp_dir.path().join("Ideas/Alpha.md"), "Alpha details\n").expect("seed note");

    let report = apply_task_convert(
        &paths,
        &TaskConvertRequest {
            file: "Ideas/Alpha".to_string(),
            line: None,
            dry_run: false,
        },
    )
    .expect("convert note report");

    assert_eq!(report.mode, "note");
    assert_eq!(report.source_path, "Ideas/Alpha.md");
    assert_eq!(report.target_path, "Ideas/Alpha.md");
    let rendered = fs::read_to_string(temp_dir.path().join("Ideas/Alpha.md"))
        .expect("converted note")
        .replace("\r\n", "\n");
    assert!(rendered.contains("title: Alpha"));
    assert!(rendered.contains("status: open"));
}

#[test]
fn apply_task_convert_note_dry_run_reports_changed_path() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::create_dir_all(temp_dir.path().join("Ideas")).expect("ideas dir");
    fs::write(temp_dir.path().join("Ideas/Alpha.md"), "Alpha details\n").expect("seed note");

    let report = apply_task_convert(
        &paths,
        &TaskConvertRequest {
            file: "Ideas/Alpha".to_string(),
            line: None,
            dry_run: true,
        },
    )
    .expect("convert note report");

    assert_eq!(report.changed_paths, vec!["Ideas/Alpha.md".to_string()]);
    let rendered = fs::read_to_string(temp_dir.path().join("Ideas/Alpha.md")).expect("source note");
    assert_eq!(rendered, "Alpha details\n");
}

#[test]
fn apply_task_convert_line_creates_tasknote_and_rewrites_source() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::write(
        temp_dir.path().join("Inbox.md"),
        "- [ ] Review launch plan tomorrow @work\n",
    )
    .expect("seed inbox");

    let report = apply_task_convert(
        &paths,
        &TaskConvertRequest {
            file: "Inbox".to_string(),
            line: Some(1),
            dry_run: false,
        },
    )
    .expect("convert line report");

    assert_eq!(report.mode, "line");
    assert_eq!(report.source_path, "Inbox.md");
    assert!(temp_dir.path().join(&report.target_path).exists());

    let source = fs::read_to_string(temp_dir.path().join("Inbox.md"))
        .expect("rewritten inbox")
        .replace("\r\n", "\n");
    let link_target = report.target_path.trim_end_matches(".md");
    assert!(source.contains(&format!("[[{link_target}]]")));
}

#[test]
fn apply_task_convert_line_dry_run_reports_both_changed_paths() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::write(
        temp_dir.path().join("Inbox.md"),
        "- [ ] Review launch plan tomorrow @work\n",
    )
    .expect("seed inbox");

    let report = apply_task_convert(
        &paths,
        &TaskConvertRequest {
            file: "Inbox".to_string(),
            line: Some(1),
            dry_run: true,
        },
    )
    .expect("convert line report");

    assert_eq!(
        report.changed_paths,
        vec!["Inbox.md".to_string(), report.target_path.clone()]
    );
    assert!(!temp_dir.path().join(&report.target_path).exists());
}

#[test]
fn apply_task_archive_moves_completed_task_into_archive_folder() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Done.md",
        "Done",
        &first_completed_status_for_test(&config),
        &[],
        "",
    )
    .expect("seed completed task");

    let report = apply_task_archive(
        &paths,
        &TaskArchiveRequest {
            task: "Tasks/Done".to_string(),
            dry_run: false,
        },
    )
    .expect("archive report");

    let archived_path = format!("{}/Done.md", config.tasknotes.archive_folder);
    assert_eq!(report.action, "archive");
    assert_eq!(report.path, archived_path);
    assert_eq!(report.moved_from.as_deref(), Some("Tasks/Done.md"));
    assert_eq!(report.moved_to.as_deref(), Some(report.path.as_str()));
    assert!(temp_dir.path().join(&report.path).exists());
    let rendered = fs::read_to_string(temp_dir.path().join(&report.path))
        .expect("archived task")
        .replace("\r\n", "\n");
    assert!(rendered.contains(&format!("- {}", config.tasknotes.field_mapping.archive_tag)));
}

#[test]
#[allow(clippy::too_many_lines)]
fn build_task_show_report_reports_tasknote_details_and_metrics() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    let config = load_vault_config(&paths).config;
    let mapping = &config.tasknotes.field_mapping;
    let reminder = YamlValue::Mapping(YamlMapping::from_iter([
        (
            YamlValue::String("id".to_string()),
            YamlValue::String("due-warning".to_string()),
        ),
        (
            YamlValue::String("type".to_string()),
            YamlValue::String("relative".to_string()),
        ),
        (
            YamlValue::String("relatedTo".to_string()),
            YamlValue::String("due".to_string()),
        ),
        (
            YamlValue::String("offset".to_string()),
            YamlValue::String("-PT15M".to_string()),
        ),
    ]));
    let time_entry = YamlValue::Mapping(YamlMapping::from_iter([
        (
            YamlValue::String("startTime".to_string()),
            YamlValue::String("2026-04-17T08:00:00Z".to_string()),
        ),
        (
            YamlValue::String("endTime".to_string()),
            YamlValue::String("2026-04-17T09:00:00Z".to_string()),
        ),
        (
            YamlValue::String("description".to_string()),
            YamlValue::String("Deep work".to_string()),
        ),
    ]));
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Write Docs.md",
        "Write docs",
        "in-progress",
        &[
            (
                mapping.priority.as_str(),
                YamlValue::String("high".to_string()),
            ),
            (
                mapping.due.as_str(),
                YamlValue::String("2026-04-20T10:00:00Z".to_string()),
            ),
            (
                mapping.contexts.as_str(),
                YamlValue::Sequence(vec![
                    YamlValue::String("@desk".to_string()),
                    YamlValue::String("@work".to_string()),
                ]),
            ),
            (
                mapping.projects.as_str(),
                YamlValue::Sequence(vec![YamlValue::String("[[Projects/Website]]".to_string())]),
            ),
            (
                mapping.blocked_by.as_str(),
                YamlValue::Sequence(vec![YamlValue::String(
                    "TaskNotes/Tasks/Prep Outline.md".to_string(),
                )]),
            ),
            (
                mapping.reminders.as_str(),
                YamlValue::Sequence(vec![reminder]),
            ),
            (
                mapping.time_entries.as_str(),
                YamlValue::Sequence(vec![time_entry]),
            ),
            (
                mapping.time_estimate.as_str(),
                YamlValue::Number(serde_yaml::Number::from(90_u64)),
            ),
            (
                "effort",
                serde_yaml::to_value(3.0_f64).expect("float yaml value"),
            ),
        ],
        "Write the docs body.\n",
    )
    .expect("seed task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = build_task_show_report(&paths, "Tasks/Write Docs").expect("show report");

    assert_eq!(report.path, "Tasks/Write Docs.md");
    assert_eq!(report.title, "Write docs");
    assert_eq!(report.status, "in-progress");
    assert_eq!(report.status_type, "IN_PROGRESS");
    assert!(!report.completed);
    assert_eq!(report.priority, "high");
    assert_eq!(report.due.as_deref(), Some("2026-04-20T10:00:00Z"));
    assert_eq!(report.contexts, vec!["@desk", "@work"]);
    assert_eq!(report.projects, vec!["[[Projects/Website]]"]);
    assert_eq!(report.blocked_by.len(), 1);
    assert_eq!(report.reminders.len(), 1);
    assert_eq!(report.time_entries.len(), 1);
    assert_eq!(report.total_time_minutes, 60);
    assert_eq!(report.active_time_minutes, 0);
    assert_eq!(report.estimate_remaining_minutes, Some(30));
    assert_eq!(report.efficiency_ratio, Some(67));
    assert_eq!(report.custom_fields["effort"], serde_json::json!(3.0));
    assert_eq!(report.frontmatter["title"], "Write docs");
    assert_eq!(report.body, "Write the docs body.\n");
}

#[test]
fn build_task_due_report_filters_tasks_within_window() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    let config = load_vault_config(&paths).config;
    let due_key = config.tasknotes.field_mapping.due.clone();
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Future.md",
        "Future",
        "open",
        &[(
            due_key.as_str(),
            YamlValue::String("2999-01-01T10:00:00Z".to_string()),
        )],
        "",
    )
    .expect("seed future task");
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Overdue.md",
        "Overdue",
        "open",
        &[(
            due_key.as_str(),
            YamlValue::String("2000-01-01T10:00:00Z".to_string()),
        )],
        "",
    )
    .expect("seed overdue task");
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Done.md",
        "Done",
        &first_completed_status_for_test(&config),
        &[(
            due_key.as_str(),
            YamlValue::String("2000-01-01T10:00:00Z".to_string()),
        )],
        "",
    )
    .expect("seed completed task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = build_task_due_report(&paths, "2000y").expect("due report");

    assert_eq!(report.within, "2000y");
    assert_eq!(report.tasks.len(), 2);
    assert_eq!(report.tasks[0].path, "Tasks/Overdue.md");
    assert!(report.tasks[0].overdue);
    assert_eq!(report.tasks[1].path, "Tasks/Future.md");
    assert!(!report.tasks[1].overdue);
}

#[test]
#[allow(clippy::too_many_lines)]
fn build_task_reminders_report_includes_relative_and_absolute_reminders() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    let config = load_vault_config(&paths).config;
    let mapping = &config.tasknotes.field_mapping;
    let relative_reminder = YamlValue::Mapping(YamlMapping::from_iter([
        (
            YamlValue::String("id".to_string()),
            YamlValue::String("rel-1".to_string()),
        ),
        (
            YamlValue::String("type".to_string()),
            YamlValue::String("relative".to_string()),
        ),
        (
            YamlValue::String("relatedTo".to_string()),
            YamlValue::String("due".to_string()),
        ),
        (
            YamlValue::String("offset".to_string()),
            YamlValue::String("-PT15M".to_string()),
        ),
        (
            YamlValue::String("description".to_string()),
            YamlValue::String("Before due".to_string()),
        ),
    ]));
    let absolute_reminder = YamlValue::Mapping(YamlMapping::from_iter([
        (
            YamlValue::String("id".to_string()),
            YamlValue::String("abs-1".to_string()),
        ),
        (
            YamlValue::String("type".to_string()),
            YamlValue::String("absolute".to_string()),
        ),
        (
            YamlValue::String("absoluteTime".to_string()),
            YamlValue::String("2999-01-01T09:00:00Z".to_string()),
        ),
        (
            YamlValue::String("description".to_string()),
            YamlValue::String("Absolute reminder".to_string()),
        ),
    ]));
    let far_future_reminder = YamlValue::Mapping(YamlMapping::from_iter([
        (
            YamlValue::String("id".to_string()),
            YamlValue::String("abs-2".to_string()),
        ),
        (
            YamlValue::String("type".to_string()),
            YamlValue::String("absolute".to_string()),
        ),
        (
            YamlValue::String("absoluteTime".to_string()),
            YamlValue::String("4999-01-01T09:00:00Z".to_string()),
        ),
    ]));
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Relative.md",
        "Relative",
        "open",
        &[
            (
                mapping.due.as_str(),
                YamlValue::String("2999-01-01T10:00:00Z".to_string()),
            ),
            (
                mapping.reminders.as_str(),
                YamlValue::Sequence(vec![relative_reminder]),
            ),
        ],
        "",
    )
    .expect("seed relative task");
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Absolute.md",
        "Absolute",
        "open",
        &[(
            mapping.reminders.as_str(),
            YamlValue::Sequence(vec![absolute_reminder]),
        )],
        "",
    )
    .expect("seed absolute task");
    seed_tasknote(
        &paths,
        &config,
        "Tasks/FarFuture.md",
        "Far Future",
        "open",
        &[(
            mapping.reminders.as_str(),
            YamlValue::Sequence(vec![far_future_reminder]),
        )],
        "",
    )
    .expect("seed far future task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = build_task_reminders_report(&paths, "2000y").expect("task reminders report");

    assert_eq!(report.upcoming, "2000y");
    assert_eq!(report.reminders.len(), 2);
    assert_eq!(report.reminders[0].path, "Tasks/Absolute.md");
    assert_eq!(report.reminders[0].reminder_id, "abs-1");
    assert_eq!(report.reminders[0].notify_at, "2999-01-01T09:00:00Z");
    assert!(!report.reminders[0].overdue);
    assert_eq!(report.reminders[1].path, "Tasks/Relative.md");
    assert_eq!(report.reminders[1].reminder_id, "rel-1");
    assert_eq!(report.reminders[1].notify_at, "2999-01-01T09:45:00Z");
    assert_eq!(
        report.reminders[1].description.as_deref(),
        Some("Before due")
    );
}

#[test]
fn build_tasks_next_report_lists_upcoming_recurring_instances() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    write_tasks_recurrence_fixture(&paths);
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = build_tasks_next_report(&paths, 4, Some("2026-03-29")).expect("tasks next report");

    assert_eq!(report.reference_date, "2026-03-29");
    assert_eq!(report.result_count, 4);
    assert_eq!(report.occurrences.len(), 4);
    assert_eq!(report.occurrences[0].date, "2026-03-30");
    assert_eq!(
        report.occurrences[0].task["recurrenceRule"],
        serde_json::json!("FREQ=WEEKLY;INTERVAL=2")
    );
    assert_eq!(report.occurrences[1].date, "2026-04-09");
    assert_eq!(
        report.occurrences[1].task["recurrenceRule"],
        serde_json::json!("FREQ=WEEKLY;INTERVAL=2;BYDAY=TH")
    );
    assert_eq!(report.occurrences[2].date, "2026-04-13");
    assert_eq!(report.occurrences[2].sequence, 2);
    assert_eq!(report.occurrences[3].date, "2026-04-15");
    assert_eq!(
        report.occurrences[3].task["recurrence"],
        serde_json::json!("every month on the 15th")
    );
    assert_eq!(
        report.occurrences[3].task["recurrenceMonthDay"],
        serde_json::json!(15)
    );
}

#[test]
fn build_tasks_eval_report_evaluates_selected_block_with_defaults() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    write_tasks_query_fixture(&paths);
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = build_tasks_eval_report(
        &paths,
        &TaskEvalRequest {
            file: "Dashboard".to_string(),
            block: Some(1),
        },
    )
    .expect("tasks eval report");

    assert_eq!(report.file, "Dashboard.md");
    assert_eq!(report.blocks.len(), 1);
    assert_eq!(report.blocks[0].block_index, 1);
    assert_eq!(report.blocks[0].source, "path includes Tasks");
    assert_eq!(
        report.blocks[0].effective_source.as_deref(),
        Some("tag includes #task\nnot done\npath includes Tasks")
    );
    let result = report.blocks[0].result.as_ref().expect("tasks result");
    assert_eq!(result.result_count, 2);
    assert_eq!(result.tasks[0]["text"], "Write docs");
    assert_eq!(result.tasks[1]["text"], "Plan backlog");
}

#[test]
fn build_tasks_list_report_accepts_tasks_dsl_filters() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    write_tasks_query_fixture(&paths);
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = build_tasks_list_report(
        &paths,
        &TaskListRequest {
            filter: Some("not done".to_string()),
            ..TaskListRequest::default()
        },
    )
    .expect("tasks list report");

    assert_eq!(report.result_count, 2);
    assert_eq!(report.tasks.len(), 2);
    assert_eq!(report.tasks[0]["text"], "Write docs");
    assert_eq!(report.tasks[0]["tags"], serde_json::json!([]));
    assert_eq!(report.tasks[1]["text"], "Plan backlog");
}

#[test]
fn build_tasks_view_list_report_lists_base_files_and_saved_view_aliases() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    write_tasknotes_saved_view_config(&paths);
    write_tasknotes_views_fixture(&paths);

    let report = build_tasks_view_list_report(&paths).expect("tasks view list report");

    assert!(report.views.iter().any(|view| {
        view.file == "TaskNotes/Views/tasks-default.base"
            && view.view_name.as_deref() == Some("Tasks")
            && view.view_type == "tasknotesTaskList"
            && view.supported
    }));
    assert!(report.views.iter().any(|view| {
        view.file == "config.tasknotes.saved_views.blocked"
            && view.file_stem == "blocked"
            && view.view_name.as_deref() == Some("Blocked Tasks")
            && view.view_type == "tasknotesTaskList"
            && view.supported
    }));
}

#[test]
fn build_tasks_view_report_evaluates_named_tasknotes_view() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    write_tasknotes_views_fixture(&paths);
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "TaskNotes/Tasks/Prep Outline.md",
        "Prep Outline",
        "open",
        &[],
        "",
    )
    .expect("seed task");
    seed_tasknote(
        &paths,
        &config,
        "TaskNotes/Tasks/Write Docs.md",
        "Write Docs",
        "in-progress",
        &[],
        "",
    )
    .expect("seed task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = build_tasks_view_report(&paths, "Tasks").expect("tasks view report");

    assert_eq!(report.file, "TaskNotes/Views/tasks-default.base");
    assert_eq!(report.views.len(), 1);
    assert_eq!(report.views[0].name.as_deref(), Some("Tasks"));
    assert_eq!(report.views[0].rows.len(), 2);
    assert!(report.views[0]
        .rows
        .iter()
        .any(|row| row.document_path == "TaskNotes/Tasks/Prep Outline.md"));
    assert!(report.views[0]
        .rows
        .iter()
        .any(|row| row.document_path == "TaskNotes/Tasks/Write Docs.md"));
}

#[test]
fn build_tasks_view_report_evaluates_saved_view_aliases() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    write_tasknotes_saved_view_config(&paths);
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "TaskNotes/Tasks/Prep Outline.md",
        "Prep Outline",
        "open",
        &[],
        "",
    )
    .expect("seed task");
    seed_tasknote(
        &paths,
        &config,
        "TaskNotes/Tasks/Write Docs.md",
        "Write Docs",
        "in-progress",
        &[],
        "",
    )
    .expect("seed task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = build_tasks_view_report(&paths, "blocked").expect("tasks view report");

    assert_eq!(report.file, "config.tasknotes.saved_views.blocked");
    assert_eq!(report.views.len(), 1);
    assert_eq!(report.views[0].name.as_deref(), Some("Blocked Tasks"));
    assert_eq!(report.views[0].rows.len(), 1);
    assert_eq!(
        report.views[0].rows[0].document_path,
        "TaskNotes/Tasks/Write Docs.md"
    );
}

#[test]
fn build_tasks_blocked_report_lists_open_and_unresolved_blockers() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    write_tasks_dependency_fixture(&paths);
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = build_tasks_blocked_report(&paths).expect("tasks blocked report");

    assert_eq!(report.tasks.len(), 2);
    assert_eq!(report.tasks[0].task["text"], "Publish docs ⛔ SHIP-1");
    assert_eq!(report.tasks[0].blockers[0].blocker_id, "SHIP-1");
    assert_eq!(report.tasks[0].blockers[0].blocker_completed, Some(false));
    assert_eq!(report.tasks[1].task["text"], "Prep launch ⛔ MISSING-1");
    assert!(!report.tasks[1].blockers[0].resolved);
}

#[test]
fn build_tasks_graph_report_lists_dependency_nodes_and_edges() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    write_tasks_dependency_fixture(&paths);
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = build_tasks_graph_report(&paths).expect("tasks graph report");

    assert_eq!(report.nodes.len(), 4);
    assert_eq!(report.edges.len(), 2);
    assert_eq!(report.edges[0].blocker_id, "SHIP-1");
    assert!(report.edges[0].resolved);
    assert_eq!(report.edges[1].blocker_id, "MISSING-1");
    assert!(!report.edges[1].resolved);
}

#[test]
fn task_track_workflows_update_entries_and_reports() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    let config = load_vault_config(&paths).config;
    let estimate_key = config.tasknotes.field_mapping.time_estimate.clone();
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Tracked.md",
        "Tracked",
        "open",
        &[(
            estimate_key.as_str(),
            YamlValue::Number(serde_yaml::Number::from(120_u64)),
        )],
        "",
    )
    .expect("seed task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let start = apply_task_track_start(
        &paths,
        &TaskTrackStartRequest {
            task: "Tasks/Tracked".to_string(),
            description: Some("Deep work".to_string()),
            dry_run: false,
        },
    )
    .expect("track start");

    assert_eq!(start.action, "start");
    assert_eq!(start.path, "Tasks/Tracked.md");
    assert!(start.session.active);
    assert_eq!(start.session.description.as_deref(), Some("Deep work"));
    assert_eq!(start.changed_paths, vec!["Tasks/Tracked.md".to_string()]);

    let tracked_path = temp_dir.path().join("Tasks/Tracked.md");
    let adjusted = fs::read_to_string(&tracked_path)
        .expect("tracked note")
        .replace(&start.session.start_time, "2026-04-17T08:00:00Z");
    fs::write(&tracked_path, adjusted).expect("tracked note updated");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let stop = apply_task_track_stop(
        &paths,
        &TaskTrackStopRequest {
            task: Some("Tasks/Tracked".to_string()),
            dry_run: false,
        },
    )
    .expect("track stop");

    assert_eq!(stop.action, "stop");
    assert_eq!(stop.path, "Tasks/Tracked.md");
    assert!(!stop.session.active);
    assert!(stop.total_time_minutes > 0);
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let status = build_task_track_status_report(&paths).expect("track status");
    assert_eq!(status.total_active_sessions, 0);

    let log = build_task_track_log_report(&paths, "Tasks/Tracked").expect("track log");
    assert_eq!(log.entries.len(), 1);
    assert_eq!(log.entries[0].description.as_deref(), Some("Deep work"));
    assert!(log.total_time_minutes > 0);

    let summary = build_task_track_summary_report(&paths, TaskTrackSummaryPeriod::All)
        .expect("track summary");
    assert_eq!(summary.tasks_with_time, 1);
    assert_eq!(summary.top_tasks[0].path, "Tasks/Tracked.md");
    assert!(summary.total_minutes > 0);
}

#[test]
fn task_pomodoro_start_stop_and_status_manage_task_storage() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    let config = load_vault_config(&paths).config;
    seed_tasknote(&paths, &config, "Tasks/Focus.md", "Focus", "open", &[], "").expect("seed task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let start = apply_task_pomodoro_start(
        &paths,
        &TaskPomodoroStartRequest {
            task: "Tasks/Focus".to_string(),
            dry_run: false,
        },
    )
    .expect("pomodoro start");

    assert_eq!(start.action, "start");
    assert_eq!(start.storage_note_path, "Tasks/Focus.md");
    assert!(start.session.active);
    assert_eq!(start.changed_paths, vec!["Tasks/Focus.md".to_string()]);
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let stop = apply_task_pomodoro_stop(
        &paths,
        &TaskPomodoroStopRequest {
            task: Some("Tasks/Focus".to_string()),
            dry_run: false,
        },
    )
    .expect("pomodoro stop");

    assert_eq!(stop.action, "stop");
    assert_eq!(stop.storage_note_path, "Tasks/Focus.md");
    assert!(!stop.session.active);
    assert!(stop.session.interrupted);
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let status = build_task_pomodoro_status_report(&paths).expect("pomodoro status");
    assert!(status.active.is_none());

    let rendered = fs::read_to_string(temp_dir.path().join("Tasks/Focus.md"))
        .expect("updated task")
        .replace("\r\n", "\n");
    assert!(rendered.contains("pomodoros:"));
    assert!(rendered.contains("interrupted: true"));
}

#[test]
fn task_pomodoro_status_completes_due_daily_note_sessions_without_extra_rescan() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    fs::write(
        temp_dir.path().join(".vulcan/config.toml"),
        concat!(
            "[tasknotes.pomodoro]\n",
            "work_duration = 1\n",
            "short_break = 3\n",
            "long_break = 20\n",
            "long_break_interval = 1\n",
            "storage_location = \"daily-note\"\n",
        ),
    )
    .expect("config written");
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "TaskNotes/Tasks/Prep Outline.md",
        "Prep Outline",
        "open",
        &[],
        "",
    )
    .expect("seed task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let start = apply_task_pomodoro_start(
        &paths,
        &TaskPomodoroStartRequest {
            task: "TaskNotes/Tasks/Prep Outline".to_string(),
            dry_run: false,
        },
    )
    .expect("pomodoro start");
    let daily_note_path = temp_dir.path().join(&start.storage_note_path);
    let updated = fs::read_to_string(&daily_note_path)
        .expect("daily note")
        .replace(&start.session.start_time, "2026-04-17T08:00:00Z");
    fs::write(&daily_note_path, updated).expect("daily note updated");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let status = build_task_pomodoro_status_report(&paths).expect("pomodoro status");

    assert!(status.active.is_none());
    assert_eq!(status.completed_work_sessions, 1);
    assert_eq!(status.suggested_break_type, "long-break");
    assert_eq!(status.suggested_break_minutes, 20);

    let rendered = fs::read_to_string(&daily_note_path)
        .expect("daily note rendered")
        .replace("\r\n", "\n");
    assert!(rendered.contains("completed: true"));
    assert!(rendered.contains("taskPath: TaskNotes/Tasks/Prep Outline.md"));
}

fn seed_tasknote(
    paths: &VaultPaths,
    config: &VaultConfig,
    relative_path: &str,
    title: &str,
    status: &str,
    extra_fields: &[(&str, YamlValue)],
    body: &str,
) -> Result<(), AppError> {
    let mapping = &config.tasknotes.field_mapping;
    let mut frontmatter = YamlMapping::new();
    frontmatter.insert(
        YamlValue::String(mapping.title.clone()),
        YamlValue::String(title.to_string()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.status.clone()),
        YamlValue::String(status.to_string()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.priority.clone()),
        YamlValue::String(config.tasknotes.default_priority.clone()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.date_created.clone()),
        YamlValue::String("2026-04-17T09:00:00Z".to_string()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.date_modified.clone()),
        YamlValue::String("2026-04-17T09:00:00Z".to_string()),
    );
    match config.tasknotes.identification_method {
        vulcan_core::TaskNotesIdentificationMethod::Tag => {
            frontmatter.insert(
                YamlValue::String("tags".to_string()),
                YamlValue::Sequence(vec![YamlValue::String(config.tasknotes.task_tag.clone())]),
            );
        }
        vulcan_core::TaskNotesIdentificationMethod::Property => {
            if let Some(property_name) = config.tasknotes.task_property_name.as_ref() {
                let property_value = config
                    .tasknotes
                    .task_property_value
                    .as_ref()
                    .map_or(YamlValue::Bool(true), |value| {
                        YamlValue::String(value.clone())
                    });
                frontmatter.insert(YamlValue::String(property_name.clone()), property_value);
            }
        }
    }
    for (key, value) in extra_fields {
        frontmatter.insert(YamlValue::String((*key).to_string()), value.clone());
    }

    let rendered = render_note_from_parts(Some(&frontmatter), body).map_err(AppError::operation)?;
    let absolute_path = paths.vault_root().join(relative_path);
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    fs::write(absolute_path, rendered).map_err(AppError::operation)
}

fn write_tasks_query_fixture(paths: &VaultPaths) {
    fs::write(
        paths.vault_root().join(".vulcan/config.toml"),
        concat!(
            "[tasks]\n",
            "global_filter = \"#task\"\n",
            "global_query = \"not done\"\n",
            "remove_global_filter = true\n",
        ),
    )
    .expect("config should be written");
    fs::write(
        paths.vault_root().join("Tasks.md"),
        concat!(
            "# Sprint\n\n",
            "- [ ] Write docs #task\n",
            "- [x] Ship release #task\n",
            "- [x] Archive misc #misc\n",
            "- [ ] Plan backlog #task\n",
        ),
    )
    .expect("tasks note should be written");
    fs::write(
        paths.vault_root().join("Dashboard.md"),
        concat!(
            "```tasks\n",
            "done\n",
            "```\n\n",
            "```tasks\n",
            "path includes Tasks\n",
            "```\n",
        ),
    )
    .expect("dashboard note should be written");
}

fn write_tasknotes_views_fixture(paths: &VaultPaths) {
    fs::create_dir_all(paths.vault_root().join("TaskNotes/Views"))
        .expect("tasknotes views directory should be created");
    fs::write(
        paths
            .vault_root()
            .join("TaskNotes/Views/tasks-default.base"),
        concat!(
            "source:\n",
            "  type: tasknotes\n",
            "  config:\n",
            "    type: tasknotesTaskList\n",
            "    includeArchived: false\n",
            "views:\n",
            "  - type: tasknotesTaskList\n",
            "    name: Tasks\n",
            "    order:\n",
            "      - file.name\n",
            "      - priorityWeight\n",
            "      - efficiencyRatio\n",
            "      - urgencyScore\n",
            "    sort:\n",
            "      - column: file.name\n",
            "        direction: ASC\n",
        ),
    )
    .expect("tasks default base should be written");
    fs::write(
        paths
            .vault_root()
            .join("TaskNotes/Views/kanban-default.base"),
        concat!(
            "source:\n",
            "  type: tasknotes\n",
            "  config:\n",
            "    type: tasknotesKanban\n",
            "    includeArchived: false\n",
            "views:\n",
            "  - type: tasknotesKanban\n",
            "    name: Kanban Board\n",
            "    order:\n",
            "      - file.name\n",
            "      - status\n",
            "    groupBy:\n",
            "      property: status\n",
            "      direction: ASC\n",
        ),
    )
    .expect("kanban default base should be written");
}

fn write_tasknotes_saved_view_config(paths: &VaultPaths) {
    fs::write(
        paths.vault_root().join(".vulcan/config.toml"),
        r#"[tasknotes]

[[tasknotes.saved_views]]
id = "blocked"
name = "Blocked Tasks"

[tasknotes.saved_views.query]
type = "group"
id = "root"
conjunction = "and"
sortKey = "due"
sortDirection = "asc"

[[tasknotes.saved_views.query.children]]
type = "condition"
id = "status-filter"
property = "status"
operator = "is"
value = "in-progress"
"#,
    )
    .expect("config should be written");
}

fn write_tasks_dependency_fixture(paths: &VaultPaths) {
    fs::write(
        paths.vault_root().join(".vulcan/config.toml"),
        "[tasks]\nglobal_filter = \"#task\"\nremove_global_filter = true\n",
    )
    .expect("config should be written");
    fs::write(
        paths.vault_root().join("Tasks.md"),
        concat!(
            "- [ ] Write docs #task 🆔 WRITE-1\n",
            "- [ ] Ship release #task 🆔 SHIP-1\n",
            "- [ ] Publish docs #task ⛔ SHIP-1\n",
            "- [ ] Prep launch #task ⛔ MISSING-1\n",
            "- [ ] Archive misc #misc ⛔ WRITE-1\n",
        ),
    )
    .expect("dependency note should be written");
}

fn write_tasks_recurrence_fixture(paths: &VaultPaths) {
    fs::write(
        paths.vault_root().join(".vulcan/config.toml"),
        "[tasks]\nglobal_filter = \"#task\"\nremove_global_filter = true\n",
    )
    .expect("config should be written");
    fs::write(
            paths.vault_root().join("Recurring.md"),
            concat!(
                "- [ ] Review sprint #task ⏳ 2026-03-30 🔁 every 2 weeks\n",
                "- [ ] Close books #task ⏳ 2026-02-15 [repeat:: every month on the 15th]\n",
                "- [ ] Publish notes #task ⏳ 2026-03-26 [repeat:: RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TH]\n",
                "- [ ] Ignore misc #misc ⏳ 2026-03-30 🔁 every 2 weeks\n",
            ),
        )
        .expect("recurring note should be written");
}

fn first_completed_status_for_test(config: &VaultConfig) -> String {
    config
        .tasknotes
        .statuses
        .iter()
        .find(|status| status.is_completed)
        .map_or_else(|| "done".to_string(), |status| status.value.clone())
}

use crate::AppError;
use vulcan_core::VaultConfig;
