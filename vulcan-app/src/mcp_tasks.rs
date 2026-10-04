//! MCP task mutation workflows shared by stdio and hosted transports.

use vulcan_core::{ProfilePermissionGuard, VaultPaths};

use crate::commit::AutoCommitPolicy;
use crate::mcp_access::check_write_path_access;
use crate::mcp_protocol::{
    McpMethodError, McpTaskCompleteArgs, McpTaskCreateArgs, McpTaskRescheduleArgs,
};
use crate::scan::refresh_cache_incrementally;
use crate::tasks::{
    apply_task_complete_with_guard, apply_task_create_with_guard, apply_task_reschedule_with_guard,
    TaskCompleteRequest, TaskCreateReport, TaskCreateRequest, TaskMutationReport,
    TaskRescheduleRequest,
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

fn refresh_and_commit(
    paths: &VaultPaths,
    profile_name: &str,
    trigger: &str,
    changed_paths: &[String],
    dry_run: bool,
    no_commit: bool,
) -> Result<(), McpMethodError> {
    if !dry_run && !changed_paths.is_empty() {
        refresh_cache_incrementally(paths)
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
    }
    if !dry_run {
        AutoCommitPolicy::for_mutation(paths, no_commit)
            .commit(paths, trigger, changed_paths, Some(profile_name), true)
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
}
