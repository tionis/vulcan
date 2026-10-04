use super::{
    apply_note_frontmatter_mutation, apply_task_add, apply_task_archive, apply_task_complete,
    apply_task_complete_with_guard, apply_task_convert, apply_task_create,
    apply_task_create_with_guard, apply_task_pomodoro_start, apply_task_pomodoro_stop,
    apply_task_reschedule, apply_task_reschedule_with_guard, apply_task_set,
    apply_task_track_start, apply_task_track_stop, build_task_due_report,
    build_task_pomodoro_status_report, build_task_reminders_report, build_task_show_report,
    build_task_track_log_report, build_task_track_status_report, build_task_track_summary_report,
    build_tasks_blocked_report, build_tasks_eval_report, build_tasks_graph_report,
    build_tasks_list_report, build_tasks_next_report, build_tasks_view_list_report,
    build_tasks_view_report, current_utc_date_string, move_ordinary_tasknote_if_unchanged,
    process_due_tasknote_auto_archives, write_ordinary_task_conversion, TaskAddRequest,
    TaskArchiveRequest, TaskCompleteRequest, TaskConvertRequest, TaskCreateRequest,
    TaskEvalRequest, TaskListRequest, TaskPomodoroStartRequest, TaskPomodoroStopRequest,
    TaskRescheduleRequest, TaskSetRequest, TaskTrackStartRequest, TaskTrackStopRequest,
    TaskTrackSummaryPeriod,
};
use crate::templates::render_note_from_parts;
use serde::Serialize;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use std::fs;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tempfile::tempdir;
use vulcan_core::mdbase::list_mdbase_write_outbox;
use vulcan_core::{
    initialize_vulcan_dir, load_vault_config, resolve_permission_profile, scan_vault_with_progress,
    ProfilePermissionGuard, RefactorChange, ScanMode, VaultPaths,
};

#[test]
fn guarded_tasknote_reports_scope_records_and_totals_without_write_access() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    let profile = "[permissions.profiles.scoped]\nread = { allow = [\"tag:visible\"], deny = [\"tag:secret\"] }\nwrite = \"none\"\n";
    fs::write(paths.config_file(), profile).unwrap();
    for (path, tags) in [
        ("Hidden.md", "[task]"),
        ("Denied.md", "[task, visible, secret]"),
        ("Visible.md", "[task, visible]"),
    ] {
        fs::write(temp.path().join(path), format!(
            "---\ntags: {tags}\ntitle: {path}\nstatus: open\ndue: 2020-01-01\nreminders:\n  - id: reminder\n    type: absolute\n    absoluteTime: '2020-01-01T08:00:00Z'\ntimeEntries:\n  - startTime: '2020-01-01T09:00:00Z'\n---\n"
        )).unwrap();
    }
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    assert_eq!(build_task_due_report(&paths, "7d").unwrap().tasks.len(), 3);
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let due = super::build_task_due_report_with_guard(&paths, "7d", Some(&guard)).unwrap();
    assert_eq!(due.tasks.len(), 1);
    assert_eq!(due.tasks[0].path, "Visible.md");
    let reminders =
        super::build_task_reminders_report_with_guard(&paths, "7d", Some(&guard)).unwrap();
    assert_eq!(reminders.reminders.len(), 1);
    assert_eq!(reminders.reminders[0].path, "Visible.md");
    let log = super::build_task_track_log_report_with_guard(&paths, "Visible", &guard).unwrap();
    assert_eq!(log.path, "Visible.md");
    assert_eq!(log.entries.len(), 1);
    for path in ["Hidden.md", "Denied.md"] {
        assert!(super::build_task_track_log_report_with_guard(&paths, path, &guard).is_err());
    }
    let status = super::build_task_track_status_report_with_guard(&paths, Some(&guard)).unwrap();
    assert_eq!(status.total_active_sessions, 1);
    assert_eq!(status.active_sessions[0].path, "Visible.md");
    assert_eq!(
        status.total_elapsed_minutes,
        status.active_sessions[0].session.duration_minutes
    );
    let summary = super::build_task_track_summary_report_with_guard(
        &paths,
        TaskTrackSummaryPeriod::All,
        Some(&guard),
    )
    .unwrap();
    assert_eq!(summary.tasks_with_time, 1);
    assert_eq!(summary.active_tasks, 1);
    assert_eq!(summary.top_tasks[0].path, "Visible.md");
    assert_eq!(summary.total_minutes, summary.top_tasks[0].minutes);
    // Source changes must not reuse cached time entries or cached tag grants.
    fs::write(
        temp.path().join("Visible.md"),
        "---\ntags: [task, visible]\ntitle: Fresh source\nstatus: open\n---\n",
    )
    .unwrap();
    let log = super::build_task_track_log_report_with_guard(&paths, "Visible", &guard).unwrap();
    assert_eq!(log.title, "Fresh source");
    assert!(log.entries.is_empty());
    fs::write(
        temp.path().join("Visible.md"),
        "---\ntags: [task]\nstatus: open\n---\n",
    )
    .unwrap();
    assert!(super::build_task_track_log_report_with_guard(&paths, "Visible", &guard).is_err());
    fs::write(
        paths.config_file(),
        profile.replace("tag:visible", "tag:revoked"),
    )
    .unwrap();
    for error in [
        super::build_task_track_log_report_with_guard(&paths, "Visible", &guard).unwrap_err(),
        super::build_task_due_report_with_guard(&paths, "7d", Some(&guard)).unwrap_err(),
        super::build_task_reminders_report_with_guard(&paths, "7d", Some(&guard)).unwrap_err(),
        super::build_task_track_status_report_with_guard(&paths, Some(&guard)).unwrap_err(),
        super::build_task_track_summary_report_with_guard(
            &paths,
            TaskTrackSummaryPeriod::All,
            Some(&guard),
        )
        .unwrap_err(),
    ] {
        assert!(error.to_string().contains("authority changed"));
    }
}

#[test]
fn guarded_task_report_rechecks_authority_after_derivation_even_on_error() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    let profile = "[permissions.profiles.scoped]\nread = \"all\"\nwrite = \"none\"\n";
    fs::write(paths.config_file(), profile).unwrap();
    fs::write(temp.path().join("Task.md"), "- [ ] Visible task\n").unwrap();
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    for fail in [false, true] {
        fs::write(paths.config_file(), profile).unwrap();
        let result = super::derive_task_report_with_guard(&paths, "", Some(&guard), |tasks| {
            assert_eq!(tasks.result_count, 1);
            fs::write(
                paths.config_file(),
                profile.replace("read = \"all\"", "read = \"none\""),
            )
            .unwrap();
            if fail {
                Err(AppError::operation("derived failure sentinel"))
            } else {
                Ok(())
            }
        });
        let error = result.unwrap_err().to_string();
        assert!(error.contains("authority changed"));
        assert!(!error.contains("derived failure sentinel"));
    }
}

#[test]
fn guarded_task_dependency_reports_hide_target_state_and_recheck_grants() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    let profile = "[permissions.profiles.scoped]\nread = { allow = [\"tag:visible\"], deny = [\"tag:secret\"] }\nwrite = \"none\"\n";
    fs::write(paths.config_file(), profile).unwrap();
    for (path, source) in [
        ("AHidden.md", "- [x] Hidden secret 🆔 BLOCK-1\n"),
        (
            "BDenied.md",
            "---\ntags: [visible, secret]\n---\n- [x] Denied secret 🆔 DENIED-1\n",
        ),
        (
            "CVisible.md",
            concat!(
                "---\ntags: [visible]\n---\n",
                "- [ ] Await hidden ⛔ BLOCK-1\n- [ ] Await absent ⛔ MISSING-1\n",
                "- [ ] Await denied ⛔ DENIED-1\n- [x] Visible done 🆔 DONE-1\n",
                "- [ ] Ready ⛔ DONE-1\n"
            ),
        ),
    ] {
        fs::write(temp.path().join(path), source).unwrap();
    }
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    assert_eq!(build_tasks_blocked_report(&paths).unwrap().tasks.len(), 1);
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let graph = super::build_tasks_graph_report_with_guard(&paths, &guard).unwrap();
    assert_eq!(graph.nodes.len(), 5);
    assert_eq!(graph.edges.len(), 4);
    assert!(graph.nodes.iter().all(|node| node.path == "CVisible.md"));
    for edge in &graph.edges {
        if edge.blocker_id == "DONE-1" {
            assert!(edge.resolved);
            assert_eq!(edge.blocker_completed, Some(true));
        } else {
            assert!(!edge.resolved);
            assert!(edge.blocker_key.is_none() && edge.blocker_path.is_none());
            assert!(edge.blocker_line.is_none() && edge.blocker_text.is_none());
            assert!(edge.blocker_completed.is_none());
        }
    }
    let blocked = super::build_tasks_blocked_report_with_guard(&paths, &guard).unwrap();
    assert_eq!(blocked.tasks.len(), 3);
    assert!(blocked.tasks.iter().all(|task| !task.blockers[0].resolved));
    fs::write(temp.path().join("AHidden.md"), "No task remains\n").unwrap();
    fs::write(temp.path().join("BDenied.md"), "No task remains\n").unwrap();
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    assert_eq!(
        graph,
        super::build_tasks_graph_report_with_guard(&paths, &guard).unwrap()
    );
    assert_eq!(
        blocked,
        super::build_tasks_blocked_report_with_guard(&paths, &guard).unwrap()
    );
    fs::write(
        paths.config_file(),
        profile.replace("tag:visible", "tag:revoked"),
    )
    .unwrap();
    for error in [
        super::build_tasks_graph_report_with_guard(&paths, &guard).unwrap_err(),
        super::build_tasks_blocked_report_with_guard(&paths, &guard).unwrap_err(),
    ] {
        assert!(error.to_string().contains("authority changed"));
    }
}

