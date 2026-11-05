//! MCP task mutation workflows shared by stdio and hosted transports.

use vulcan_core::Verbosity;
use vulcan_core::{ProfilePermissionGuard, VaultPaths};

use crate::commit::AutoCommitPolicy;
use crate::mcp_access::check_write_path_access;
use crate::mcp_protocol::{
    McpMethodError, McpTaskCompleteArgs, McpTaskCreateArgs, McpTaskReminderAddArgs,
    McpTaskReminderRemoveArgs, McpTaskRescheduleArgs,
};
use crate::scan::refresh_cache_for_paths;
use crate::tasks::{
    apply_task_complete_with_guard, apply_task_create_with_guard,
    apply_task_reminder_add_with_guard, apply_task_reminder_remove_with_guard,
    apply_task_reschedule_with_guard, TaskCompleteRequest, TaskCreateReport, TaskCreateRequest,
    TaskMutationReport, TaskReminderAddRequest, TaskReminderRemoveRequest, TaskRescheduleRequest,
};

pub fn task_create(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: McpTaskCreateArgs,
) -> Result<TaskCreateReport, McpMethodError> {
    let mut request = TaskCreateRequest {
        text: args.text,
        note: args.note,
        due: args.due,
        priority: args.priority,
        dry_run: true,
    };
    if !args.dry_run {
        let planned = apply_task_create_with_guard(paths, &request, Some(guard))
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        for path in &planned.changed_paths {
            check_write_path_access(guard, path)?;
        }
    }
    request.dry_run = args.dry_run;
    let report = apply_task_create_with_guard(paths, &request, Some(guard))
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    refresh_and_commit(
        paths,
        profile_name,
        "task-create",
        &report.changed_paths,
        args.dry_run,
        args.no_commit,
    )?;
    Ok(report)
}

pub fn task_complete(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: McpTaskCompleteArgs,
) -> Result<TaskMutationReport, McpMethodError> {
    let mut request = TaskCompleteRequest {
        task: args.task,
        date: args.date,
        dry_run: true,
    };
    if !args.dry_run {
        let planned = apply_task_complete_with_guard(paths, &request, Some(guard))
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        for path in &planned.changed_paths {
            check_write_path_access(guard, path)?;
        }
    }
    request.dry_run = args.dry_run;
    let report = apply_task_complete_with_guard(paths, &request, Some(guard))
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    refresh_and_commit(
        paths,
        profile_name,
        "task-complete",
        &report.changed_paths,
        args.dry_run,
        args.no_commit,
    )?;
    Ok(report)
}

pub fn task_reschedule(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: McpTaskRescheduleArgs,
) -> Result<TaskMutationReport, McpMethodError> {
    let mut request = TaskRescheduleRequest {
        task: args.task,
        due: args.due,
        dry_run: true,
    };
    if !args.dry_run {
        let planned = apply_task_reschedule_with_guard(paths, &request, Some(guard))
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        for path in &planned.changed_paths {
            check_write_path_access(guard, path)?;
        }
    }
    request.dry_run = args.dry_run;
    let report = apply_task_reschedule_with_guard(paths, &request, Some(guard))
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    refresh_and_commit(
        paths,
        profile_name,
        "task-reschedule",
        &report.changed_paths,
        args.dry_run,
        args.no_commit,
    )?;
    Ok(report)
}

pub fn task_reminder_add(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: McpTaskReminderAddArgs,
) -> Result<TaskMutationReport, McpMethodError> {
    let mut request = TaskReminderAddRequest {
        task: args.task,
        spec: args.reminder,
        description: args.description,
        dry_run: true,
    };
    if !args.dry_run {
        let planned = apply_task_reminder_add_with_guard(paths, &request, Some(guard))
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        for path in &planned.changed_paths {
            check_write_path_access(guard, path)?;
        }
    }
    request.dry_run = args.dry_run;
    let report = apply_task_reminder_add_with_guard(paths, &request, Some(guard))
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    refresh_and_commit(
        paths,
        profile_name,
        "task-reminder-add",
        &report.changed_paths,
        args.dry_run,
        args.no_commit,
    )?;
    Ok(report)
}