#[test]
fn guarded_task_next_filters_before_occurrence_limits() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    let profile = "[permissions.profiles.scoped]\nread = { allow = [\"tag:visible\"], deny = [\"tag:secret\"] }\nwrite = \"none\"\n";
    fs::write(paths.config_file(), profile).unwrap();
    for (path, tags) in [
        ("AHidden.md", "[]"),
        ("BDenied.md", "[visible, secret]"),
        ("CVisible.md", "[visible]"),
    ] {
        fs::write(
            temp.path().join(path),
            format!("---\ntags: {tags}\n---\n- [ ] Review ⏳ 2026-03-30 🔁 every 2 weeks\n"),
        )
        .unwrap();
    }
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    assert_eq!(
        build_tasks_next_report(&paths, 1, Some("2026-03-29"))
            .unwrap()
            .occurrences[0]
            .task["path"],
        "AHidden.md"
    );
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let report =
        super::build_tasks_next_report_with_guard(&paths, 1, Some("2026-03-29"), &guard).unwrap();
    assert_eq!(report.result_count, 1);
    assert_eq!(report.occurrences[0].task["path"], "CVisible.md");
    assert_eq!(report.occurrences[0].date, "2026-03-30");
    fs::write(
        paths.config_file(),
        profile.replace("tag:visible", "tag:revoked"),
    )
    .unwrap();
    let error = super::build_tasks_next_report_with_guard(&paths, 1, None, &guard).unwrap_err();
    assert!(error.to_string().contains("authority changed"));
}

#[test]
fn guarded_tasks_eval_scopes_sources_and_results_before_limits() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    let profile = "[permissions.profiles.scoped]\nread = { allow = [\"tag:visible\"], deny = [\"tag:secret\"] }\nwrite = \"none\"\n";
    fs::write(paths.config_file(), profile).unwrap();
    for (path, source) in [
        ("AHidden.md", "- [ ] Hidden sentinel\n"),
        ("ZVisible.md", "---\ntags: [visible]\n---\n- [ ] Visible task\n"),
        ("Public/Dashboard.md", "---\ntags: [visible]\naliases: [Review]\n---\n```tasks\nnot done\ngroup by path\nlimit 1\n```\n\n```tasks\nunsupported sentinel\n```\n"),
        ("Private/Dashboard.md", "---\ntags: [visible, secret]\naliases: [Review]\n---\n```tasks\nhidden source sentinel\n```\n"),
    ] {
        let target = temp.path().join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, source).unwrap();
    }
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let unrestricted = build_tasks_eval_report(
        &paths,
        &TaskEvalRequest {
            file: "Public/Dashboard.md".into(),
            block: Some(0),
        },
    )
    .unwrap();
    assert_eq!(
        unrestricted.blocks[0].result.as_ref().unwrap().tasks[0]["path"],
        "AHidden.md"
    );
    for file in ["Dashboard", "Review", "Public/Dashboard.md"] {
        let report = super::build_tasks_eval_report_with_guard(
            &paths,
            &TaskEvalRequest {
                file: file.into(),
                block: None,
            },
            &guard,
        )
        .unwrap();
        assert_eq!(report.file, "Public/Dashboard.md");
        assert_eq!(report.blocks.len(), 2);
        let result = report.blocks[0].result.as_ref().unwrap();
        assert_eq!(result.result_count, 1);
        assert_eq!(result.tasks[0]["path"], "ZVisible.md");
        assert_eq!(result.groups.len(), 1);
        assert!(report.blocks[1].error.is_some());
        assert!(report.blocks[1].result.is_none());
    }
    let request = TaskEvalRequest {
        file: "Private/Dashboard.md".into(),
        block: Some(42),
    };
    let error = super::build_tasks_eval_report_with_guard(&paths, &request, &guard).unwrap_err();
    assert!(error.to_string().contains("note not found"));
    fs::write(
        paths.config_file(),
        profile.replace("tag:visible", "tag:revoked"),
    )
    .unwrap();
    let error = super::build_tasks_eval_report_with_guard(&paths, &request, &guard).unwrap_err();
    assert!(error.to_string().contains("authority changed"));
}

#[test]
fn task_read_decisions_are_shared_only_within_one_operation() {
    use vulcan_core::permissions::{PermissionError, PermissionGrant};
    use vulcan_core::PermissionGuard;
    struct Guard {
        grant: PermissionGrant,
        calls: std::cell::Cell<usize>,
    }
    impl PermissionGuard for Guard {
        fn profile_name(&self) -> &'static str {
            "test"
        }
        fn grant(&self) -> &PermissionGrant {
            &self.grant
        }
        fn has_policy_hook(&self) -> bool {
            true
        }
        fn check_policy_decision(
            &self,
            action: &'static str,
            resource: Option<&str>,
        ) -> Result<(), PermissionError> {
            self.calls.set(self.calls.get() + 1);
            match resource {
                Some("deny") => Err(PermissionError::PathDenied {
                    profile: "test".into(),
                    action,
                    path: "deny".into(),
                }),
                Some("fail") => Err(PermissionError::PolicyHookFailed {
                    profile: "test".into(),
                    action,
                    resource: resource.map(str::to_string),
                    reason: "broken".into(),
                }),
                _ => Ok(()),
            }
        }
    }
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    let guard = Guard {
        grant: resolve_permission_profile(&paths, None).unwrap().grant,
        calls: std::cell::Cell::new(0),
    };
    let decisions = super::TaskReadDecisions::new(&guard);
    for resource in ["allow", "deny", "fail"] {
        let first = decisions.check_policy_decision("read", Some(resource));
        assert_eq!(
            first,
            decisions.check_policy_decision("read", Some(resource))
        );
    }
    assert_eq!(guard.calls.get(), 3);
    decisions
        .check_policy_decision("write", Some("allow"))
        .unwrap();
    decisions
        .check_policy_decision("write", Some("allow"))
        .unwrap();
    assert_eq!(guard.calls.get(), 5);
    super::TaskReadDecisions::new(&guard)
        .check_policy_decision("read", Some("allow"))
        .unwrap();
    assert_eq!(guard.calls.get(), 6);
}

#[test]
fn guarded_task_show_reads_uncached_paths_with_static_path_authority() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\nwrite = \"none\"\n").unwrap();
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "Public/Task.md",
        "Uncached",
        "open",
        &[],
        "Body\n",
    )
    .unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let report = super::build_task_show_report_with_guard(&paths, "Public/Task", &guard).unwrap();
    assert_eq!(report.title, "Uncached");
    let error =
        super::build_task_show_report_with_guard(&paths, "Hidden/Task", &guard).unwrap_err();
    assert!(error.to_string().contains("permission denied"));
}