pub fn task_reminder_remove(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: McpTaskReminderRemoveArgs,
) -> Result<TaskMutationReport, McpMethodError> {
    let mut request = TaskReminderRemoveRequest {
        task: args.task,
        id: args.id,
        dry_run: true,
    };
    if !args.dry_run {
        let planned = apply_task_reminder_remove_with_guard(paths, &request, Some(guard))
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        for path in &planned.changed_paths {
            check_write_path_access(guard, path)?;
        }
    }
    request.dry_run = args.dry_run;
    let report = apply_task_reminder_remove_with_guard(paths, &request, Some(guard))
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    refresh_and_commit(
        paths,
        profile_name,
        "task-reminder-remove",
        &report.changed_paths,
        args.dry_run,
        args.no_commit,
    )?;
    Ok(report)
}

fn refresh_and_commit(
    paths: &VaultPaths,
    profile_name: &str,
    trigger: &str,
    changed_paths: &[String],
    dry_run: bool,
    no_commit: bool,
) -> Result<(), McpMethodError> {
    if !dry_run && !changed_paths.is_empty() {
        refresh_cache_for_paths(paths, changed_paths)
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
    }
    if !dry_run {
        AutoCommitPolicy::for_mutation(paths, no_commit)
            .commit(
                paths,
                trigger,
                changed_paths,
                Some(profile_name),
                Verbosity::Quiet,
            )
            .map_err(|error| McpMethodError::tool(error.clone()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use vulcan_core::paths::initialize_vulcan_dir;
    use vulcan_core::resolve_permission_profile;

    #[test]
    fn task_planning_and_apply_deny_hidden_controls_before_inspection() {
        for control in [None, Some("hidden_synthetic_control: [invalid\n")] {
            let temporary = tempfile::tempdir().unwrap();
            let paths = VaultPaths::new(temporary.path());
            initialize_vulcan_dir(&paths).unwrap();
            fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:Public/**\"] }\nwrite = { allow = [\"folder:Public/**\"] }\n").unwrap();
            fs::create_dir_all(temporary.path().join("Public")).unwrap();
            let source = "- [ ] Visible synthetic task\n";
            fs::write(temporary.path().join("Public/Tasks.md"), source).unwrap();
            if let Some(control) = control {
                fs::write(temporary.path().join("mdbase.yaml"), control).unwrap();
            }
            vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).unwrap();
            let guard = ProfilePermissionGuard::new(
                &paths,
                resolve_permission_profile(&paths, Some("scoped")).unwrap(),
            );
            for dry_run in [true, false] {
                let errors = [
                    task_create(
                        &paths,
                        &guard,
                        "scoped",
                        serde_json::from_value(json!({
                            "text": "New task", "note": "Public/Tasks.md",
                            "dry_run": dry_run, "no_commit": true,
                        }))
                        .unwrap(),
                    )
                    .unwrap_err(),
                    task_complete(
                        &paths,
                        &guard,
                        "scoped",
                        serde_json::from_value(json!({
                            "task": "Public/Tasks.md:1", "date": "2026-04-04",
                            "dry_run": dry_run, "no_commit": true,
                        }))
                        .unwrap(),
                    )
                    .unwrap_err(),
                    task_reschedule(
                        &paths,
                        &guard,
                        "scoped",
                        serde_json::from_value(json!({
                            "task": "Public/Tasks.md:1", "due": "2026-04-05",
                            "dry_run": dry_run, "no_commit": true,
                        }))
                        .unwrap(),
                    )
                    .unwrap_err(),
                ];
                for error in errors {
                    let McpMethodError::Tool { message, .. } = error else {
                        panic!("unexpected protocol error: {error:?}");
                    };
                    assert_eq!(message, "permission denied for required mdbase controls");
                }
                assert_eq!(
                    fs::read_to_string(temporary.path().join("Public/Tasks.md")).unwrap(),
                    source
                );
            }
        }
    }

    #[test]
    fn task_create_denies_readonly_before_write_and_applies_under_writable_grant() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        let readonly = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let writable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("unrestricted")).unwrap(),
        );
        let args = || {
            serde_json::from_value::<McpTaskCreateArgs>(
                json!({"text": "Review MCP grant", "note": "Tasks.md", "no_commit": true}),
            )
            .unwrap()
        };
        assert!(task_create(&paths, &readonly, "readonly", args()).is_err());
        assert!(!temporary.path().join("Tasks.md").exists());
        let report = task_create(&paths, &writable, "unrestricted", args()).unwrap();
        assert_eq!(report.changed_paths, vec!["Tasks.md"]);
        assert!(fs::read_to_string(temporary.path().join("Tasks.md"))
            .unwrap()
            .contains("Review MCP grant"));
        let reschedule_args = || {
            serde_json::from_value::<McpTaskRescheduleArgs>(json!({
                "task": report.task,
                "due": "2026-05-12",
                "no_commit": true,
            }))
            .unwrap()
        };
        assert!(task_reschedule(&paths, &readonly, "readonly", reschedule_args()).is_err());
        assert!(!fs::read_to_string(temporary.path().join("Tasks.md"))
            .unwrap()
            .contains("2026-05-12"));
        task_reschedule(&paths, &writable, "unrestricted", reschedule_args()).unwrap();
        assert!(fs::read_to_string(temporary.path().join("Tasks.md"))
            .unwrap()
            .contains("2026-05-12"));
        let complete_args = || {
            serde_json::from_value::<McpTaskCompleteArgs>(json!({
                "task": report.task,
                "date": "2026-05-13",
                "no_commit": true,
            }))
            .unwrap()
        };
        assert!(task_complete(&paths, &readonly, "readonly", complete_args()).is_err());
        task_complete(&paths, &writable, "unrestricted", complete_args()).unwrap();
        assert!(fs::read_to_string(temporary.path().join("Tasks.md"))
            .unwrap()
            .contains("- [x]"));
    }

    #[test]
    fn task_reminders_need_write_access_and_round_trip_under_a_writable_grant() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::create_dir_all(temporary.path().join("TaskNotes/Tasks")).unwrap();
        let task_path = temporary.path().join("TaskNotes/Tasks/Dentist.md");
        let source =
            "---\ntitle: Dentist\nstatus: open\ndue: 2026-11-06T14:00\ntags:\n- task\n---\n";
        fs::write(&task_path, source).unwrap();
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).unwrap();
        let guard = |profile: &str| {
            ProfilePermissionGuard::new(
                &paths,
                resolve_permission_profile(&paths, Some(profile)).unwrap(),
            )
        };
        let (readonly, writable) = (guard("readonly"), guard("unrestricted"));
        let add_args = || {
            serde_json::from_value::<McpTaskReminderAddArgs>(json!({
                "task": "Dentist", "reminder": "-1h", "description": "Leave", "no_commit": true,
            }))
            .unwrap()
        };

        assert!(task_reminder_add(&paths, &readonly, "readonly", add_args()).is_err());
        assert_eq!(fs::read_to_string(&task_path).unwrap(), source);
        let report = task_reminder_add(&paths, &writable, "unrestricted", add_args()).unwrap();
        assert_eq!(report.changed_paths, vec!["TaskNotes/Tasks/Dentist.md"]);
        let written = fs::read_to_string(&task_path).unwrap();
        assert!(written.contains("offset: -PT1H"), "{written}");
        assert!(written.contains("relatedTo: due"), "{written}");
        let id = written
            .lines()
            .find_map(|line| {
                line.trim_start()
                    .trim_start_matches("- ")
                    .strip_prefix("id: ")
            })
            .expect("reminder id")
            .to_string();

        let remove_args = |dry_run: bool| {
            serde_json::from_value::<McpTaskReminderRemoveArgs>(json!({
                "task": "Dentist", "id": id, "dry_run": dry_run, "no_commit": true,
            }))
            .unwrap()
        };
        assert!(task_reminder_remove(&paths, &readonly, "readonly", remove_args(false)).is_err());
        task_reminder_remove(&paths, &writable, "unrestricted", remove_args(true)).unwrap();
        assert_eq!(fs::read_to_string(&task_path).unwrap(), written);
        task_reminder_remove(&paths, &writable, "unrestricted", remove_args(false)).unwrap();
        assert!(!fs::read_to_string(&task_path).unwrap().contains(&id));
    }
}