#[test]
fn guarded_task_show_uses_read_only_scope_and_rejects_stale_authority() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    let profile = "[permissions.profiles.scoped]\nread = { allow = [\"tag:visible\"], deny = [\"tag:secret\"] }\nwrite = \"none\"\n";
    fs::write(paths.config_file(), profile).unwrap();
    let config = load_vault_config(&paths).config;
    for (folder, tags) in [
        ("Visible", vec!["task", "visible"]),
        ("Hidden", vec!["task"]),
        ("Denied", vec!["task", "visible", "secret"]),
    ] {
        seed_tasknote(
            &paths,
            &config,
            &format!("{folder}/Task.md"),
            folder,
            "open",
            &[
                ("tags", serde_yaml::to_value(tags).unwrap()),
                ("aliases", serde_yaml::to_value(["Shared"]).unwrap()),
            ],
            &format!("{folder} sentinel\n"),
        )
        .unwrap();
    }
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    for identifier in ["Task", "Shared", "Visible/Task.md"] {
        let report = super::build_task_show_report_with_guard(&paths, identifier, &guard).unwrap();
        assert_eq!(report.path, "Visible/Task.md");
        assert_eq!(report.body, "Visible sentinel\n");
    }
    for identifier in ["Hidden/Task.md", "Denied/Task.md"] {
        assert!(super::build_task_show_report_with_guard(&paths, identifier, &guard).is_err());
    }
    // Show must use current source properties, not stale cached content.
    let visible = temp.path().join("Visible/Task.md");
    let source = fs::read_to_string(&visible).unwrap();
    fs::write(&visible, source.replace("Visible", "Updated")).unwrap();
    let report = super::build_task_show_report_with_guard(&paths, "Task", &guard).unwrap();
    assert_eq!(report.title, "Updated");
    assert_eq!(report.body, "Updated sentinel\n");
    fs::write(&visible, source.replace("visible", "secret")).unwrap();
    let error = super::build_task_show_report_with_guard(&paths, "Task", &guard).unwrap_err();
    assert!(error.to_string().contains("no longer readable"));
    fs::write(&visible, &source).unwrap();
    fs::write(
        paths.config_file(),
        profile.replace("tag:visible", "tag:revoked"),
    )
    .unwrap();
    let error = super::build_task_show_report_with_guard(&paths, "Task", &guard).unwrap_err();
    assert!(error.to_string().contains("authority changed"));
}

#[test]
fn guarded_daily_sessions_do_not_expose_or_complete_hidden_tasks() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(
        paths.config_file(),
        "tasknotes.pomodoro.storage_location = \"daily-note\"\n",
    )
    .unwrap();
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "Hidden/Task.md",
        "Hidden sentinel",
        "open",
        &[],
        "",
    )
    .unwrap();
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let started = apply_task_pomodoro_start(
        &paths,
        &TaskPomodoroStartRequest {
            task: "Hidden/Task.md".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let daily = paths.vault_root().join(&started.storage_note_path);
    let content = fs::read_to_string(&daily)
        .unwrap()
        .replace(&started.session.start_time, "2026-04-01T08:00:00Z");
    fs::write(&daily, &content).unwrap();
    fs::write(paths.config_file(), format!("tasknotes.pomodoro.storage_location = \"daily-note\"\n[permissions.profiles.scoped]\nread = {{ allow = [\"note:{}\", \"note:mdbase.yaml\"] }}\nwrite = {{ allow = [\"note:{}\"] }}\n", started.storage_note_path, started.storage_note_path)).unwrap();
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let report = super::build_task_pomodoro_status_report_with_guard(&paths, Some(&guard)).unwrap();
    assert!(report.active.is_none());
    assert_eq!(report.completed_work_sessions, 0);
    assert!(report.changed_paths.is_empty());
    assert!(!serde_json::to_string(&report).unwrap().contains("Hidden"));
    for task in [None, Some("Hidden/Task.md".to_string())] {
        assert!(super::apply_task_pomodoro_stop_with_guard(
            &paths,
            &TaskPomodoroStopRequest {
                task,
                dry_run: false
            },
            Some(&guard)
        )
        .is_err());
        assert_eq!(fs::read_to_string(&daily).unwrap(), content);
    }
}

#[test]
fn task_editor_preflight_requires_execution_and_path_grants() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), "[permissions.profiles.noexec]\nread = \"all\"\nwrite = \"all\"\nexecute = \"deny\"\n[permissions.profiles.readonly]\nread = \"all\"\nwrite = { allow = [] }\nexecute = \"allow\"\n").unwrap();
    let config = load_vault_config(&paths).config;
    seed_tasknote(&paths, &config, "Task.md", "Task", "open", &[], "").unwrap();
    for profile in ["noexec", "readonly"] {
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some(profile)).unwrap(),
        );
        assert!(super::prepare_task_editor_path(&paths, "Task.md", &guard).is_err());
    }
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("unrestricted")).unwrap(),
    );
    assert_eq!(
        super::prepare_task_editor_path(&paths, "Task.md", &guard).unwrap(),
        "Task.md"
    );
}

#[test]
#[cfg(unix)]
fn guarded_task_source_reads_reject_file_and_directory_symlinks() {
    use std::os::unix::fs::symlink;
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\", \"note:mdbase.yaml\"] }\nwrite = { allow = [\"folder:Public/**\"] }\n").unwrap();
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "Hidden/Task.md",
        "Hidden sentinel",
        "open",
        &[],
        "",
    )
    .unwrap();
    fs::create_dir_all(temp.path().join("Public")).unwrap();
    symlink(
        temp.path().join("Hidden/Task.md"),
        temp.path().join("Public/Linked.md"),
    )
    .unwrap();
    symlink(
        temp.path().join("Hidden"),
        temp.path().join("Public/Folder"),
    )
    .unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let before = fs::read(temp.path().join("Hidden/Task.md")).unwrap();
    for task in ["Public/Linked.md", "Public/Folder/Task.md"] {
        assert!(super::read_task_source(&paths, task).is_err());
        for dry_run in [true, false] {
            let error = super::apply_task_set_with_guard(
                &paths,
                &TaskSetRequest {
                    task: task.into(),
                    property: "status".into(),
                    value: "done".into(),
                    dry_run,
                },
                Some(&guard),
            )
            .unwrap_err();
            assert!(!error.to_string().contains("Hidden sentinel"));
            assert_eq!(
                fs::read(temp.path().join("Hidden/Task.md")).unwrap(),
                before
            );
        }
    }
    assert!(super::read_task_source(&paths, "Public").is_err());
    assert!(super::read_task_source(&paths, "Hidden/Task.md")
        .unwrap()
        .contains("Hidden sentinel"));
}

#[test]
fn periodic_missing_template_fallback_requires_complete_read_authority() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), "periodic.daily.template = \"Day\"\n[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\nwrite = \"all\"\n").unwrap();
    for profile in ["unrestricted", "scoped"] {
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some(profile)).unwrap(),
        );
        let mut warnings = Vec::new();
        let result = crate::notes::render_periodic_note_contents_with_guard(
            &paths,
            "daily",
            "Public/Daily.md",
            &mut warnings,
            Some(&guard),
            true,
        );
        if profile == "unrestricted" {
            assert_eq!(result.unwrap(), "");
            assert_eq!(warnings.len(), 1);
        } else {
            assert!(result.is_err());
            assert!(warnings.is_empty());
        }
    }
    // A readable but corrupt source is not absence and must not silently fall back.
    fs::create_dir_all(paths.vulcan_dir().join("templates")).unwrap();
    fs::write(paths.vulcan_dir().join("templates/Day.md"), [0xff]).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("unrestricted")).unwrap(),
    );
    let mut warnings = Vec::new();
    assert!(crate::notes::render_periodic_note_contents_with_guard(
        &paths,
        "daily",
        "Public/Daily.md",
        &mut warnings,
        Some(&guard),
        true,
    )
    .is_err());
    assert!(warnings.is_empty());
}

#[test]
fn guarded_daily_pomodoro_templates_preserve_authority_and_preview_safety() {
    for readable in [false, true] {
        for dry_run in [true, false] {
            let temp = tempdir().unwrap();
            let paths = VaultPaths::new(temp.path());
            initialize_vulcan_dir(&paths).unwrap();
            fs::write(paths.config_file(), "tasknotes.pomodoro.storage_location = \"daily-note\"\nperiodic.daily.template = \"Day\"\n").unwrap();
            let initial_config = load_vault_config(&paths).config;
            let daily = super::task_pomodoro_storage_target_path(
                &initial_config,
                "Public/Task.md",
                super::current_utc_timestamp_ms(),
            )
            .unwrap();
            let template_grant = if readable {
                ", \"note:.vulcan/templates/Day.md\""
            } else {
                ""
            };
            fs::write(paths.config_file(), format!("tasknotes.pomodoro.storage_location = \"daily-note\"\nperiodic.daily.template = \"Day\"\n[permissions.profiles.scoped]\nread = {{ allow = [\"folder:Public/**\", \"note:mdbase.yaml\", \"note:{daily}\"{template_grant}] }}\nwrite = {{ allow = [\"folder:Public/**\", \"note:{daily}\"] }}\n")).unwrap();
            fs::create_dir_all(paths.vulcan_dir().join("templates")).unwrap();
            fs::write(
                paths.vulcan_dir().join("templates/Day.md"),
                "Daily template content\n",
            )
            .unwrap();
            let config = load_vault_config(&paths).config;
            seed_tasknote(&paths, &config, "Public/Task.md", "Task", "open", &[], "").unwrap();
            scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
            let guard = ProfilePermissionGuard::new(
                &paths,
                resolve_permission_profile(&paths, Some("scoped")).unwrap(),
            );
            let result = super::apply_task_pomodoro_start_with_guard(
                &paths,
                &TaskPomodoroStartRequest {
                    task: "Public/Task.md".into(),
                    dry_run,
                },
                Some(&guard),
            );
            assert_eq!(result.is_ok(), readable, "{result:?}");
            assert_eq!(temp.path().join(&daily).exists(), readable && !dry_run);
            if readable && !dry_run {
                assert!(fs::read_to_string(temp.path().join(&daily))
                    .unwrap()
                    .contains("Daily template content"));
            }
        }
    }
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(
        paths.config_file(),
        "tasknotes.pomodoro.storage_location = \"daily-note\"\nperiodic.daily.template = \"Day\"\n",
    )
    .unwrap();
    fs::create_dir_all(paths.vulcan_dir().join("templates")).unwrap();
    fs::write(
        paths.vulcan_dir().join("templates/Day.md"),
        "<% tp.file.create_new('side', 'Side') %>Daily\n",
    )
    .unwrap();
    let config = load_vault_config(&paths).config;
    seed_tasknote(&paths, &config, "Public/Task.md", "Task", "open", &[], "").unwrap();
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let daily = super::task_pomodoro_storage_target_path(
        &config,
        "Public/Task.md",
        super::current_utc_timestamp_ms(),
    )
    .unwrap();
    let _ = apply_task_pomodoro_start(
        &paths,
        &TaskPomodoroStartRequest {
            task: "Public/Task.md".into(),
            dry_run: true,
        },
    );
    assert!(!temp.path().join("Side.md").exists());
    assert!(!temp.path().join(daily).exists());
}

#[test]
fn guarded_pomodoro_status_completes_only_visible_due_sessions() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\", \"note:mdbase.yaml\"] }\nwrite = { allow = [\"folder:Public/**\"] }\n").unwrap();
    let config = load_vault_config(&paths).config;
    for folder in ["Public", "Hidden"] {
        let task = format!("{folder}/Task.md");
        let sessions = serde_yaml::to_value(serde_json::json!([{
            "id": folder, "startTime": "2026-04-01T08:00:00Z",
            "plannedDuration": 1, "type": "work", "taskPath": task,
            "completed": false,
            "activePeriods": [{"startTime": "2026-04-01T08:00:00Z"}]
        }]))
        .unwrap();
        seed_tasknote(
            &paths,
            &config,
            &task,
            "Task",
            "open",
            &[(config.tasknotes.field_mapping.pomodoros.as_str(), sessions)],
            "",
        )
        .unwrap();
    }
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let before = fs::read(temp.path().join("Hidden/Task.md")).unwrap();
    let report = super::build_task_pomodoro_status_report_with_guard(&paths, Some(&guard)).unwrap();
    assert_eq!(report.completed_work_sessions, 1);
    assert_eq!(report.changed_paths, vec!["Public/Task.md"]);
    assert!(report.active.is_none());
    assert_eq!(
        fs::read(temp.path().join("Hidden/Task.md")).unwrap(),
        before
    );
}

#[test]
fn guarded_pomodoro_start_denies_daily_storage_outside_scope() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), "tasknotes.pomodoro.storage_location = \"daily-note\"\n[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\", \"note:mdbase.yaml\"] }\nwrite = { allow = [\"folder:Public/**\"] }\n").unwrap();
    let config = load_vault_config(&paths).config;
    seed_tasknote(&paths, &config, "Public/Task.md", "Task", "open", &[], "").unwrap();
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let daily = super::task_pomodoro_storage_target_path(
        &config,
        "Public/Task.md",
        super::current_utc_timestamp_ms(),
    )
    .unwrap();
    let before = fs::read(temp.path().join("Public/Task.md")).unwrap();
    for dry_run in [true, false] {
        assert!(super::apply_task_pomodoro_start_with_guard(
            &paths,
            &TaskPomodoroStartRequest {
                task: "Public/Task.md".into(),
                dry_run
            },
            Some(&guard)
        )
        .is_err());
        assert!(!temp.path().join(&daily).exists());
        assert_eq!(
            fs::read(temp.path().join("Public/Task.md")).unwrap(),
            before
        );
    }
}

#[test]
fn guarded_task_add_uses_readable_template_without_hidden_shadowing() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), "tasknotes.tasks_folder = \"Public/Tasks\"\ntemplates.templater_folder = \"Public/Templates\"\n[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\", \"note:mdbase.yaml\"] }\nwrite = { allow = [\"folder:Public/Tasks/**\"] }\n").unwrap();
    fs::create_dir_all(paths.vulcan_dir().join("templates")).unwrap();
    fs::create_dir_all(temp.path().join("Public/Templates")).unwrap();
    fs::write(paths.vulcan_dir().join("templates/Task.md"), [0xff, 0xfe]).unwrap();
    fs::write(
        temp.path().join("Public/Templates/Task.md"),
        "Readable template\n",
    )
    .unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let mut request = TaskAddRequest {
        text: "Scoped task".into(),
        no_nlp: true,
        status: None,
        priority: None,
        due: None,
        scheduled: None,
        contexts: vec![],
        projects: vec![],
        tags: vec![],
        template: Some("Task".into()),
        dry_run: true,
    };
    let report = super::apply_task_add_with_guard(&paths, &request, Some(&guard)).unwrap();
    assert!(report.body.contains("Readable template"));
    assert!(!temp.path().join(&report.path).exists());
    request.dry_run = false;
    let report = super::apply_task_add_with_guard(&paths, &request, Some(&guard)).unwrap();
    assert!(fs::read_to_string(temp.path().join(report.path))
        .unwrap()
        .contains("Readable template"));
}

#[test]
fn task_add_template_dry_run_cannot_create_side_notes() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::create_dir_all(paths.vulcan_dir().join("templates")).unwrap();
    fs::write(
        paths.vulcan_dir().join("templates/Task.md"),
        "<% tp.file.create_new('Side body', 'Side') %>Main body",
    )
    .unwrap();
    let request = TaskAddRequest {
        text: "Preview task".into(),
        no_nlp: true,
        status: None,
        priority: None,
        due: None,
        scheduled: None,
        contexts: vec![],
        projects: vec![],
        tags: vec![],
        template: Some("Task".into()),
        dry_run: true,
    };
    let _ = apply_task_add(&paths, &request);
    assert!(!temp.path().join("Side.md").exists());
    assert!(!temp.path().join("Tasks/Preview task.md").exists());
}

#[test]
fn guarded_conversion_checks_source_and_destination_before_writing() {
    for line in [None, Some(1)] {
        for dry_run in [true, false] {
            for allow_destination in [false, true] {
                let temp = tempdir().unwrap();
                let paths = VaultPaths::new(temp.path());
                initialize_vulcan_dir(&paths).unwrap();
                let destination = if allow_destination {
                    "Public/Tasks"
                } else {
                    "Hidden/Tasks"
                };
                fs::write(paths.config_file(), format!(
                    "tasknotes.tasks_folder = \"{destination}\"\n[permissions.profiles.scoped]\nread = {{ allow = [\"folder:Public/**\", \"note:mdbase.yaml\"] }}\nwrite = {{ allow = [\"folder:Public/**\"] }}\n"
                )).unwrap();
                for folder in ["Public", "Hidden"] {
                    fs::create_dir_all(temp.path().join(folder)).unwrap();
                    fs::write(
                        temp.path().join(folder).join("Inbox.md"),
                        "- [ ] Convert me\n",
                    )
                    .unwrap();
                }
                scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
                let guard = ProfilePermissionGuard::new(
                    &paths,
                    resolve_permission_profile(&paths, Some("scoped")).unwrap(),
                );
                let hidden_before = fs::read(temp.path().join("Hidden/Inbox.md")).unwrap();
                assert!(super::apply_task_convert_with_guard(
                    &paths,
                    &TaskConvertRequest {
                        file: "Hidden/Inbox.md".into(),
                        line,
                        dry_run,
                    },
                    Some(&guard)
                )
                .is_err());
                let result = super::apply_task_convert_with_guard(
                    &paths,
                    &TaskConvertRequest {
                        file: "Public/Inbox.md".into(),
                        line,
                        dry_run,
                    },
                    Some(&guard),
                );
                let denied = line.is_some() && !allow_destination;
                assert_eq!(result.is_err(), denied, "{result:?}");
                assert_eq!(
                    fs::read(temp.path().join("Hidden/Inbox.md")).unwrap(),
                    hidden_before
                );
                assert!(!temp.path().join("Hidden/Tasks").exists());
                if denied || dry_run {
                    assert_eq!(
                        fs::read_to_string(temp.path().join("Public/Inbox.md")).unwrap(),
                        "- [ ] Convert me\n"
                    );
                }
            }
        }
    }
}

#[test]
fn guarded_tracking_selects_only_visible_active_sessions() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\", \"note:mdbase.yaml\"] }\nwrite = { allow = [\"folder:Public/**\"] }\n").unwrap();
    let config = load_vault_config(&paths).config;
    for folder in ["Public", "Hidden"] {
        let task = format!("{folder}/Task.md");
        seed_tasknote(&paths, &config, &task, "Task", "open", &[], "").unwrap();
        apply_task_track_start(
            &paths,
            &TaskTrackStartRequest {
                task,
                description: None,
                dry_run: false,
            },
        )
        .unwrap();
    }
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let before = fs::read(temp.path().join("Hidden/Task.md")).unwrap();
    let stopped = super::apply_task_track_stop_with_guard(
        &paths,
        &TaskTrackStopRequest {
            task: None,
            dry_run: false,
        },
        Some(&guard),
    )
    .unwrap();
    assert_eq!(stopped.path, "Public/Task.md");
    assert_eq!(
        fs::read(temp.path().join("Hidden/Task.md")).unwrap(),
        before
    );
    for dry_run in [true, false] {
        assert!(super::apply_task_track_start_with_guard(
            &paths,
            &TaskTrackStartRequest {
                task: "Hidden/Task.md".into(),
                description: None,
                dry_run,
            },
            Some(&guard)
        )
        .is_err());
    }
    assert_eq!(
        fs::read(temp.path().join("Hidden/Task.md")).unwrap(),
        before
    );
}

#[test]
fn read_only_auto_archive_leaves_due_completed_tasks_unchanged() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), concat!(
        "tasknotes.archive_folder = \"Public/Archive\"\n",
        "[[tasknotes.statuses]]\nid = \"done\"\nvalue = \"done\"\nlabel = \"Done\"\ncolor = \"#16a34a\"\nisCompleted = true\norder = 1\nautoArchive = true\nautoArchiveDelay = 0\n",
        "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\nwrite = \"none\"\n",
    )).unwrap();
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "Public/Done.md",
        "Done",
        "done",
        &[(
            config.tasknotes.field_mapping.completed_date.as_str(),
            YamlValue::String("2026-04-01T09:00:00Z".into()),
        )],
        "Keep this body.\n",
    )
    .unwrap();
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let before = fs::read(temp.path().join("Public/Done.md")).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    // Neither missing nor malformed unreadable MDB controls should be consulted
    // for an automatic mutation that this caller has no authority to perform.
    for control in [None, Some("types: [not valid\n")] {
        if let Some(control) = control {
            fs::write(temp.path().join("mdbase.yaml"), control).unwrap();
        }
        let changed =
            super::process_due_tasknote_auto_archives_with_guard(&paths, None, Some(&guard))
                .unwrap();
        assert!(changed.is_empty());
        assert_eq!(
            fs::read(temp.path().join("Public/Done.md")).unwrap(),
            before
        );
        assert!(!temp.path().join("Public/Archive").exists());
    }
}

#[test]
fn guarded_auto_archive_preserves_hidden_completed_tasks() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), concat!(
        "tasknotes.archive_folder = \"Public/Archive\"\n",
        "[[tasknotes.statuses]]\nid = \"done\"\nvalue = \"done\"\nlabel = \"Done\"\ncolor = \"#16a34a\"\nisCompleted = true\norder = 1\nautoArchive = true\nautoArchiveDelay = 0\n",
        "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\", \"note:mdbase.yaml\"] }\nwrite = { allow = [\"folder:Public/**\"] }\n",
    )).unwrap();
    let config = load_vault_config(&paths).config;
    for folder in ["Public", "Hidden"] {
        seed_tasknote(
            &paths,
            &config,
            &format!("{folder}/Done.md"),
            "Done",
            "done",
            &[(
                config.tasknotes.field_mapping.completed_date.as_str(),
                YamlValue::String("2026-04-01T09:00:00Z".into()),
            )],
            "",
        )
        .unwrap();
    }
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let before = fs::read(temp.path().join("Hidden/Done.md")).unwrap();
    let changed =
        super::process_due_tasknote_auto_archives_with_guard(&paths, None, Some(&guard)).unwrap();
    assert_eq!(changed, vec!["Public/Archive/Done.md", "Public/Done.md"]);
    assert_eq!(
        fs::read(temp.path().join("Hidden/Done.md")).unwrap(),
        before
    );
}

#[test]
fn guarded_inline_task_updates_ignore_hidden_text_matches() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\", \"note:mdbase.yaml\"] }\nwrite = { allow = [\"folder:Public/**\"] }\n").unwrap();
    for folder in ["Public", "Hidden"] {
        fs::create_dir_all(temp.path().join(folder)).unwrap();
        fs::write(
            temp.path().join(folder).join("Inbox.md"),
            "- [ ] Shared task\n",
        )
        .unwrap();
    }
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    for task in ["Shared task", "Inbox.md:1"] {
        let report = apply_task_complete_with_guard(
            &paths,
            &TaskCompleteRequest {
                task: task.into(),
                date: Some("2026-10-04".into()),
                dry_run: true,
            },
            Some(&guard),
        )
        .unwrap();
        assert_eq!(report.path, "Public/Inbox.md");
    }
    for task in ["Hidden/Inbox.md:1", "Hidden/Missing.md:1"] {
        assert!(apply_task_complete_with_guard(
            &paths,
            &TaskCompleteRequest {
                task: task.into(),
                date: None,
                dry_run: false,
            },
            Some(&guard)
        )
        .is_err());
    }
    assert_eq!(
        fs::read_to_string(temp.path().join("Hidden/Inbox.md")).unwrap(),
        "- [ ] Shared task\n"
    );
}

#[test]
fn guarded_task_loader_resolves_only_visible_tasks_and_denies_disk_fallback() {
    let temp = tempdir().unwrap();
    let paths = VaultPaths::new(temp.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\", \"note:mdbase.yaml\"] }\nwrite = { allow = [\"folder:Public/**\"] }\n").unwrap();
    let config = load_vault_config(&paths).config;
    for folder in ["Public", "Hidden"] {
        seed_tasknote(
            &paths,
            &config,
            &format!("{folder}/Task.md"),
            "Task",
            "open",
            &[],
            "",
        )
        .unwrap();
    }
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    let loaded = super::load_tasknote_note_with_guard(&paths, "Task", Some(&guard)).unwrap();
    assert_eq!(loaded.path, "Public/Task.md");
    for path in ["Hidden/Task.md", "Hidden/Missing.md"] {
        let error = super::load_tasknote_note_with_guard(&paths, path, Some(&guard)).unwrap_err();
        assert!(error.to_string().contains("denied"), "{error}");
    }
    for dry_run in [true, false] {
        let before = fs::read(temp.path().join("Hidden/Task.md")).unwrap();
        let result = super::apply_task_archive_with_guard(
            &paths,
            &TaskArchiveRequest {
                task: "Hidden/Task.md".into(),
                dry_run,
            },
            Some(&guard),
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read(temp.path().join("Hidden/Task.md")).unwrap(),
            before
        );
    }
}

#[test]
fn direct_task_reports_refuse_pending_ordinary_write_journal() {
    #[derive(Serialize)]
    struct JournalFixture<'a> {
        version: u32,
        transaction_id: &'a str,
        changes: Vec<vulcan_core::ordinary_write::OrdinaryWriteChange>,
        digest: String,
    }

    fn assert_pending<T: std::fmt::Debug>(result: Result<T, AppError>) {
        assert_eq!(
            result.expect_err("task read must fail closed").code(),
            Some("ordinary_write_pending")
        );
    }

    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("initialize vault");
    fs::write(temp_dir.path().join("Inbox.md"), "Original\n").expect("note");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");
    let directory = paths
        .operational_state_dir()
        .expect("operational state")
        .join("ordinary-write");
    fs::create_dir_all(&directory).expect("journal directory");
    let mut journal = JournalFixture {
        version: 1,
        transaction_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        changes: vec![vulcan_core::ordinary_write::OrdinaryWriteChange {
            path: "Inbox.md".to_string(),
            before: Some("Original\n".to_string()),
            after: Some("Updated\n".to_string()),
        }],
        digest: String::new(),
    };
    journal.digest = blake3::hash(&serde_json::to_vec(&journal).expect("journal bytes"))
        .to_hex()
        .to_string();
    let journal_path = directory.join("journal.json");
    fs::write(
        &journal_path,
        serde_json::to_vec(&journal).expect("sealed journal"),
    )
    .expect("pending journal");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600))
            .expect("owner-only journal");
    }

    assert_pending(build_task_show_report(&paths, "Missing"));
    assert_pending(build_task_track_status_report(&paths));
    assert_pending(build_task_due_report(&paths, "1d"));
    assert_pending(build_task_reminders_report(&paths, "1d"));
    assert_pending(super::build_tasks_query_result(&paths, ""));
    assert_pending(build_tasks_eval_report(
        &paths,
        &TaskEvalRequest {
            file: "Missing".to_string(),
            block: None,
        },
    ));
    assert_pending(build_tasks_list_report(&paths, &TaskListRequest::default()));
    assert_pending(build_tasks_view_list_report(&paths));
    assert_pending(build_tasks_view_report(&paths, "Missing"));
    assert_pending(build_tasks_next_report(&paths, 1, None));
    assert_pending(build_tasks_blocked_report(&paths));
    assert_pending(build_tasks_graph_report(&paths));
    assert_pending(build_task_track_log_report(&paths, "Missing"));
    assert_pending(build_task_track_summary_report(
        &paths,
        TaskTrackSummaryPeriod::All,
    ));

    vulcan_core::ordinary_write::recover_ordinary_write_batch(&paths)
        .expect("recover pending batch")
        .expect("pending batch");
    assert!(build_tasks_list_report(&paths, &TaskListRequest::default()).is_ok());
}

#[test]
fn task_note_frontmatter_mutation_rejects_a_source_changed_after_loading() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    fs::write(temp_dir.path().join("Daily.md"), "original\n").expect("seed note");

    let error = apply_note_frontmatter_mutation(
        &paths,
        "Daily.md",
        None,
        "task metadata",
        false,
        |frontmatter, _loaded| {
            frontmatter.insert(
                YamlValue::String("pomodoros".to_string()),
                YamlValue::Sequence(Vec::new()),
            );
            fs::write(temp_dir.path().join("Daily.md"), "concurrent edit\n")
                .expect("concurrent edit");
            Ok(vec![RefactorChange {
                before: "<missing>".to_string(),
                after: "pomodoros".to_string(),
            }])
        },
    )
    .expect_err("stale task metadata must fail");
    assert!(error
        .to_string()
        .contains("note changed during note task metadata"));
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Daily.md")).expect("retained note"),
        "concurrent edit\n"
    );
}

#[test]
fn task_note_frontmatter_mutation_refuses_a_late_periodic_note_collision() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    let relative_path = "Daily/2026-04-20.md";
    let target = temp_dir.path().join(relative_path);

    let error = apply_note_frontmatter_mutation(
        &paths,
        relative_path,
        Some("daily"),
        "task metadata",
        false,
        |_frontmatter, _loaded| {
            fs::create_dir_all(target.parent().expect("daily folder")).expect("create folder");
            fs::write(&target, "concurrent daily note\n").expect("concurrent note");
            Ok(Vec::new())
        },
    )
    .expect_err("late periodic note collision must fail");
    assert!(!error.to_string().is_empty());
    assert_eq!(
        fs::read_to_string(&target).expect("retained daily note"),
        "concurrent daily note\n"
    );
}

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
fn tasknote_reschedule_waits_for_the_vault_write_lock() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    let config = load_vault_config(&paths).config;
    seed_tasknote(&paths, &config, "Tasks/One.md", "One", "open", &[], "").expect("seed tasknote");
    let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("hold vault lock");
    let (started_sender, started_receiver) = mpsc::channel();
    let (result_sender, result_receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        started_sender.send(()).expect("start signal");
        let result = apply_task_reschedule(
            &paths,
            &TaskRescheduleRequest {
                task: "Tasks/One".to_string(),
                due: "2026-04-20".to_string(),
                dry_run: false,
            },
        );
        result_sender.send(result).expect("result signal");
    });
    started_receiver.recv().expect("worker started");
    assert!(result_receiver
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    drop(held);
    result_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("worker completed")
        .expect("tasknote updated");
    worker.join().expect("worker joined");
    let updated =
        fs::read_to_string(temp_dir.path().join("Tasks/One.md")).expect("updated tasknote");
    assert!(updated.contains("2026-04-20"));
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
    assert!(rendered.contains("- [ ] Call Alice 📅 2026-04-20"));
}

#[test]
fn apply_task_reschedule_replaces_any_tasks_due_marker() {
    for marker in ["📅", "📆", "🗓️", "🗓"] {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::write(
            temp_dir.path().join("Inbox.md"),
            format!("- [ ] Call Alice {marker} 2026-01-01 🔺\n"),
        )
        .expect("seed note");
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

        apply_task_reschedule(
            &paths,
            &TaskRescheduleRequest {
                task: "Inbox.md:1".to_string(),
                due: "2026-04-20".to_string(),
                dry_run: false,
            },
        )
        .expect("reschedule report");

        let rendered = fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("updated note");
        assert_eq!(rendered, "- [ ] Call Alice 📅 2026-04-20 🔺\n", "{marker}");
    }
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
fn inline_task_updates_wait_for_the_vault_write_lock() {
    for action in ["complete", "reschedule"] {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        fs::write(temp_dir.path().join("Inbox.md"), "- [ ] Call Alice\n").expect("seed note");
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");
        let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("hold vault lock");
        let (started_sender, started_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_sender.send(()).expect("start signal");
            let result = if action == "complete" {
                apply_task_complete(
                    &paths,
                    &TaskCompleteRequest {
                        task: "Inbox.md:1".to_string(),
                        date: Some("2026-04-20".to_string()),
                        dry_run: false,
                    },
                )
            } else {
                apply_task_reschedule(
                    &paths,
                    &TaskRescheduleRequest {
                        task: "Inbox.md:1".to_string(),
                        due: "2026-04-20".to_string(),
                        dry_run: false,
                    },
                )
            };
            result_sender.send(result).expect("result signal");
        });
        started_receiver.recv().expect("worker started");
        assert!(result_receiver
            .recv_timeout(Duration::from_millis(100))
            .is_err());
        assert_eq!(
            fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("still unchanged"),
            "- [ ] Call Alice\n"
        );
        drop(held);
        result_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("worker completed")
            .expect("task updated");
        worker.join().expect("worker joined");
        let updated = fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("updated note");
        assert!(
            updated.contains("2026-04-20"),
            "{action} did not update note"
        );
    }
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
fn task_add_refuses_a_destination_created_while_waiting_for_the_vault_lock() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    let config = load_vault_config(&paths).config;
    let target = temp_dir
        .path()
        .join(&config.tasknotes.tasks_folder)
        .join("Review.md");
    let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("hold vault lock");
    let (started_sender, started_receiver) = mpsc::channel();
    let (result_sender, result_receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        started_sender.send(()).expect("start signal");
        let result = apply_task_add(
            &paths,
            &TaskAddRequest {
                text: "Review".to_string(),
                no_nlp: true,
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
        );
        result_sender.send(result).expect("result signal");
    });
    started_receiver.recv().expect("worker started");
    assert!(result_receiver
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    fs::create_dir_all(target.parent().expect("task folder")).expect("create task folder");
    fs::write(&target, "concurrent task\n").expect("create concurrent task");
    drop(held);
    let error = result_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("worker completed")
        .expect_err("collision must fail");
    worker.join().expect("worker joined");
    assert!(!error.to_string().is_empty());
    assert_eq!(
        fs::read_to_string(target).expect("retained task"),
        "concurrent task\n"
    );
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
    assert!(rendered.contains("- [ ] Call Alice 📅 2026-04-20 ⏫"));
}

#[test]
fn guarded_task_routing_never_borrows_default_control_authority() {
    let directory = tempdir().unwrap();
    let paths = VaultPaths::new(directory.path());
    initialize_vulcan_dir(&paths).unwrap();
    let config = "[permissions.profiles.scoped]\nread = { allow = [\"folder:Allowed/**\"] }\nwrite = { allow = [\"folder:Allowed/**\"] }\n";
    fs::write(paths.config_file(), config).unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("scoped")).unwrap(),
    );
    for content in [None, Some("hidden: [invalid")] {
        if let Some(content) = content {
            fs::write(directory.path().join("mdbase.yaml"), content).unwrap();
        }
        for dry_run in [true, false] {
            let error = apply_task_create_with_guard(
                &paths,
                &TaskCreateRequest {
                    text: "Scoped task".into(),
                    note: Some("Allowed/Inbox".into()),
                    due: None,
                    priority: None,
                    dry_run,
                },
                Some(&guard),
            )
            .unwrap_err();
            assert_eq!(error.code(), Some("permission_denied"));
            assert_eq!(
                error.message(),
                "permission denied for required mdbase controls"
            );
            assert!(!directory.path().join("Allowed/Inbox.md").exists());
        }
    }
    // A same-name profile widened since the caller acquired its guard must not
    // silently lend the new authority to this invocation.
    fs::write(
        paths.config_file(),
        config.replace(
            "read = { allow = [\"folder:Allowed/**\"] }",
            "read = \"all\"",
        ),
    )
    .unwrap();
    let error = super::task_mutation_profile(&paths, Some(&guard)).unwrap_err();
    assert_eq!(error.code(), Some("permission_denied"));
    assert!(error.message().contains("authority changed"));
}

#[test]
fn guarded_task_create_checks_the_actual_write_path_before_mutation() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    fs::write(
        paths.config_file(),
        "[permissions.profiles.agent]\nread = \"all\"\nwrite = { allow = [\"folder:Allowed/**\"] }\n",
    )
    .expect("config");
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("agent")).expect("profile"),
    );

    let denied = apply_task_create_with_guard(
        &paths,
        &TaskCreateRequest {
            text: "Private task".to_string(),
            note: Some("Denied/Inbox".to_string()),
            due: None,
            priority: None,
            dry_run: false,
        },
        Some(&guard),
    )
    .expect_err("denied path must fail before writing");
    assert!(!denied.to_string().is_empty());
    assert!(!temp_dir.path().join("Denied/Inbox.md").exists());

    let allowed = apply_task_create_with_guard(
        &paths,
        &TaskCreateRequest {
            text: "Allowed task".to_string(),
            note: Some("Allowed/Inbox".to_string()),
            due: None,
            priority: None,
            dry_run: false,
        },
        Some(&guard),
    )
    .expect("allowed path");
    assert_eq!(allowed.path, "Allowed/Inbox.md");
    assert!(temp_dir.path().join("Allowed/Inbox.md").exists());
}

#[test]
fn guarded_task_updates_reject_denied_tasknote_and_inline_paths() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    fs::write(
        paths.config_file(),
        "[permissions.profiles.agent]\nread = \"all\"\nwrite = { allow = [\"folder:Allowed/**\"] }\n",
    )
    .expect("config");
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "Denied/Task.md",
        "Denied task",
        "open",
        &[],
        "",
    )
    .expect("tasknote");
    seed_tasknote(
        &paths,
        &config,
        "Allowed/PermittedTask.md",
        "Allowed task",
        "open",
        &[],
        "",
    )
    .expect("allowed tasknote");
    fs::write(
        temp_dir.path().join("Denied/Inline.md"),
        "- [ ] Denied inline\n",
    )
    .expect("inline note");
    fs::write(
        temp_dir.path().join("Allowed/PermittedInline.md"),
        "- [ ] Allowed inline\n",
    )
    .expect("allowed inline note");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");
    let note_index = vulcan_core::properties::load_note_index(&paths).expect("note index");
    assert_eq!(
        super::inline_tasks_for_path(&note_index, "Allowed/PermittedInline.md").len(),
        1,
        "allowed inline task should be indexed: {:?}",
        note_index
            .values()
            .map(|note| (&note.document_path, note.tasks.len()))
            .collect::<Vec<_>>()
    );
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("agent")).expect("profile"),
    );
    let tasknote_before =
        fs::read_to_string(temp_dir.path().join("Denied/Task.md")).expect("tasknote before");
    let inline_before =
        fs::read_to_string(temp_dir.path().join("Denied/Inline.md")).expect("inline before");

    for request in [
        TaskCompleteRequest {
            task: "Denied/Task".to_string(),
            date: Some("2026-04-20".to_string()),
            dry_run: false,
        },
        TaskCompleteRequest {
            task: "Denied/Inline.md:1".to_string(),
            date: Some("2026-04-20".to_string()),
            dry_run: false,
        },
    ] {
        let error = apply_task_complete_with_guard(&paths, &request, Some(&guard))
            .expect_err("denied completion must fail");
        assert!(error.to_string().contains("permission denied"), "{error}");
    }
    for request in [
        TaskRescheduleRequest {
            task: "Denied/Task".to_string(),
            due: "2026-04-20".to_string(),
            dry_run: false,
        },
        TaskRescheduleRequest {
            task: "Denied/Inline.md:1".to_string(),
            due: "2026-04-20".to_string(),
            dry_run: false,
        },
    ] {
        let error = apply_task_reschedule_with_guard(&paths, &request, Some(&guard))
            .expect_err("denied reschedule must fail");
        assert!(error.to_string().contains("permission denied"), "{error}");
    }
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Denied/Task.md")).expect("tasknote after"),
        tasknote_before
    );
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Denied/Inline.md")).expect("inline after"),
        inline_before
    );

    let completed = apply_task_complete_with_guard(
        &paths,
        &TaskCompleteRequest {
            task: "Allowed/PermittedTask".to_string(),
            date: Some("2026-04-20".to_string()),
            dry_run: false,
        },
        Some(&guard),
    )
    .expect("allowed tasknote completion");
    assert_eq!(completed.changed_paths, ["Allowed/PermittedTask.md"]);
    let rescheduled = apply_task_reschedule_with_guard(
        &paths,
        &TaskRescheduleRequest {
            task: "Allowed/PermittedInline.md:1".to_string(),
            due: "2026-04-20".to_string(),
            dry_run: false,
        },
        Some(&guard),
    )
    .expect("allowed inline reschedule");
    assert_eq!(rescheduled.changed_paths, ["Allowed/PermittedInline.md"]);
}

#[test]
fn task_create_waits_for_the_vault_write_lock() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    fs::write(temp_dir.path().join("Inbox.md"), "# Tasks\n").expect("seed inbox");
    let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("hold vault lock");
    let (started_sender, started_receiver) = mpsc::channel();
    let (result_sender, result_receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        started_sender.send(()).expect("start signal");
        let result = apply_task_create(
            &paths,
            &TaskCreateRequest {
                text: "Call Alice".to_string(),
                note: Some("Inbox".to_string()),
                due: None,
                priority: None,
                dry_run: false,
            },
        );
        result_sender.send(result).expect("result signal");
    });
    started_receiver.recv().expect("worker started");
    assert!(result_receiver
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("still unchanged"),
        "# Tasks\n"
    );
    drop(held);
    result_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("worker completed")
        .expect("task created");
    worker.join().expect("worker joined");
    let updated = fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("updated inbox");
    assert!(updated.contains("Call Alice"));
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
fn task_note_conversion_rejects_a_change_while_waiting_for_the_vault_lock() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    fs::write(temp_dir.path().join("Idea.md"), "original\n").expect("seed note");
    let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("hold vault lock");
    let (started_sender, started_receiver) = mpsc::channel();
    let (result_sender, result_receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        started_sender.send(()).expect("start signal");
        let result = apply_task_convert(
            &paths,
            &TaskConvertRequest {
                file: "Idea".to_string(),
                line: None,
                dry_run: false,
            },
        );
        result_sender.send(result).expect("result signal");
    });
    started_receiver.recv().expect("worker started");
    assert!(result_receiver
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    fs::write(temp_dir.path().join("Idea.md"), "concurrent edit\n").expect("change note");
    drop(held);
    let error = result_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("worker completed")
        .expect_err("stale conversion must fail");
    worker.join().expect("worker joined");
    assert!(error
        .to_string()
        .contains("note changed during note task conversion"));
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Idea.md")).expect("retained note"),
        "concurrent edit\n"
    );
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
    #[cfg(unix)]
    let mut original_handle = fs::File::open(temp_dir.path().join("Inbox.md"))
        .expect("open original source before conversion");

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
    assert!(!paths
        .operational_state_dir()
        .expect("operational state")
        .join("ordinary-write/journal.json")
        .exists());

    let source = fs::read_to_string(temp_dir.path().join("Inbox.md"))
        .expect("rewritten inbox")
        .replace("\r\n", "\n");
    let link_target = report.target_path.trim_end_matches(".md");
    assert!(source.contains(&format!("[[{link_target}]]")));
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut original = String::new();
        original_handle
            .read_to_string(&mut original)
            .expect("read pre-conversion source handle");
        assert_eq!(original, "- [ ] Review launch plan tomorrow @work\n");
    }
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
fn ordinary_task_conversion_routing_recheck_rejects_new_collection_membership() {
    let directory = tempdir().unwrap();
    let paths = VaultPaths::new(directory.path());
    initialize_vulcan_dir(&paths).unwrap();
    fs::write(directory.path().join("Inbox.md"), "Original\n").unwrap();
    fs::write(
        directory.path().join("mdbase.yaml"),
        "spec_version: '0.3.0'\n",
    )
    .unwrap();
    let error = write_ordinary_task_conversion(
        &paths,
        "Inbox.md",
        "Original\n",
        "Converted\n",
        "Task.md",
        "Task\n",
    )
    .unwrap_err();
    assert!(error.message().contains("mdbase collection changed"));
    assert_eq!(
        fs::read_to_string(directory.path().join("Inbox.md")).unwrap(),
        "Original\n"
    );
    assert!(!directory.path().join("Task.md").exists());
}

#[test]
fn ordinary_task_line_conversion_refuses_stale_source_and_late_target_collision() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    fs::write(temp_dir.path().join("Inbox.md"), "newer source\n").expect("seed source");

    let error = write_ordinary_task_conversion(
        &paths,
        "Inbox.md",
        "old source\n",
        "updated source\n",
        "Tasks/New.md",
        "new task\n",
    )
    .expect_err("stale source must fail");
    assert_eq!(error.code(), Some("ordinary_write_stale"));
    assert!(error.to_string().contains("Inbox.md changed before apply"));
    assert!(!temp_dir.path().join("Tasks/New.md").exists());

    fs::create_dir_all(temp_dir.path().join("Tasks")).expect("task folder");
    fs::write(temp_dir.path().join("Tasks/New.md"), "other task\n").expect("concurrent task");
    assert!(write_ordinary_task_conversion(
        &paths,
        "Inbox.md",
        "newer source\n",
        "updated source\n",
        "Tasks/New.md",
        "new task\n",
    )
    .is_err());
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("source"),
        "newer source\n"
    );
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Tasks/New.md")).expect("target"),
        "other task\n"
    );
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
    assert!(!temp_dir.path().join("Tasks/Done.md").exists());
    assert!(!paths
        .operational_state_dir()
        .expect("operational state")
        .join("ordinary-write/journal.json")
        .exists());
    let rendered = fs::read_to_string(temp_dir.path().join(&report.path))
        .expect("archived task")
        .replace("\r\n", "\n");
    assert!(rendered.contains(&format!("- {}", config.tasknotes.field_mapping.archive_tag)));
}

#[test]
fn ordinary_task_move_refuses_stale_source_and_late_destination_collision() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    fs::create_dir_all(temp_dir.path().join("Tasks")).expect("task folder");
    fs::write(temp_dir.path().join("Tasks/Done.md"), "newer task\n").expect("seed task");

    let error = move_ordinary_tasknote_if_unchanged(
        &paths,
        "Tasks/Done.md",
        "old task\n",
        "Archive/Done.md",
        "archived task\n",
        None,
    )
    .expect_err("stale move must fail");
    assert_eq!(error.code(), Some("ordinary_write_stale"));
    assert!(error
        .to_string()
        .contains("Tasks/Done.md changed before apply"));
    assert!(!temp_dir.path().join("Archive/Done.md").exists());

    fs::create_dir_all(temp_dir.path().join("Archive")).expect("archive folder");
    fs::write(temp_dir.path().join("Archive/Done.md"), "other task\n")
        .expect("concurrent destination");
    assert!(move_ordinary_tasknote_if_unchanged(
        &paths,
        "Tasks/Done.md",
        "newer task\n",
        "Archive/Done.md",
        "archived task\n",
        None,
    )
    .is_err());
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Tasks/Done.md")).expect("source"),
        "newer task\n"
    );
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Archive/Done.md")).expect("destination"),
        "other task\n"
    );
}

#[test]
fn managed_task_set_uses_validated_write_journal() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    seed_mdbase_task_type(&paths);
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Managed.md",
        "Managed",
        "open",
        &[],
        "",
    )
    .expect("seed task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan should succeed");

    apply_task_set(
        &paths,
        &TaskSetRequest {
            task: "Tasks/Managed".to_string(),
            property: "status".to_string(),
            value: "done".to_string(),
            dry_run: false,
        },
    )
    .expect("managed task update should succeed");

    let outbox = list_mdbase_write_outbox(&paths).expect("outbox");
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].operation, "update");
    assert_eq!(outbox[0].paths[0].path, "Tasks/Managed.md");
}

#[test]
fn managed_task_add_uses_create_journal() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    seed_mdbase_task_type(&paths);

    let report = apply_task_add(
        &paths,
        &TaskAddRequest {
            text: "Journal new task".to_string(),
            no_nlp: true,
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
    .expect("managed task creation should succeed");

    assert!(temp_dir.path().join(&report.path).exists());
    let outbox = list_mdbase_write_outbox(&paths).expect("outbox");
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].operation, "create");
    assert_eq!(outbox[0].paths[0].path, report.path);
}

#[test]
fn managed_task_line_conversion_uses_one_batch_journal() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    seed_mdbase_task_type(&paths);
    fs::write(
        temp_dir.path().join("Inbox.md"),
        "- [ ] Convert this task\n",
    )
    .expect("seed inbox");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan should succeed");

    let report = apply_task_convert(
        &paths,
        &TaskConvertRequest {
            file: "Inbox".to_string(),
            line: Some(1),
            dry_run: false,
        },
    )
    .expect("managed line conversion should succeed");

    assert!(temp_dir.path().join(&report.target_path).exists());
    let outbox = list_mdbase_write_outbox(&paths).expect("outbox");
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].operation, "batch");
    assert_eq!(outbox[0].paths.len(), 2);
}

#[test]
fn mixed_task_line_conversion_journals_the_ordinary_source_with_the_managed_task() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init");
    seed_mdbase_task_type(&paths);
    let source_path = "TaskNotes/Archive/Inbox.md";
    fs::create_dir_all(temp_dir.path().join("TaskNotes/Archive")).expect("archive folder");
    fs::write(
        temp_dir.path().join(source_path),
        "- [ ] Convert mixed task\n",
    )
    .expect("seed ordinary source");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

    let report = apply_task_convert(
        &paths,
        &TaskConvertRequest {
            file: source_path.to_string(),
            line: Some(1),
            dry_run: false,
        },
    )
    .expect("mixed conversion");

    assert!(temp_dir.path().join(&report.target_path).exists());
    let source = fs::read_to_string(temp_dir.path().join(source_path)).expect("rewritten source");
    assert!(source.contains(&format!(
        "[[{}]]",
        report.target_path.trim_end_matches(".md")
    )));
    let outbox = list_mdbase_write_outbox(&paths).expect("outbox");
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].operation, "batch");
    assert_eq!(outbox[0].paths.len(), 2);
    assert!(outbox[0]
        .paths
        .iter()
        .any(|event| event.path == source_path));
}

#[test]
fn invalid_managed_task_set_fails_before_writing() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    seed_mdbase_task_type(&paths);
    let config = load_vault_config(&paths).config;
    seed_tasknote(
        &paths,
        &config,
        "Tasks/Managed.md",
        "Managed",
        "open",
        &[],
        "",
    )
    .expect("seed task");
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan should succeed");
    let original = fs::read_to_string(temp_dir.path().join("Tasks/Managed.md")).unwrap();

    let error = apply_task_set(
        &paths,
        &TaskSetRequest {
            task: "Tasks/Managed".to_string(),
            property: "title".to_string(),
            value: "null".to_string(),
            dry_run: false,
        },
    )
    .expect_err("required title removal should fail");

    assert!(error.message().contains("schema_required"));
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Tasks/Managed.md")).unwrap(),
        original
    );
    assert!(list_mdbase_write_outbox(&paths).expect("outbox").is_empty());
}

#[test]
fn managed_task_archive_journals_cross_boundary_rename() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    initialize_vulcan_dir(&paths).expect("init should succeed");
    seed_mdbase_task_type(&paths);
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
    scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan should succeed");

    let report = apply_task_archive(
        &paths,
        &TaskArchiveRequest {
            task: "Tasks/Done".to_string(),
            dry_run: false,
        },
    )
    .expect("archive should succeed");

    assert!(!temp_dir.path().join("Tasks/Done.md").exists());
    assert!(temp_dir.path().join(&report.path).exists());
    let outbox = list_mdbase_write_outbox(&paths).expect("outbox");
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].operation, "rename");
    assert_eq!(outbox[0].paths.len(), 2);
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

fn seed_mdbase_task_type(paths: &VaultPaths) {
    fs::write(
        paths.vault_root().join("mdbase.yaml"),
        "spec_version: \"0.3.0\"\nsettings:\n  exclude: [TaskNotes/Archive/**]\n",
    )
    .expect("mdbase config");
    fs::create_dir_all(paths.vault_root().join("_types")).expect("type directory");
    fs::write(
        paths.vault_root().join("_types/task.md"),
        concat!(
            "---\n",
            "kind: mdbase.type\n",
            "name: task\n",
            "version: 1\n",
            "match:\n",
            "  path_glob: 'Tasks/*.md'\n",
            "schema:\n",
            "  dialect: json-schema-2020-12\n",
            "  value:\n",
            "    type: object\n",
            "    required: [title]\n",
            "    properties:\n",
            "      title: {type: string}\n",
            "      status: {type: string}\n",
            "---\n",
        ),
    )
    .expect("task type");
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
