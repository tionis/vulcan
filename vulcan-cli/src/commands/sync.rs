use crate::cli::{ManagedDirectoryProfileArg, NetworkNotificationModeArg};
use crate::editor::open_paths_in_editor;
use crate::output::print_json;
use crate::{
    selected_permission_guard, Cli, CliError, OutputFormat, SemanticGroupingArg,
    SyncCheckpointKindArg, SyncCommand, SyncConflictSideArg, SyncDeviceCommand, SyncForgeCommand,
    SyncScheduleCommand, SyncSelectionArgs, SyncTransportCommand, TermuxNetworkArg,
};
use serde::Serialize;
use std::io::{self, IsTerminal, Read, Write};
use std::time::Duration;
use vulcan_app::sync::{
    doctor_git_vault_for_profile, sync_git_vault_with_profile_and_progress, GitBranchSync,
    GitBranchSyncAction, GitDeviceBackupOutcome, GitPlatformProfile, GitRefName, GitRemote,
    GitSyncAction, GitSyncObserver, GitSyncObserverError, GitSyncOptions, GitSyncOutcome,
    GitSyncPhase, GitSyncPreviewFileState, GitSyncProgress, GitSyncReport, SyncContentProfile,
    SyncDoctorReport, SyncDoctorSeverity, VaultSyncReport,
};
use vulcan_app::sync_checkpoints::{
    create_sync_checkpoint, SyncCheckpointKind, SyncCheckpointOptions, SyncCheckpointReport,
};
use vulcan_app::sync_conflicts::{
    get_sync_conflict, list_sync_conflicts, resolve_sync_conflict, ResolveSyncConflictOptions,
    ResolveSyncConflictReport, SyncConflictDetailReport, SyncConflictListReport,
    SyncConflictResolutionSide, SyncConflictResolutionState,
};
use vulcan_app::sync_devices::{
    fetch_sync_device_backup, list_sync_device_backups_with_observation, remove_sync_device_backup,
    set_sync_device_name, GitSyncDeviceIdKind, SyncDeviceFetchReport, SyncDeviceListReport,
    SyncDeviceOptions, SyncDeviceRecoveryStatus, SyncDeviceRelation,
    SyncDeviceRemoteObservationState, SyncDeviceRemoveReport,
};
use vulcan_app::sync_notifications::{
    notification_status, publish_sync_notification_advertisement,
    remove_sync_notification_advertisement, SyncNotificationPublishOptions,
    SyncNotificationPublishReport, SyncNotificationRemoveOptions, SyncNotificationRemoveReport,
    SyncNotificationStatusOptions, SyncNotificationStatusReport,
};
use vulcan_app::sync_proposals::{
    approve_resolution_proposal, create_formatter_resolution_proposal,
    create_supplied_resolution_proposal, create_supplied_resolution_proposal_with_selection,
    prepare_editor_resolution, prepare_patch_resolution, preview_patch_resolution,
    preview_supplied_resolution, reject_resolution_proposal, ApproveResolutionProposalOptions,
    ApproveResolutionProposalReport, EditorResolutionPlan, FormatterResolutionOptions,
    FormatterResolutionReport, PatchResolutionPreviewReport, RejectResolutionProposalReport,
    ResolutionAgentPathOutput, ResolutionProposal, ResolutionProposalOptions,
    ResolutionProposalSelection, SuppliedResolutionPreviewReport,
};
#[cfg(feature = "web")]
use vulcan_app::sync_proposals::{
    create_and_auto_accept_resolution_proposal, create_resolution_proposal_for_target,
    AutoAcceptResolutionProposalReport, OpenAiCompatibleResolutionProvider,
};
use vulcan_app::sync_registration::{
    register_placeholder, revoke_registration, unregister_registration, RegistrationAction,
    RegistrationChangeReport, RegistrationListReport, RegistrationObservation, RegistrationStatus,
};
use vulcan_app::sync_retention::{
    apply_sync_retention, plan_sync_retention, SyncRetentionApplyReport, SyncRetentionPlanOptions,
    SyncRetentionPlanReport, SyncRetentionPolicy,
};
use vulcan_app::sync_semantic::{
    apply_semantic_plan, create_semantic_plan, load_semantic_plan, publish_semantic_plan,
    reject_semantic_plan, SemanticApplyReport, SemanticGrouping, SemanticPlanOptions,
    SemanticPlanReport, SemanticPublishReport, SemanticRejectReport,
};
#[cfg(feature = "web")]
use vulcan_app::sync_semantic::{
    create_semantic_plan_with_provider, OpenAiCompatibleSemanticProvider,
};
use vulcan_app::sync_semantic_auto::{run_semantic_auto, SemanticAutoOptions, SemanticAutoReport};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_core::{
    resolve_permission_profile, vulcan_user_data_dir, PermissionGuard, ProfilePermissionGuard,
    VaultPaths,
};
use vulcan_daemon::process::{daemon_status, DaemonProcessContext};
use vulcan_daemon::registry::{
    ManagedDirectoryProfile, NetworkNotificationMode, UpdateWikiRequest, WikiId, WikiRegistration,
    WikiRegistry,
};
use vulcan_daemon::sync::{sync_registered_wikis, RegisteredSyncReport, RegisteredSyncSelection};
use vulcan_daemon::termux_scheduler::{
    apply_termux_sync, load_termux_sync_plan, plan_termux_sync, TermuxNetwork, TermuxSyncAction,
    TermuxSyncInstallOptions, TermuxSyncReport, TermuxSyncUpdate,
};

pub(crate) fn handle_sync_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Result<(), CliError> {
    require_sync_knowledge_profile(cli, command)?;
    if matches!(command, SyncCommand::Clone { .. }) {
        return handle_sync_clone(cli, command);
    }
    if let Some(result) = handle_non_cycle_sync_command(cli, paths, command) {
        return result;
    }
    let (options, selection, profile) = match command {
        SyncCommand::Run {
            selection,
            target,
            profile,
            max_retries,
            git_timeout_seconds,
            dry_run,
        } => (
            GitSyncOptions {
                remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
                live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
                max_retries: *max_retries,
                command_timeout: Duration::from_secs(*git_timeout_seconds),
                dry_run: *dry_run,
                ..GitSyncOptions::default()
            },
            registered_selection(selection)?,
            *profile,
        ),
        SyncCommand::Status {
            selection,
            target,
            profile,
        } => (
            GitSyncOptions {
                remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
                live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
                dry_run: true,
                ..GitSyncOptions::default()
            },
            registered_selection(selection)?,
            *profile,
        ),
        _ => unreachable!(),
    };
    if let Some(selection) = selection {
        if profile.is_some() {
            return Err(CliError::operation(
                "`--profile` applies to direct sync only; registered wikis use their stored profile",
            ));
        }
        let registry = WikiRegistry::user_default().map_err(CliError::operation)?;
        let report =
            sync_registered_wikis(&registry, &selection, &options, cli.permissions.as_deref())
                .map_err(CliError::operation)?;
        print_registered_sync_report(cli.output, &report)?;
        if report.failed > 0 || report.conflicted > 0 || report.incomplete > 0 {
            return Err(CliError::issues(format!(
                "{} registered sync operation(s) failed, {} remain conflicted, and {} inspections were incomplete",
                report.failed, report.conflicted, report.incomplete
            )));
        }
        return Ok(());
    }
    selected_permission_guard(cli, paths)?
        .check_git()
        .map_err(CliError::operation)?;
    let mut observer = CliSyncProgress::new(
        options.max_retries.max(1),
        progress_mode(
            cli.output,
            cli.quiet,
            cli.verbose,
            io::stderr().is_terminal(),
        ),
    );
    let profile = match profile.unwrap_or(ManagedDirectoryProfileArg::Knowledge) {
        ManagedDirectoryProfileArg::Knowledge => SyncContentProfile::Knowledge,
        ManagedDirectoryProfileArg::FilesOnly => SyncContentProfile::FilesOnly,
    };
    let result = sync_git_vault_with_profile_and_progress(paths, &options, &mut observer, profile);
    observer.finish();
    let report = result.map_err(CliError::operation)?;
    print_sync_report(cli.output, cli.verbose, &report)
}

fn require_sync_knowledge_profile(cli: &Cli, command: &SyncCommand) -> Result<(), CliError> {
    let wiki = match command {
        SyncCommand::Propose { wiki, .. }
        | SyncCommand::FormatPropose { wiki, .. }
        | SyncCommand::SemanticPlan { wiki, .. }
        | SyncCommand::SemanticAuto { wiki, .. }
        | SyncCommand::Resolve {
            wiki,
            approve_proposal: Some(_),
            ..
        } => wiki.as_deref(),
        SyncCommand::SemanticApply { .. }
        | SyncCommand::SemanticPublish { .. }
        | SyncCommand::SemanticReject { .. } => None,
        _ => return Ok(()),
    };
    let registration = if let Some(id) = wiki {
        let registry = WikiRegistry::user_default().map_err(CliError::operation)?;
        Some(
            registry
                .show(&WikiId::parse(id).map_err(CliError::operation)?)
                .map_err(CliError::operation)?
                .registration,
        )
    } else {
        crate::registered_directory_for_path(&cli.vault_root()?)?
    };
    if let Some(registration) = registration {
        if !registration.capabilities().knowledge_services {
            return Err(CliError::operation(format!(
                "registered directory `{}` uses the files-only profile; this sync operation requires knowledge services. Change it with `vulcan vault set {} --profile knowledge`",
                registration.id, registration.id
            )));
        }
    }
    Ok(())
}

fn handle_sync_clone(cli: &Cli, command: &SyncCommand) -> Result<(), CliError> {
    let SyncCommand::Clone {
        remote,
        path,
        id,
        profile,
        group,
        git_dir,
        platform,
        permissions_profile,
        no_device_key,
        login,
        dry_run,
    } = command
    else {
        unreachable!("sync clone handler requires a clone command")
    };
    let id = id.as_deref().map_or_else(
        || {
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    CliError::operation("cannot derive a wiki ID from the destination; pass --id")
                })
                .and_then(|value| WikiId::parse(value).map_err(CliError::operation))
        },
        |value| WikiId::parse(value).map_err(CliError::operation),
    )?;
    let platform = resolve_sync_clone_platform(
        *platform,
        cfg!(target_os = "android"),
        std::env::var_os("PREFIX").is_some(),
    );
    let default_git_dir;
    let git_dir = if platform == GitPlatformProfile::AndroidShared && git_dir.is_none() {
        default_git_dir = vulcan_user_data_dir()
            .ok_or_else(|| CliError::operation("Vulcan user data directory is unavailable"))?
            .join("git")
            .join(format!("{}.git", id.as_str()));
        Some(default_git_dir.as_path())
    } else {
        git_dir.as_deref()
    };
    let registry = WikiRegistry::user_default().map_err(CliError::operation)?;
    crate::commands::vault::clone_wiki(
        cli,
        &registry,
        crate::commands::vault::CloneCliRequest {
            id,
            profile: crate::commands::vault::managed_profile(*profile),
            remote,
            path,
            groups: group,
            git_dir,
            platform,
            permissions_profile: permissions_profile.as_deref(),
            no_device_key: *no_device_key,
            login: *login,
            dry_run: *dry_run,
        },
    )
}

fn resolve_sync_clone_platform(
    explicit: Option<crate::ClonePlatformArg>,
    target_is_android: bool,
    termux_prefix_present: bool,
) -> GitPlatformProfile {
    match explicit {
        Some(crate::ClonePlatformArg::AndroidShared) => GitPlatformProfile::AndroidShared,
        None if target_is_android && termux_prefix_present => GitPlatformProfile::AndroidShared,
        None | Some(crate::ClonePlatformArg::Native) => GitPlatformProfile::native(),
    }
}

struct CliSyncProgress {
    max_attempts: usize,
    mode: CliSyncProgressMode,
    transient_active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliSyncProgressMode {
    Silent,
    Transient,
    Verbose,
}

impl CliSyncProgress {
    fn new(max_attempts: usize, mode: CliSyncProgressMode) -> Self {
        Self {
            max_attempts,
            mode,
            transient_active: false,
        }
    }

    fn finish(&mut self) {
        if self.transient_active {
            let mut stderr = io::stderr().lock();
            let _ = write!(stderr, "\r\x1b[2K");
            let _ = stderr.flush();
            self.transient_active = false;
        }
    }
}

impl GitSyncObserver for CliSyncProgress {
    fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
        match self.mode {
            CliSyncProgressMode::Silent => {}
            CliSyncProgressMode::Transient => {
                let mut stderr = io::stderr().lock();
                if progress.attempt == 0 {
                    let _ = write!(
                        stderr,
                        "\r\x1b[2KSync: {}",
                        sync_phase_message(progress.phase)
                    );
                } else {
                    let _ = write!(
                        stderr,
                        "\r\x1b[2KSync retry {}/{}: {}",
                        progress.attempt + 1,
                        self.max_attempts,
                        sync_phase_message(progress.phase)
                    );
                }
                let _ = stderr.flush();
                self.transient_active = true;
            }
            CliSyncProgressMode::Verbose => {
                let mut stderr = io::stderr().lock();
                let _ = writeln!(
                    stderr,
                    "Sync attempt {}/{}: {}",
                    progress.attempt + 1,
                    self.max_attempts,
                    sync_phase_message(progress.phase)
                );
            }
        }
        Ok(())
    }
}

fn progress_mode(
    output: OutputFormat,
    quiet: bool,
    verbose: bool,
    stderr_is_terminal: bool,
) -> CliSyncProgressMode {
    if quiet || output != OutputFormat::Human {
        CliSyncProgressMode::Silent
    } else if verbose {
        CliSyncProgressMode::Verbose
    } else if stderr_is_terminal {
        CliSyncProgressMode::Transient
    } else {
        CliSyncProgressMode::Silent
    }
}

fn sync_phase_message(phase: GitSyncPhase) -> &'static str {
    match phase {
        GitSyncPhase::Preparing => "preparing repository",
        GitSyncPhase::Capturing => "capturing local worktree",
        GitSyncPhase::Captured => "local snapshot captured",
        GitSyncPhase::BackingUp => "publishing device safety backup",
        GitSyncPhase::Fetching => "querying remote",
        GitSyncPhase::Fetched => "remote revision ready",
        GitSyncPhase::Merging => "merging revisions",
        GitSyncPhase::Pushing => "publishing with exact lease",
        GitSyncPhase::Applying => "applying accepted tree",
        GitSyncPhase::Verifying => "verifying current worktree",
        GitSyncPhase::Paused => "paused with recovery state preserved",
        GitSyncPhase::Conflicted => "conflict preserved for review",
        GitSyncPhase::Completed => "cycle complete",
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn sync_phase_messages_are_specific_and_nonempty() {
        let phases = [
            GitSyncPhase::Preparing,
            GitSyncPhase::Capturing,
            GitSyncPhase::Captured,
            GitSyncPhase::BackingUp,
            GitSyncPhase::Fetching,
            GitSyncPhase::Fetched,
            GitSyncPhase::Merging,
            GitSyncPhase::Pushing,
            GitSyncPhase::Applying,
            GitSyncPhase::Verifying,
            GitSyncPhase::Paused,
            GitSyncPhase::Conflicted,
            GitSyncPhase::Completed,
        ];
        let messages = phases.map(sync_phase_message);

        assert!(messages.iter().all(|message| !message.is_empty()));
        assert_eq!(
            messages
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            phases.len()
        );
    }

    #[test]
    fn progress_is_transient_only_for_normal_interactive_human_output() {
        assert_eq!(
            progress_mode(OutputFormat::Human, false, false, true),
            CliSyncProgressMode::Transient
        );
        assert_eq!(
            progress_mode(OutputFormat::Human, false, true, true),
            CliSyncProgressMode::Verbose
        );
        for mode in [
            progress_mode(OutputFormat::Human, true, true, true),
            progress_mode(OutputFormat::Human, false, false, false),
            progress_mode(OutputFormat::Json, false, true, true),
            progress_mode(OutputFormat::Markdown, false, true, true),
        ] {
            assert_eq!(mode, CliSyncProgressMode::Silent);
        }
    }

    #[test]
    fn sync_clone_selects_android_only_inside_termux() {
        assert_eq!(
            resolve_sync_clone_platform(None, true, true),
            GitPlatformProfile::AndroidShared
        );
        assert_eq!(
            resolve_sync_clone_platform(None, true, false),
            GitPlatformProfile::native()
        );
        assert_eq!(
            resolve_sync_clone_platform(None, false, true),
            GitPlatformProfile::native()
        );
        assert_eq!(
            resolve_sync_clone_platform(Some(crate::ClonePlatformArg::AndroidShared), false, false),
            GitPlatformProfile::AndroidShared
        );
        assert_eq!(
            resolve_sync_clone_platform(Some(crate::ClonePlatformArg::Native), true, true),
            GitPlatformProfile::native()
        );
    }
}

#[allow(clippy::too_many_lines)]
fn handle_non_cycle_sync_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Option<Result<(), CliError>> {
    if let Some(result) = handle_retention_command(cli, paths, command) {
        return Some(result);
    }
    if let Some(result) = handle_semantic_sync_command(cli, paths, command) {
        return Some(result);
    }
    if let Some(result) = handle_notification_sync_command(cli, paths, command) {
        return Some(result);
    }
    if let Some(result) = handle_termux_sync_command(cli, command) {
        return Some(result);
    }
    if let SyncCommand::Devices { command } = command {
        return Some(handle_sync_devices(cli, paths, command));
    }
    if let SyncCommand::Transport { command } = command {
        return Some(handle_sync_transport(cli, paths, command));
    }
    if let SyncCommand::Forge { command } = command {
        return Some(handle_sync_forge(cli, paths, command));
    }
    let result = match command {
        SyncCommand::Pause { wiki, dry_run } => {
            set_automatic_sync(cli.output, paths, wiki.as_deref(), true, *dry_run)
        }
        SyncCommand::Resume { wiki, dry_run } => {
            set_automatic_sync(cli.output, paths, wiki.as_deref(), false, *dry_run)
        }
        SyncCommand::Advertise { .. }
        | SyncCommand::Unadvertise { .. }
        | SyncCommand::Notifications { .. } => {
            unreachable!("notification commands are dispatched before the general sync match")
        }
        SyncCommand::Doctor {
            wiki,
            target,
            profile,
        } => run_sync_doctor(cli, paths, wiki.as_deref(), target, *profile),
        command @ SyncCommand::Conflicts { .. } => {
            handle_sync_conflicts_command(cli, paths, command)
        }
        SyncCommand::ConflictsArchive {
            wiki,
            older_than_days,
            dry_run,
        } => run_sync_conflicts_archive(cli, paths, wiki.as_deref(), *older_than_days, *dry_run),
        SyncCommand::Propose {
            conflict_id,
            wiki,
            groups,
            target,
            base_url,
            model,
            api_key_env,
            context,
            allow_broad_context,
            auto_accept,
        } => run_sync_propose(
            cli,
            paths,
            wiki.as_deref(),
            conflict_id,
            groups,
            base_url,
            model,
            api_key_env.as_deref(),
            context,
            *allow_broad_context,
            *auto_accept,
            target,
        ),
        SyncCommand::FormatPropose {
            conflict_id,
            wiki,
            groups,
            target,
            formatter,
            formatter_args,
            formatter_version,
            formatter_config,
            timeout_seconds,
            dry_run,
        } => run_sync_format_propose(
            cli,
            paths,
            wiki.as_deref(),
            conflict_id,
            groups,
            target,
            formatter,
            formatter_args,
            formatter_version,
            formatter_config.as_deref(),
            *timeout_seconds,
            *dry_run,
        ),
        SyncCommand::Reject {
            conflict_id,
            proposal_id,
            wiki,
            dry_run,
        } => run_sync_reject(
            cli,
            paths,
            wiki.as_deref(),
            conflict_id,
            proposal_id,
            *dry_run,
        ),
        command @ SyncCommand::Resolve { .. } => handle_sync_resolve_command(cli, paths, command),
        SyncCommand::Checkpoint {
            wiki,
            kind,
            target,
            dry_run,
        } => run_sync_checkpoint(cli, paths, wiki.as_deref(), *kind, target, *dry_run),
        SyncCommand::SemanticPlan { .. }
        | SyncCommand::SemanticApply { .. }
        | SyncCommand::SemanticPublish { .. }
        | SyncCommand::SemanticAuto { .. }
        | SyncCommand::SemanticReject { .. } => {
            unreachable!("semantic commands are dispatched before the general sync match")
        }
        SyncCommand::Clone { .. } => {
            unreachable!("clone is dispatched before the general sync match")
        }
        SyncCommand::Run { .. } | SyncCommand::Status { .. } => return None,
        SyncCommand::Devices { .. } => {
            unreachable!("device commands are dispatched before the general sync match")
        }
        SyncCommand::TermuxInstall { .. }
        | SyncCommand::TermuxUninstall { .. }
        | SyncCommand::Schedule { .. } => {
            unreachable!("Termux commands are dispatched before the general sync match")
        }
        SyncCommand::RetentionPlan { .. } | SyncCommand::RetentionApply { .. } => {
            unreachable!("retention commands are dispatched before the general sync match")
        }
        SyncCommand::Transport { .. } | SyncCommand::Forge { .. } => {
            unreachable!(
                "transport and forge commands are dispatched before the general sync match"
            )
        }
    };
    Some(result)
}

fn handle_notification_sync_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Option<Result<(), CliError>> {
    match command {
        SyncCommand::Advertise {
            wiki,
            subscribe_url_file,
            remote,
            expected,
            sign,
            signing_key,
            dry_run,
        } => Some(run_sync_advertise(
            cli,
            paths,
            wiki.as_deref(),
            subscribe_url_file,
            remote,
            expected.as_deref(),
            *sign,
            signing_key.as_deref(),
            *dry_run,
        )),
        SyncCommand::Unadvertise {
            wiki,
            remote,
            expected,
            dry_run,
        } => Some(run_sync_unadvertise(
            cli,
            paths,
            wiki.as_deref(),
            remote,
            expected.as_deref(),
            *dry_run,
        )),
        SyncCommand::Notifications { wiki, remote } => {
            Some(run_sync_notifications(cli, paths, wiki.as_deref(), remote))
        }
        _ => None,
    }
}

fn handle_termux_sync_command(cli: &Cli, command: &SyncCommand) -> Option<Result<(), CliError>> {
    match command {
        SyncCommand::Schedule { command } => Some(handle_sync_schedule(cli, command)),
        SyncCommand::TermuxInstall {
            wiki,
            period_minutes,
            network,
            charging,
            allow_low_battery,
            no_persist,
            job_id,
            network_notification_mode,
            network_failure_count,
            network_failure_minutes,
            dry_run,
        } => Some(install_termux_sync(
            cli,
            wiki,
            &TermuxSyncInstallOptions {
                period_minutes: *period_minutes,
                network: match network {
                    TermuxNetworkArg::Any => TermuxNetwork::Any,
                    TermuxNetworkArg::Unmetered => TermuxNetwork::Unmetered,
                    TermuxNetworkArg::Cellular => TermuxNetwork::Cellular,
                    TermuxNetworkArg::NotRoaming => TermuxNetwork::NotRoaming,
                },
                battery_not_low: !*allow_low_battery,
                charging: *charging,
                persisted: !*no_persist,
                job_id: *job_id,
                network_notification_mode: map_network_mode(*network_notification_mode),
                network_failure_count: *network_failure_count,
                network_failure_minutes: *network_failure_minutes,
            },
            *dry_run,
        )),
        SyncCommand::TermuxUninstall { wiki, dry_run } => {
            Some(uninstall_termux_sync(cli.output, wiki, *dry_run))
        }
        _ => None,
    }
}

fn handle_sync_schedule(cli: &Cli, command: &SyncScheduleCommand) -> Result<(), CliError> {
    let (SyncScheduleCommand::Show { wiki } | SyncScheduleCommand::Set { wiki, .. }) = command;
    WikiId::parse(wiki).map_err(CliError::operation)?;
    let state_root = vulcan_core::vulcan_user_state_dir()
        .ok_or_else(|| CliError::operation("Vulcan user state directory is unavailable"))?;
    let installed = load_termux_sync_plan(&state_root, wiki)
        .map_err(CliError::operation)?
        .ok_or_else(|| CliError::operation(format!(
            "no managed Termux sync job exists for wiki `{wiki}`; install one with `vulcan sync termux-install {wiki}`"
        )))?;
    match command {
        SyncScheduleCommand::Show { .. } => {
            if cli.output == OutputFormat::Json {
                return print_json(&installed);
            }
            println!(
                "Saved Android sync schedule for `{wiki}` (job {})",
                installed.job_id
            );
            println!("Approximate interval: {} minutes", installed.period_minutes);
            println!("Network: {:?}", installed.network);
            println!("Battery not low: {}", installed.battery_not_low);
            println!("Charging required: {}", installed.charging);
            println!("Persist across reboots: {}", installed.persisted);
            println!(
                "Network failure notices: {:?} ({} failures or {} minutes)",
                installed.network_notification_mode,
                installed.network_failure_count,
                installed.network_failure_minutes
            );
            println!("These are saved settings; inspect Android jobs with `termux-job-scheduler --pending`.");
            Ok(())
        }
        SyncScheduleCommand::Set {
            period_minutes,
            network,
            charging,
            battery_not_low,
            persisted,
            network_notification_mode,
            network_failure_count,
            network_failure_minutes,
            dry_run,
            ..
        } => {
            let update = TermuxSyncUpdate {
                period_minutes: *period_minutes,
                network: network.map(|network| match network {
                    TermuxNetworkArg::Any => TermuxNetwork::Any,
                    TermuxNetworkArg::Unmetered => TermuxNetwork::Unmetered,
                    TermuxNetworkArg::Cellular => TermuxNetwork::Cellular,
                    TermuxNetworkArg::NotRoaming => TermuxNetwork::NotRoaming,
                }),
                charging: *charging,
                battery_not_low: *battery_not_low,
                persisted: *persisted,
                network_notification_mode: network_notification_mode.map(map_network_mode),
                network_failure_count: *network_failure_count,
                network_failure_minutes: *network_failure_minutes,
            };
            install_termux_sync(cli, wiki, &update.apply_to(&installed), *dry_run)
        }
    }
}

fn install_termux_sync(
    cli: &Cli,
    wiki: &str,
    options: &TermuxSyncInstallOptions,
    dry_run: bool,
) -> Result<(), CliError> {
    let id = WikiId::parse(wiki).map_err(CliError::operation)?;
    let status = WikiRegistry::user_default()
        .map_err(CliError::operation)?
        .show(&id)
        .map_err(CliError::operation)?;
    if status.registration.sync_backend.as_deref() != Some("git")
        || status.registration.platform_profile.as_deref() != Some("android_shared")
        || status.registration.git_dir.is_none()
    {
        return Err(CliError::operation(format!(
            "wiki `{wiki}` must be a registered Git wiki with a detached git directory and the android-shared platform profile"
        )));
    }
    let paths = VaultPaths::new(status.registration.path);
    check_sync_permission(
        cli,
        &paths,
        status.registration.permissions_profile.as_deref(),
    )?;
    let state_root = vulcan_core::vulcan_user_state_dir()
        .ok_or_else(|| CliError::operation("Vulcan user state directory is unavailable"))?;
    let executable = std::env::current_exe().map_err(CliError::operation)?;
    let plan = plan_termux_sync(
        TermuxSyncAction::Install,
        wiki,
        &executable,
        &state_root,
        options,
    )
    .map_err(CliError::operation)?;
    let report = apply_termux_sync(plan, dry_run).map_err(CliError::operation)?;
    print_termux_sync_report(cli.output, &report)
}

fn uninstall_termux_sync(output: OutputFormat, wiki: &str, dry_run: bool) -> Result<(), CliError> {
    WikiId::parse(wiki).map_err(CliError::operation)?;
    let state_root = vulcan_core::vulcan_user_state_dir()
        .ok_or_else(|| CliError::operation("Vulcan user state directory is unavailable"))?;
    let executable = std::env::current_exe().map_err(CliError::operation)?;
    let installed = load_termux_sync_plan(&state_root, wiki)
        .map_err(CliError::operation)?
        .ok_or_else(|| {
            CliError::operation(format!(
                "no managed Termux synchronization job exists for wiki `{wiki}`"
            ))
        })?;
    let options = TermuxSyncInstallOptions {
        period_minutes: installed.period_minutes,
        network: installed.network,
        battery_not_low: installed.battery_not_low,
        charging: installed.charging,
        persisted: installed.persisted,
        job_id: Some(installed.job_id),
        network_notification_mode: installed.network_notification_mode,
        network_failure_count: installed.network_failure_count,
        network_failure_minutes: installed.network_failure_minutes,
    };
    let plan = plan_termux_sync(
        TermuxSyncAction::Uninstall,
        wiki,
        &executable,
        &state_root,
        &options,
    )
    .map_err(CliError::operation)?;
    let report = apply_termux_sync(plan, dry_run).map_err(CliError::operation)?;
    print_termux_sync_report(output, &report)
}

fn print_termux_sync_report(
    output: OutputFormat,
    report: &TermuxSyncReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    let action = match report.plan.action {
        TermuxSyncAction::Install => "install",
        TermuxSyncAction::Uninstall => "uninstall",
    };
    if report.dry_run {
        println!(
            "Would {action} Android job {} for wiki `{}`",
            report.plan.job_id, report.plan.wiki_id
        );
    } else {
        println!(
            "Android job {} for wiki `{}` was {action}ed",
            report.plan.job_id, report.plan.wiki_id
        );
    }
    println!("Script: {}", report.plan.script_path.display());
    if report.plan.action == TermuxSyncAction::Install {
        println!(
            "Approximate interval: {} minutes",
            report.plan.period_minutes
        );
        println!("Inspect: vulcan sync schedule show {}", report.plan.wiki_id);
        println!(
            "Change interval: vulcan sync schedule set {} --period-minutes <minutes>",
            report.plan.wiki_id
        );
    }
    Ok(())
}

fn handle_semantic_sync_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Option<Result<(), CliError>> {
    let result = match command {
        SyncCommand::SemanticPlan {
            wiki,
            from,
            to,
            semantic_ref,
            target,
            group_by,
            agent,
            base_url,
            model,
            api_key_env,
            dry_run,
        } => run_semantic_plan(
            cli,
            paths,
            wiki.as_deref(),
            from,
            to,
            semantic_ref,
            target,
            *group_by,
            *agent,
            base_url.as_deref(),
            model.as_deref(),
            api_key_env.as_deref(),
            *dry_run,
        ),
        SyncCommand::SemanticApply { plan_id, dry_run } => {
            run_semantic_apply(cli, plan_id, *dry_run)
        }
        SyncCommand::SemanticPublish { plan_id, dry_run } => {
            run_semantic_publish(cli, plan_id, *dry_run)
        }
        SyncCommand::SemanticAuto {
            wiki,
            semantic_ref,
            target,
            group_by,
            agent,
            base_url,
            model,
            api_key_env,
            quiet_seconds,
            maximum_wait_seconds,
            no_publish,
            dry_run,
        } => run_semantic_auto_command(
            cli,
            paths,
            wiki.as_deref(),
            semantic_ref,
            target,
            *group_by,
            *agent,
            base_url.as_deref(),
            model.as_deref(),
            api_key_env.as_deref(),
            *quiet_seconds,
            *maximum_wait_seconds,
            !*no_publish,
            *dry_run,
        ),
        SyncCommand::SemanticReject { plan_id, dry_run } => {
            run_semantic_reject(cli, plan_id, *dry_run)
        }
        _ => return None,
    };
    Some(result)
}

fn handle_retention_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Option<Result<(), CliError>> {
    match command {
        SyncCommand::RetentionPlan {
            wiki,
            target,
            live_epoch_max_commits,
            recovery_checkpoints_keep,
            epoch_archives_keep,
        } => Some(run_sync_retention_plan(
            cli,
            paths,
            wiki.as_deref(),
            target,
            *live_epoch_max_commits,
            *recovery_checkpoints_keep,
            *epoch_archives_keep,
        )),
        SyncCommand::RetentionApply {
            wiki,
            target,
            live_epoch_max_commits,
            recovery_checkpoints_keep,
            epoch_archives_keep,
            dry_run,
            rollover,
            expire_epoch_archives,
        } => Some(run_sync_retention_apply(
            cli,
            paths,
            wiki.as_deref(),
            target,
            *live_epoch_max_commits,
            *recovery_checkpoints_keep,
            *epoch_archives_keep,
            *dry_run,
            *rollover,
            *expire_epoch_archives,
        )),
        _ => None,
    }
}

fn handle_sync_conflicts_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Result<(), CliError> {
    let SyncCommand::Conflicts {
        conflict_id,
        path_offset,
        path_limit,
        wiki,
    } = command
    else {
        unreachable!("conflicts handler receives a conflicts command")
    };
    run_sync_conflicts(
        cli,
        paths,
        wiki.as_deref(),
        conflict_id.as_deref(),
        *path_offset,
        *path_limit,
    )
}

fn handle_sync_resolve_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Result<(), CliError> {
    let SyncCommand::Resolve {
        conflict_id,
        side,
        approve_proposal,
        files,
        patch,
        editor,
        groups,
        wiki,
        target,
        dry_run,
    } = command
    else {
        unreachable!("resolve handler receives a resolve command")
    };
    run_sync_resolve(
        cli,
        paths,
        wiki.as_deref(),
        conflict_id,
        cli_resolution(
            *side,
            approve_proposal.as_deref(),
            files,
            patch.as_deref(),
            *editor,
        ),
        groups,
        target,
        *dry_run,
    )
}

fn run_sync_reject(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    conflict_id: &str,
    proposal_id: &str,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let report = reject_resolution_proposal(&paths, conflict_id, proposal_id, dry_run)
        .map_err(CliError::operation)?;
    print_rejection_report(cli.output, &report)
}

fn print_rejection_report(
    output: OutputFormat,
    report: &RejectResolutionProposalReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Resolution proposal {}: {:?}",
        report.proposal_id, report.outcome
    );
    Ok(())
}

#[cfg(feature = "web")]
#[allow(clippy::too_many_arguments)]
fn run_sync_propose(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    conflict_id: &str,
    groups: &[String],
    base_url: &str,
    model: &str,
    api_key_env: Option<&str>,
    context: &[String],
    allow_broad_context: bool,
    auto_accept: bool,
    target: &crate::SyncTargetArgs,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    let profile = cli
        .permissions
        .as_deref()
        .or(registration_profile.as_deref())
        .unwrap_or("unrestricted");
    let selection =
        resolve_permission_profile(&paths, Some(profile)).map_err(CliError::operation)?;
    ProfilePermissionGuard::new(&paths, selection)
        .check_network(base_url)
        .map_err(CliError::operation)?;
    let api_key = api_key_env
        .map(|name| {
            std::env::var(name)
                .map_err(|_| CliError::operation(format!("agent API key env `{name}` is not set")))
        })
        .transpose()?;
    let provider = OpenAiCompatibleResolutionProvider::new(base_url, model, api_key)
        .map_err(CliError::operation)?;
    let proposal_options = ResolutionProposalOptions {
        permission_profile: profile.to_string(),
        focused_context: context.to_vec(),
        allow_broad_context,
        group_ids: groups.to_vec(),
    };
    let cancellation = vulcan_app::sync::SyncCancellationToken::default();
    if auto_accept {
        let mut approval_options = approval_options(target, false)?;
        approval_options.automatic = true;
        let report = create_and_auto_accept_resolution_proposal(
            &paths,
            conflict_id,
            &proposal_options,
            &approval_options,
            &provider,
            &cancellation,
        )
        .map_err(CliError::operation)?;
        print_auto_accept_resolution_proposal(cli.output, &report)
    } else {
        let proposal = create_resolution_proposal_for_target(
            &paths,
            conflict_id,
            &proposal_options,
            &GitRemote::parse(&target.remote).map_err(CliError::operation)?,
            &GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
            &provider,
            &cancellation,
        )
        .map_err(CliError::operation)?;
        print_resolution_proposal(cli.output, &proposal)
    }
}

#[allow(clippy::too_many_arguments)]
fn run_sync_format_propose(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    conflict_id: &str,
    groups: &[String],
    target: &crate::SyncTargetArgs,
    formatter: &std::path::Path,
    formatter_args: &[String],
    formatter_version: &str,
    formatter_config: Option<&std::path::Path>,
    timeout_seconds: u64,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let (mut proposal_options, approval_options) =
        manual_resolution_options(cli, registration_profile.as_deref(), target, dry_run)?;
    proposal_options.group_ids = groups.to_vec();
    let report = create_formatter_resolution_proposal(
        &paths,
        conflict_id,
        &proposal_options,
        &approval_options,
        &FormatterResolutionOptions {
            executable: formatter.to_path_buf(),
            arguments: formatter_args.to_vec(),
            expected_version: formatter_version.to_string(),
            config: formatter_config.map(std::path::Path::to_path_buf),
            timeout: Duration::from_secs(timeout_seconds),
        },
        &vulcan_app::sync::SyncCancellationToken::default(),
    )
    .map_err(CliError::operation)?;
    match report {
        FormatterResolutionReport::Preview { report } => {
            print_supplied_resolution_preview(cli.output, &report)
        }
        FormatterResolutionReport::Proposed { proposal } => {
            print_resolution_proposal(cli.output, &proposal)
        }
    }
}

#[cfg(not(feature = "web"))]
#[allow(clippy::too_many_arguments)]
fn run_sync_propose(
    _cli: &Cli,
    _selected_paths: &VaultPaths,
    _wiki: Option<&str>,
    _conflict_id: &str,
    _groups: &[String],
    _base_url: &str,
    _model: &str,
    _api_key_env: Option<&str>,
    _context: &[String],
    _allow_broad_context: bool,
    _auto_accept: bool,
    _target: &crate::SyncTargetArgs,
) -> Result<(), CliError> {
    Err(CliError::operation(
        "sync proposal generation requires the `web` feature",
    ))
}

#[cfg(feature = "web")]
fn print_auto_accept_resolution_proposal(
    output: OutputFormat,
    report: &AutoAcceptResolutionProposalReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Resolution proposal {} was auto-accepted and {:?} for conflict {}.",
        report.proposal.proposal_id, report.approval.outcome, report.proposal.conflict_id
    );
    Ok(())
}

fn print_resolution_proposal(
    output: OutputFormat,
    proposal: &ResolutionProposal,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(proposal);
    }
    println!(
        "Resolution proposal {} is ready for conflict {} ({} path(s)).",
        proposal.proposal_id,
        proposal.conflict_id,
        proposal.paths.len()
    );
    println!(
        "Preview approval with: vulcan sync resolve {} --approve-proposal {} --dry-run",
        proposal.conflict_id, proposal.proposal_id
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_semantic_plan(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    from: &str,
    to: &str,
    semantic_ref: &str,
    target: &crate::SyncTargetArgs,
    group_by: SemanticGroupingArg,
    agent: bool,
    base_url: Option<&str>,
    model: Option<&str>,
    api_key_env: Option<&str>,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let options = SemanticPlanOptions {
        from: from.to_string(),
        to: to.to_string(),
        semantic_ref: GitRefName::parse(semantic_ref).map_err(CliError::operation)?,
        remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
        live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
        grouping: match group_by {
            SemanticGroupingArg::TopLevel => SemanticGrouping::TopLevel,
            SemanticGroupingArg::File => SemanticGrouping::File,
            SemanticGroupingArg::Change => SemanticGrouping::Change,
            SemanticGroupingArg::Hunk => SemanticGrouping::Hunk,
            SemanticGroupingArg::All => SemanticGrouping::All,
        },
        agent,
        dry_run,
    };
    let report = if agent {
        create_agent_semantic_plan(
            cli,
            &paths,
            registration_profile.as_deref(),
            &options,
            base_url,
            model,
            api_key_env,
        )?
    } else {
        if model.is_some() || api_key_env.is_some() {
            return Err(CliError::operation(
                "--model and --api-key-env require --agent",
            ));
        }
        create_semantic_plan(&paths, &options).map_err(CliError::operation)?
    };
    print_semantic_plan(cli.output, &report)
}

#[cfg(feature = "web")]
fn create_agent_semantic_plan(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
    options: &SemanticPlanOptions,
    base_url: Option<&str>,
    model: Option<&str>,
    api_key_env: Option<&str>,
) -> Result<SemanticPlanReport, CliError> {
    let agent = resolve_semantic_agent(base_url, model, api_key_env)?;
    let (base_url, model, api_key_env) = (
        agent.base_url.as_str(),
        agent.model.as_str(),
        agent.api_key_env.as_deref(),
    );
    let profile = cli
        .permissions
        .as_deref()
        .or(registration_profile)
        .unwrap_or("unrestricted");
    let selection =
        resolve_permission_profile(paths, Some(profile)).map_err(CliError::operation)?;
    ProfilePermissionGuard::new(paths, selection)
        .check_network(base_url)
        .map_err(CliError::operation)?;
    let api_key = api_key_env
        .map(|name| {
            std::env::var(name).map_err(|_| {
                CliError::operation(format!("semantic agent API key env `{name}` is not set"))
            })
        })
        .transpose()?;
    let provider = OpenAiCompatibleSemanticProvider::new(base_url, model, api_key)
        .map_err(CliError::operation)?;
    create_semantic_plan_with_provider(
        paths,
        options,
        &provider,
        &vulcan_app::sync::SyncCancellationToken::default(),
    )
    .map_err(CliError::operation)
}

#[cfg(feature = "web")]
const DEFAULT_SEMANTIC_AGENT_BASE_URL: &str = "http://localhost:11434/v1";

#[cfg(feature = "web")]
/// The provider a direct `--agent` semantic command calls.
#[derive(Debug, PartialEq, Eq)]
struct SemanticAgentSelection {
    base_url: String,
    model: String,
    api_key_env: Option<String>,
}

/// Selects the semantic provider from explicit flags and, unless `--base-url`
/// names another endpoint, the daemon's configured semantic agent.
#[cfg(feature = "web")]
fn resolve_semantic_agent(
    base_url: Option<&str>,
    model: Option<&str>,
    api_key_env: Option<&str>,
) -> Result<SemanticAgentSelection, CliError> {
    let configured = match base_url {
        Some(_) => None,
        None => {
            WikiRegistry::user_default()
                .and_then(|registry| registry.load())
                .map_err(CliError::operation)?
                .semantic_agent
        }
    };
    select_semantic_agent(base_url, model, api_key_env, configured.as_ref())
}

#[cfg(feature = "web")]
/// An explicit `--base-url` never inherits the configured key variable, so a
/// configured credential cannot be sent to a different endpoint.
fn select_semantic_agent(
    base_url: Option<&str>,
    model: Option<&str>,
    api_key_env: Option<&str>,
    configured: Option<&vulcan_daemon::registry::DaemonAgentConfig>,
) -> Result<SemanticAgentSelection, CliError> {
    let configured = configured.filter(|_| base_url.is_none());
    let model = model
        .map(str::to_string)
        .or_else(|| configured.map(|agent| agent.model.clone()))
        .ok_or_else(|| {
            CliError::operation(
                "--agent requires --model or a semantic agent configured with `vulcan daemon config set-agent semantic`",
            )
        })?;
    Ok(SemanticAgentSelection {
        base_url: base_url
            .map(str::to_string)
            .or_else(|| configured.map(|agent| agent.base_url.clone()))
            .unwrap_or_else(|| DEFAULT_SEMANTIC_AGENT_BASE_URL.to_string()),
        model,
        api_key_env: api_key_env
            .map(str::to_string)
            .or_else(|| configured.and_then(|agent| agent.api_key_env.clone())),
    })
}

#[cfg(not(feature = "web"))]
fn create_agent_semantic_plan(
    _cli: &Cli,
    _paths: &VaultPaths,
    _registration_profile: Option<&str>,
    _options: &SemanticPlanOptions,
    _base_url: Option<&str>,
    _model: Option<&str>,
    _api_key_env: Option<&str>,
) -> Result<SemanticPlanReport, CliError> {
    Err(CliError::operation(
        "agent-assisted semantic planning requires the `web` feature",
    ))
}

fn run_semantic_apply(cli: &Cli, plan_id: &str, dry_run: bool) -> Result<(), CliError> {
    let plan = load_semantic_plan(plan_id).map_err(CliError::operation)?;
    let paths = VaultPaths::new(&plan.vault);
    selected_permission_guard(cli, &paths)?
        .check_git()
        .map_err(CliError::operation)?;
    let report = apply_semantic_plan(plan_id, dry_run).map_err(CliError::operation)?;
    print_semantic_apply(cli.output, &report)
}

fn print_semantic_plan(output: OutputFormat, report: &SemanticPlanReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Semantic plan {}: {:?} ({} commit(s))",
        report.plan_id,
        report.status,
        report.commits.len()
    );
    for commit in &report.commits {
        println!(
            "{}. {} [{} path(s)]",
            commit.position,
            commit.group,
            commit.paths.len()
        );
    }
    Ok(())
}

fn print_semantic_apply(
    output: OutputFormat,
    report: &SemanticApplyReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Semantic plan {}: {} -> {}{}",
        report.plan_id,
        report.previous_revision,
        report.applied_revision,
        if report.dry_run { " (dry run)" } else { "" }
    );
    Ok(())
}

fn run_semantic_publish(cli: &Cli, plan_id: &str, dry_run: bool) -> Result<(), CliError> {
    let plan = load_semantic_plan(plan_id).map_err(CliError::operation)?;
    let paths = VaultPaths::new(&plan.vault);
    selected_permission_guard(cli, &paths)?
        .check_git()
        .map_err(CliError::operation)?;
    let report = publish_semantic_plan(plan_id, dry_run).map_err(CliError::operation)?;
    print_semantic_publish(cli.output, &report)
}

fn print_semantic_publish(
    output: OutputFormat,
    report: &SemanticPublishReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Semantic plan {}: published {} to {}/{}{}{}",
        report.plan_id,
        report.published_revision,
        report.remote,
        report.semantic_ref,
        if report.dry_run { " (dry run)" } else { "" },
        if report.already_published {
            " (already published)"
        } else {
            ""
        }
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_semantic_auto_command(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    semantic_ref: &str,
    target: &crate::SyncTargetArgs,
    group_by: SemanticGroupingArg,
    agent: bool,
    base_url: Option<&str>,
    model: Option<&str>,
    api_key_env: Option<&str>,
    quiet_seconds: u64,
    maximum_wait_seconds: u64,
    publish: bool,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let options = SemanticAutoOptions {
        semantic_ref: GitRefName::parse(semantic_ref).map_err(CliError::operation)?,
        remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
        live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
        grouping: semantic_grouping(group_by),
        agent,
        publish,
        quiet_seconds,
        maximum_wait_seconds,
        dry_run,
    };
    let store = SyncStateStore::user_default().map_err(CliError::operation)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(CliError::operation)?
        .as_millis()
        .try_into()
        .map_err(CliError::operation)?;
    let report = run_semantic_auto_with_optional_provider(
        cli,
        &paths,
        registration_profile.as_deref(),
        &options,
        base_url,
        model,
        api_key_env,
        &store,
        now,
    )?;
    print_semantic_auto(cli.output, &report)
}

fn semantic_grouping(grouping: SemanticGroupingArg) -> SemanticGrouping {
    match grouping {
        SemanticGroupingArg::TopLevel => SemanticGrouping::TopLevel,
        SemanticGroupingArg::File => SemanticGrouping::File,
        SemanticGroupingArg::Change => SemanticGrouping::Change,
        SemanticGroupingArg::Hunk => SemanticGrouping::Hunk,
        SemanticGroupingArg::All => SemanticGrouping::All,
    }
}

#[cfg(feature = "web")]
#[allow(clippy::too_many_arguments)]
fn run_semantic_auto_with_optional_provider(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
    options: &SemanticAutoOptions,
    base_url: Option<&str>,
    model: Option<&str>,
    api_key_env: Option<&str>,
    store: &SyncStateStore,
    now: u64,
) -> Result<SemanticAutoReport, CliError> {
    if !options.agent {
        if model.is_some() || api_key_env.is_some() {
            return Err(CliError::operation(
                "--model and --api-key-env require --agent",
            ));
        }
        return run_semantic_auto(
            paths,
            options,
            None,
            &vulcan_app::sync::SyncCancellationToken::default(),
            store,
            now,
        )
        .map_err(CliError::operation);
    }
    let agent = resolve_semantic_agent(base_url, model, api_key_env)?;
    let (base_url, model, api_key_env) = (
        agent.base_url.as_str(),
        agent.model.as_str(),
        agent.api_key_env.as_deref(),
    );
    let profile = cli
        .permissions
        .as_deref()
        .or(registration_profile)
        .unwrap_or("unrestricted");
    let selection =
        resolve_permission_profile(paths, Some(profile)).map_err(CliError::operation)?;
    ProfilePermissionGuard::new(paths, selection)
        .check_network(base_url)
        .map_err(CliError::operation)?;
    let api_key = api_key_env
        .map(|name| {
            std::env::var(name).map_err(|_| {
                CliError::operation(format!("semantic agent API key env `{name}` is not set"))
            })
        })
        .transpose()?;
    let provider = OpenAiCompatibleSemanticProvider::new(base_url, model, api_key)
        .map_err(CliError::operation)?;
    run_semantic_auto(
        paths,
        options,
        Some(&provider),
        &vulcan_app::sync::SyncCancellationToken::default(),
        store,
        now,
    )
    .map_err(CliError::operation)
}

#[cfg(not(feature = "web"))]
#[allow(clippy::too_many_arguments)]
fn run_semantic_auto_with_optional_provider(
    _cli: &Cli,
    paths: &VaultPaths,
    _registration_profile: Option<&str>,
    options: &SemanticAutoOptions,
    _base_url: Option<&str>,
    _model: Option<&str>,
    _api_key_env: Option<&str>,
    store: &SyncStateStore,
    now: u64,
) -> Result<SemanticAutoReport, CliError> {
    if options.agent {
        return Err(CliError::operation(
            "agent-assisted semantic automation requires the `web` feature",
        ));
    }
    run_semantic_auto(
        paths,
        options,
        None,
        &vulcan_app::sync::SyncCancellationToken::default(),
        store,
        now,
    )
    .map_err(CliError::operation)
}

fn print_semantic_auto(output: OutputFormat, report: &SemanticAutoReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Semantic automation: {:?} ({} -> {}, stable {}s)",
        report.outcome, report.source_revision, report.target_revision, report.stable_for_seconds
    );
    Ok(())
}

fn run_semantic_reject(cli: &Cli, plan_id: &str, dry_run: bool) -> Result<(), CliError> {
    let plan = load_semantic_plan(plan_id).map_err(CliError::operation)?;
    let paths = VaultPaths::new(&plan.vault);
    selected_permission_guard(cli, &paths)?
        .check_git()
        .map_err(CliError::operation)?;
    let report = reject_semantic_plan(plan_id, dry_run).map_err(CliError::operation)?;
    print_semantic_reject(cli.output, &report)
}

fn print_semantic_reject(
    output: OutputFormat,
    report: &SemanticRejectReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Semantic plan {}: {:?} (proposal ref {}).",
        report.plan_id, report.outcome, report.proposal_ref
    );
    Ok(())
}

fn run_sync_checkpoint(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    kind: SyncCheckpointKindArg,
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let report = create_sync_checkpoint(
        &paths,
        &SyncCheckpointOptions {
            kind: match kind {
                SyncCheckpointKindArg::Recovery => SyncCheckpointKind::Recovery,
                SyncCheckpointKindArg::Semantic => SyncCheckpointKind::Semantic,
            },
            remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
            live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    print_sync_checkpoint(cli.output, &report)
}

fn print_sync_checkpoint(
    output: OutputFormat,
    report: &SyncCheckpointReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Sync checkpoint: {} -> {} ({:?})",
        report.checkpoint_ref, report.revision, report.kind
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_sync_advertise(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    subscribe_url_file: &std::path::Path,
    remote: &str,
    expected: Option<&str>,
    sign: bool,
    signing_key: Option<&str>,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let subscribe_url = read_subscribe_url(subscribe_url_file)?;
    let report = publish_sync_notification_advertisement(
        &paths,
        &SyncNotificationPublishOptions {
            subscribe_url,
            remote: GitRemote::parse(remote).map_err(CliError::operation)?,
            expected: expected.map(str::to_string),
            sign,
            signing_key: signing_key.map(str::to_string),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    print_sync_advertise(cli.output, &report)
}

const MAX_SUBSCRIBE_URL_INPUT_BYTES: usize = 4 * 1024;

fn read_subscribe_url(source: &std::path::Path) -> Result<String, CliError> {
    let mut bytes = Vec::new();
    if source == std::path::Path::new("-") {
        if io::stdin().is_terminal() {
            return Err(CliError::operation(
                "--subscribe-url-file - requires piped stdin; interactive secret entry is not supported",
            ));
        }
        io::stdin()
            .lock()
            .take((MAX_SUBSCRIBE_URL_INPUT_BYTES + 3) as u64)
            .read_to_end(&mut bytes)
            .map_err(CliError::operation)?;
    } else {
        let file = open_subscribe_url_file(source)?;
        let metadata = file.metadata().map_err(CliError::operation)?;
        if !metadata.is_file() {
            return Err(CliError::operation(
                "the notification subscribe URL source must be a regular file",
            ));
        }
        require_private_subscribe_url_file(&metadata)?;
        file.take((MAX_SUBSCRIBE_URL_INPUT_BYTES + 3) as u64)
            .read_to_end(&mut bytes)
            .map_err(CliError::operation)?;
    }
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.pop();
    }
    if bytes.len() > MAX_SUBSCRIBE_URL_INPUT_BYTES {
        return Err(CliError::operation(
            "notification subscribe URL input exceeds the 4096-byte limit",
        ));
    }
    let value = String::from_utf8(bytes)
        .map_err(|_| CliError::operation("notification subscribe URL input is not valid UTF-8"))?;
    if value.contains(['\r', '\n']) {
        return Err(CliError::operation(
            "notification subscribe URL input must contain exactly one line",
        ));
    }
    Ok(value)
}

#[cfg(unix)]
fn open_subscribe_url_file(source: &std::path::Path) -> Result<std::fs::File, CliError> {
    let absolute = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(CliError::operation)?
            .join(source)
    };
    let relative = absolute
        .strip_prefix(std::path::Path::new("/"))
        .map_err(|_| {
            CliError::operation("notification subscribe URL path is not an absolute Unix path")
        })?;
    vulcan_core::paths::secure_open_read(std::path::Path::new("/"), relative)
        .map_err(CliError::operation)
}

#[cfg(not(unix))]
fn open_subscribe_url_file(_source: &std::path::Path) -> Result<std::fs::File, CliError> {
    Err(CliError::operation(
        "notification subscribe URL files require Unix privacy modes; use --subscribe-url-file - with piped stdin on this platform",
    ))
}

#[cfg(unix)]
fn require_private_subscribe_url_file(metadata: &std::fs::Metadata) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(CliError::operation(
            "notification subscribe URL files must not be accessible by group or other users",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_private_subscribe_url_file(_metadata: &std::fs::Metadata) -> Result<(), CliError> {
    unreachable!("non-Unix file inputs are rejected before metadata validation")
}

fn print_sync_advertise(
    output: OutputFormat,
    report: &SyncNotificationPublishReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.dry_run {
        println!(
            "Would advertise {} ({}) on {} {}; currently {}.",
            report.origin,
            report.fingerprint,
            report.remote.as_str(),
            report.advertisement_ref,
            report
                .previous_revision
                .as_deref()
                .map_or("nothing is advertised".to_string(), |revision| format!(
                    "revision {revision} is advertised"
                )),
        );
        return Ok(());
    }
    println!(
        "Advertised {} ({}) on {} {} at revision {}.",
        report.origin,
        report.fingerprint,
        report.remote.as_str(),
        report.advertisement_ref,
        report.revision.as_deref().unwrap_or("unknown"),
    );
    if report.signed {
        println!("The advertisement commit is signed.");
    }
    if let Some(previous) = &report.previous_revision {
        println!("Replaced revision {previous}.");
    }
    Ok(())
}

fn run_sync_unadvertise(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    remote: &str,
    expected: Option<&str>,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let report = remove_sync_notification_advertisement(
        &paths,
        &SyncNotificationRemoveOptions {
            remote: GitRemote::parse(remote).map_err(CliError::operation)?,
            expected: expected.map(str::to_string),
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    if !report.dry_run && !report.deleted && report.previous_revision.is_some() {
        return Err(CliError::issues(format!(
            "Notification advertisement {} on {} changed while it was being removed; retry with --expected {}",
            report.advertisement_ref,
            report.remote.as_str(),
            report.previous_revision.as_deref().unwrap_or("unknown"),
        )));
    }
    print_sync_unadvertise(cli.output, &report)
}

fn print_sync_unadvertise(
    output: OutputFormat,
    report: &SyncNotificationRemoveReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.dry_run {
        println!(
            "Would remove {} on {}; currently {}.",
            report.advertisement_ref,
            report.remote.as_str(),
            report
                .previous_revision
                .as_deref()
                .map_or("nothing is advertised".to_string(), |revision| format!(
                    "revision {revision} is advertised"
                )),
        );
        return Ok(());
    }
    if report.deleted {
        println!(
            "Removed {} on {} (was revision {}).",
            report.advertisement_ref,
            report.remote.as_str(),
            report.previous_revision.as_deref().unwrap_or("unknown"),
        );
    } else {
        println!(
            "No notification advertisement on {}; nothing removed.",
            report.remote.as_str(),
        );
    }
    Ok(())
}

fn run_sync_notifications(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    remote: &str,
) -> Result<(), CliError> {
    let (paths, registration_profile, paused) = resolve_notification_paths(selected_paths, wiki)?;
    let daemon_running = DaemonProcessContext::user_default()
        .ok()
        .and_then(|context| daemon_status(&context).ok())
        .is_some_and(|status| status.running);
    let report = notification_status(
        &paths,
        &SyncNotificationStatusOptions {
            remote: GitRemote::parse(remote).map_err(CliError::operation)?,
            permissions_profile: cli
                .permissions
                .as_deref()
                .or(registration_profile.as_deref())
                .map(str::to_string),
            paused,
            git_backend: wiki.map(|_| true),
            daemon_running,
        },
    )
    .map_err(CliError::operation)?;
    print_sync_notifications(cli.output, &report)
}

/// Resolves the vault, effective permission profile, and pause state for a
/// notification inspection. Unlike mutating commands, a denied Git capability
/// is a report finding rather than an error, so no permission gate applies.
fn resolve_notification_paths(
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
) -> Result<(VaultPaths, Option<String>, Option<bool>), CliError> {
    let Some(wiki) = wiki else {
        return Ok((selected_paths.clone(), None, None));
    };
    let id = WikiId::parse(wiki).map_err(CliError::operation)?;
    let registration = WikiRegistry::user_default()
        .map_err(CliError::operation)?
        .show(&id)
        .map_err(CliError::operation)?
        .registration;
    if registration
        .sync_backend
        .as_deref()
        .is_some_and(|backend| backend != "git")
    {
        return Err(CliError::operation(format!(
            "wiki `{id}` uses unsupported sync backend `{}`",
            registration.sync_backend.as_deref().unwrap_or_default()
        )));
    }
    Ok((
        VaultPaths::new(registration.path),
        registration.permissions_profile,
        Some(registration.sync_paused),
    ))
}

fn print_sync_notifications(
    output: OutputFormat,
    report: &SyncNotificationStatusReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.would_listen {
        println!(
            "Would use notification server {} ({}) at revision {} via {}.",
            report.origin.as_deref().unwrap_or("unknown"),
            report.fingerprint.as_deref().unwrap_or("unknown"),
            report.revision.as_deref().unwrap_or("unknown"),
            report.remote.as_str(),
        );
        return Ok(());
    }
    println!(
        "Would not use a notification server: {}.",
        if report.reasons.is_empty() {
            "unknown reason".to_string()
        } else {
            report.reasons.join(", ")
        },
    );
    println!("Detail: {}", report.detail);
    Ok(())
}

fn run_sync_retention_plan(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    target: &crate::SyncTargetArgs,
    live_epoch_max_commits: usize,
    recovery_checkpoints_keep: usize,
    epoch_archives_keep: usize,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let report = plan_sync_retention(
        &paths,
        &SyncRetentionPlanOptions {
            remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
            live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
            policy: SyncRetentionPolicy {
                live_epoch_max_commits,
                recovery_checkpoints_keep,
                epoch_archives_keep,
            },
        },
    )
    .map_err(CliError::operation)?;
    print_sync_retention_plan(cli.output, &report)
}

fn print_sync_retention_plan(
    output: OutputFormat,
    report: &SyncRetentionPlanReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Active live epoch: {} observed commit(s), rollover {}.",
        report.active_epoch.observed_commits,
        if report.active_epoch.rollover_required {
            "required"
        } else {
            "not required"
        }
    );
    println!(
        "Recovery checkpoints: {} retained, {} expirable; semantic checkpoints remain permanent.",
        report.recovery_checkpoints.retained.len(),
        report.recovery_checkpoints.expirable.len()
    );
    println!(
        "Epoch archives: {} retained, {} expirable; chain {}.",
        report.epoch_archives.retained.len(),
        report.epoch_archives.expirable.len(),
        if report.epoch_archives.chain_complete {
            "complete"
        } else {
            "incomplete"
        }
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_sync_retention_apply(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    target: &crate::SyncTargetArgs,
    live_epoch_max_commits: usize,
    recovery_checkpoints_keep: usize,
    epoch_archives_keep: usize,
    dry_run: bool,
    rollover: bool,
    expire_epoch_archives: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let report = apply_sync_retention(
        &paths,
        &SyncRetentionPlanOptions {
            remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
            live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
            policy: SyncRetentionPolicy {
                live_epoch_max_commits,
                recovery_checkpoints_keep,
                epoch_archives_keep,
            },
        },
        dry_run,
        rollover,
        expire_epoch_archives,
    )
    .map_err(CliError::operation)?;
    print_sync_retention_apply(cli.output, &report)
}

fn print_sync_retention_apply(
    output: OutputFormat,
    report: &SyncRetentionApplyReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Recovery checkpoints: {} {}.",
        if report.dry_run {
            report.plan.recovery_checkpoints.expirable.len()
        } else {
            report.released_recovery_checkpoints.len()
        },
        if report.dry_run {
            "would be released"
        } else {
            "released"
        }
    );
    println!(
        "Epoch archives: {} {}.",
        if report.dry_run {
            report.plan.epoch_archives.expirable.len()
        } else {
            report.released_epoch_archives.len()
        },
        if report.dry_run {
            "would be eligible for explicit expiry"
        } else {
            "released"
        }
    );
    if let Some(rollover) = &report.epoch_rollover {
        println!(
            "Live epoch rolled over to {} with archive {}.",
            rollover.root_revision, rollover.remote_archive_ref
        );
    } else if report.plan.active_epoch.rollover_required {
        println!("Live epoch rollover remains required and was not applied.");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum CliResolution<'a> {
    Side(SyncConflictSideArg),
    Proposal(&'a str),
    Files(&'a [String]),
    Patch(&'a str),
    Editor,
}

fn cli_resolution<'a>(
    side: Option<SyncConflictSideArg>,
    proposal: Option<&'a str>,
    files: &'a [String],
    patch: Option<&'a str>,
    editor: bool,
) -> CliResolution<'a> {
    if let Some(proposal) = proposal {
        CliResolution::Proposal(proposal)
    } else if !files.is_empty() {
        CliResolution::Files(files)
    } else if let Some(patch) = patch {
        CliResolution::Patch(patch)
    } else if editor {
        CliResolution::Editor
    } else {
        CliResolution::Side(side.expect("clap requires one resolution mode"))
    }
}

#[allow(clippy::too_many_arguments)]
fn run_sync_resolve(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    conflict_id: &str,
    resolution: CliResolution<'_>,
    groups: &[String],
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    if !groups.is_empty() && matches!(resolution, CliResolution::Proposal(_)) {
        return Err(CliError::operation(
            "--group is already bound into the retained proposal selected by --approve-proposal",
        ));
    }
    match resolution {
        CliResolution::Proposal(proposal_id) => {
            let report = approve_resolution_proposal(
                &paths,
                conflict_id,
                proposal_id,
                &approval_options(target, dry_run)?,
                &vulcan_app::sync::SyncCancellationToken::default(),
            )
            .map_err(CliError::operation)?;
            print_proposal_approval(cli.output, &report)
        }
        CliResolution::Files(specifications) => run_file_resolution(
            cli,
            &paths,
            registration_profile.as_deref(),
            conflict_id,
            specifications,
            groups,
            target,
            dry_run,
        ),
        CliResolution::Patch(source) => run_patch_resolution(
            cli,
            &paths,
            registration_profile.as_deref(),
            conflict_id,
            source,
            groups,
            target,
            dry_run,
        ),
        CliResolution::Editor => run_editor_resolution(
            cli,
            &paths,
            registration_profile.as_deref(),
            conflict_id,
            groups,
            target,
            dry_run,
        ),
        CliResolution::Side(side) => {
            let report = resolve_sync_conflict(
                &paths,
                conflict_id,
                &ResolveSyncConflictOptions {
                    side: match side {
                        SyncConflictSideArg::Base => SyncConflictResolutionSide::Base,
                        SyncConflictSideArg::Local => SyncConflictResolutionSide::Local,
                        SyncConflictSideArg::Remote => SyncConflictResolutionSide::Remote,
                    },
                    group_ids: groups.to_vec(),
                    remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
                    live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
                    dry_run,
                },
            )
            .map_err(CliError::operation)?;
            print_sync_resolution(cli.output, &report)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_file_resolution(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
    conflict_id: &str,
    specifications: &[String],
    groups: &[String],
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    let (mut proposal_options, approval_options) =
        manual_resolution_options(cli, registration_profile, target, dry_run)?;
    proposal_options.group_ids = groups.to_vec();
    let supplied = read_supplied_resolution_files(specifications)?;
    if dry_run {
        let report = preview_supplied_resolution(
            paths,
            conflict_id,
            &proposal_options,
            &approval_options,
            supplied,
        )
        .map_err(CliError::operation)?;
        return print_supplied_resolution_preview(cli.output, &report);
    }
    run_supplied_resolution(
        cli.output,
        paths,
        conflict_id,
        &proposal_options,
        &approval_options,
        supplied,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_patch_resolution(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
    conflict_id: &str,
    source: &str,
    groups: &[String],
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    let patch = std::fs::read(source).map_err(|error| {
        CliError::operation(format!("cannot read resolution patch `{source}`: {error}"))
    })?;
    let (mut proposal_options, approval_options) =
        manual_resolution_options(cli, registration_profile, target, dry_run)?;
    proposal_options.group_ids = groups.to_vec();
    if dry_run {
        let report = preview_patch_resolution(
            paths,
            conflict_id,
            &proposal_options,
            &approval_options,
            &patch,
        )
        .map_err(CliError::operation)?;
        return print_patch_resolution_preview(cli.output, &report);
    }
    let prepared = prepare_patch_resolution(
        paths,
        conflict_id,
        &proposal_options,
        &approval_options,
        &patch,
    )
    .map_err(CliError::operation)?;
    run_supplied_resolution(
        cli.output,
        paths,
        conflict_id,
        &proposal_options,
        &approval_options,
        prepared.paths,
        prepared.selection.as_ref(),
    )
}

fn run_editor_resolution(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
    conflict_id: &str,
    groups: &[String],
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    let (mut proposal_options, approval_options) =
        manual_resolution_options(cli, registration_profile, target, dry_run)?;
    proposal_options.group_ids = groups.to_vec();
    let plan = prepare_editor_resolution(paths, conflict_id, &proposal_options, &approval_options)
        .map_err(CliError::operation)?;
    if dry_run {
        return print_editor_resolution_preview(cli.output, &plan);
    }
    let supplied = edit_resolution_files(&plan)?;
    run_supplied_resolution(
        cli.output,
        paths,
        conflict_id,
        &proposal_options,
        &approval_options,
        supplied,
        plan.selection.as_ref(),
    )
}

fn edit_resolution_files(
    plan: &EditorResolutionPlan,
) -> Result<Vec<ResolutionAgentPathOutput>, CliError> {
    let temporary = tempfile::tempdir().map_err(CliError::operation)?;
    let mut edited_paths = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        let path = safe_editor_path(temporary.path(), &file.path)?;
        std::fs::create_dir_all(
            path.parent()
                .expect("editor conflict path always has a temporary parent"),
        )
        .map_err(CliError::operation)?;
        std::fs::write(&path, &file.initial_content).map_err(CliError::operation)?;
        edited_paths.push(path);
    }
    let editor_paths = edited_paths
        .iter()
        .map(std::path::PathBuf::as_path)
        .collect::<Vec<_>>();
    open_paths_in_editor(&editor_paths).map_err(CliError::operation)?;
    plan.files
        .iter()
        .zip(edited_paths)
        .map(|(file, path)| {
            let content = std::fs::read(&path).map_err(CliError::operation)?;
            if content == file.initial_content {
                return Err(CliError::operation(format!(
                    "editor left conflict path `{}` unchanged",
                    file.path
                )));
            }
            if contains_bytes(&content, file.marker_token.as_bytes()) {
                return Err(CliError::operation(format!(
                    "editor result for `{}` still contains Vulcan conflict markers",
                    file.path
                )));
            }
            Ok(ResolutionAgentPathOutput {
                path: file.path.clone(),
                content,
            })
        })
        .collect()
}

fn safe_editor_path(
    root: &std::path::Path,
    relative: &str,
) -> Result<std::path::PathBuf, CliError> {
    let path = std::path::Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(CliError::operation(format!(
            "conflict path `{relative}` is unsafe for editor materialization"
        )));
    }
    Ok(root.join(path))
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn manual_resolution_options(
    cli: &Cli,
    registration_profile: Option<&str>,
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(ResolutionProposalOptions, ApproveResolutionProposalOptions), CliError> {
    let profile = cli
        .permissions
        .as_deref()
        .or(registration_profile)
        .unwrap_or("unrestricted");
    Ok((
        ResolutionProposalOptions {
            permission_profile: profile.to_string(),
            focused_context: Vec::new(),
            allow_broad_context: false,
            group_ids: Vec::new(),
        },
        approval_options(target, dry_run)?,
    ))
}

fn approval_options(
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<ApproveResolutionProposalOptions, CliError> {
    Ok(ApproveResolutionProposalOptions {
        remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
        live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
        dry_run,
        automatic: false,
    })
}

fn run_supplied_resolution(
    output: OutputFormat,
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    supplied: Vec<ResolutionAgentPathOutput>,
    expected_selection: Option<&ResolutionProposalSelection>,
) -> Result<(), CliError> {
    let cancellation = vulcan_app::sync::SyncCancellationToken::default();
    let proposal = if let Some(selection) = expected_selection {
        create_supplied_resolution_proposal_with_selection(
            paths,
            conflict_id,
            proposal_options,
            approval_options,
            supplied,
            selection,
            &cancellation,
        )
    } else {
        create_supplied_resolution_proposal(
            paths,
            conflict_id,
            proposal_options,
            approval_options,
            supplied,
            &cancellation,
        )
    }
    .map_err(CliError::operation)?;
    let report = approve_resolution_proposal(
        paths,
        conflict_id,
        &proposal.proposal_id,
        approval_options,
        &cancellation,
    )
    .map_err(CliError::operation)?;
    print_proposal_approval(output, &report)
}

fn read_supplied_resolution_files(
    specifications: &[String],
) -> Result<Vec<ResolutionAgentPathOutput>, CliError> {
    specifications
        .iter()
        .map(|specification| {
            let (path, source) = specification.split_once('=').ok_or_else(|| {
                CliError::operation(format!(
                    "invalid --file `{specification}`; expected CONFLICT_PATH=SOURCE"
                ))
            })?;
            if path.is_empty() || source.is_empty() {
                return Err(CliError::operation(format!(
                    "invalid --file `{specification}`; path and source must be non-empty"
                )));
            }
            let content = std::fs::read(source).map_err(|error| {
                CliError::operation(format!("cannot read resolution source `{source}`: {error}"))
            })?;
            Ok(ResolutionAgentPathOutput {
                path: path.to_string(),
                content,
            })
        })
        .collect()
}

fn print_supplied_resolution_preview(
    output: OutputFormat,
    report: &SuppliedResolutionPreviewReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Conflict {}: {:?} ({} supplied path(s), dry run)",
        report.conflict_id,
        report.outcome,
        report.paths.len()
    );
    Ok(())
}

fn print_patch_resolution_preview(
    output: OutputFormat,
    report: &PatchResolutionPreviewReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Conflict {}: {:?} (patch touches {} path(s), dry run)",
        report.conflict_id,
        report.outcome,
        report.paths.len()
    );
    Ok(())
}

fn print_editor_resolution_preview(
    output: OutputFormat,
    plan: &EditorResolutionPlan,
) -> Result<(), CliError> {
    let report = plan.preview_report();
    if output == OutputFormat::Json {
        return print_json(&report);
    }
    println!(
        "Conflict {}: {:?} (editor would open {} path(s), dry run)",
        report.conflict_id,
        report.outcome,
        report.paths.len()
    );
    Ok(())
}

fn print_proposal_approval(
    output: OutputFormat,
    report: &ApproveResolutionProposalReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Resolution proposal {}: {:?}{}",
        report.proposal_id,
        report.outcome,
        if report.dry_run { " (dry run)" } else { "" }
    );
    Ok(())
}

fn print_sync_resolution(
    output: OutputFormat,
    report: &ResolveSyncConflictReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Conflict {}: {:?} ({:?})",
        report.conflict_id, report.outcome, report.side
    );
    if let Some(commit) = &report.resolution_commit {
        println!("Accepted: {commit}");
    }
    if let Some(recovery) = &report.recovery_revision {
        println!("Recovery: {recovery}");
    }
    Ok(())
}

fn run_sync_doctor(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    target: &crate::SyncTargetArgs,
    profile: Option<ManagedDirectoryProfileArg>,
) -> Result<(), CliError> {
    if wiki.is_some() && profile.is_some() {
        return Err(CliError::operation(
            "`--profile` applies to direct paths only; registered wikis use their stored profile",
        ));
    }
    let (paths, registration_profile, registration_platform) =
        resolve_sync_paths(selected_paths, wiki)?;
    let profile = if let Some(wiki) = wiki {
        let id = WikiId::parse(wiki).map_err(CliError::operation)?;
        let status = WikiRegistry::user_default()
            .map_err(CliError::operation)?
            .show(&id)
            .map_err(CliError::operation)?;
        match status.registration.profile {
            ManagedDirectoryProfile::Knowledge => SyncContentProfile::Knowledge,
            ManagedDirectoryProfile::FilesOnly => SyncContentProfile::FilesOnly,
        }
    } else if let Some(profile) = profile {
        match profile {
            ManagedDirectoryProfileArg::Knowledge => SyncContentProfile::Knowledge,
            ManagedDirectoryProfileArg::FilesOnly => SyncContentProfile::FilesOnly,
        }
    } else if let Some(registration) = crate::registered_directory_for_path(paths.vault_root())? {
        match registration.profile {
            ManagedDirectoryProfile::Knowledge => SyncContentProfile::Knowledge,
            ManagedDirectoryProfile::FilesOnly => SyncContentProfile::FilesOnly,
        }
    } else {
        SyncContentProfile::Knowledge
    };
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let options = GitSyncOptions {
        remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
        live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
        dry_run: true,
        ..GitSyncOptions::default()
    };
    let platform = registration_platform
        .as_deref()
        .map(GitPlatformProfile::parse)
        .transpose()
        .map_err(CliError::operation)?
        .unwrap_or_else(GitPlatformProfile::native);
    let report = doctor_git_vault_for_profile(
        &paths,
        &options,
        platform,
        profile,
        profile == SyncContentProfile::FilesOnly,
    );
    print_sync_doctor_report(cli.output, &report)
}

fn resolve_sync_paths(
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
) -> Result<(VaultPaths, Option<String>, Option<String>), CliError> {
    let resolved = if let Some(wiki) = wiki {
        let id = WikiId::parse(wiki).map_err(CliError::operation)?;
        let status = WikiRegistry::user_default()
            .map_err(CliError::operation)?
            .show(&id)
            .map_err(CliError::operation)?;
        if status
            .registration
            .sync_backend
            .as_deref()
            .is_some_and(|backend| backend != "git")
        {
            return Err(CliError::operation(format!(
                "wiki `{id}` uses unsupported sync backend `{}`",
                status
                    .registration
                    .sync_backend
                    .as_deref()
                    .unwrap_or_default()
            )));
        }
        (
            VaultPaths::new(status.registration.path),
            status.registration.permissions_profile,
            status.registration.platform_profile,
        )
    } else {
        (selected_paths.clone(), None, None)
    };
    Ok(resolved)
}

fn check_sync_permission(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
) -> Result<(), CliError> {
    let profile = cli.permissions.as_deref().or(registration_profile);
    let selection = resolve_permission_profile(paths, profile).map_err(CliError::operation)?;
    ProfilePermissionGuard::new(paths, selection)
        .check_git()
        .map_err(CliError::operation)
}

fn handle_sync_devices(
    cli: &Cli,
    selected_paths: &VaultPaths,
    command: &SyncDeviceCommand,
) -> Result<(), CliError> {
    if let SyncDeviceCommand::SetName {
        device_id,
        name,
        wiki,
        dry_run,
    } = command
    {
        return handle_sync_device_name(
            cli,
            selected_paths,
            wiki.as_deref(),
            device_id,
            Some(name),
            *dry_run,
        );
    }
    if let SyncDeviceCommand::ClearName {
        device_id,
        wiki,
        dry_run,
    } = command
    {
        return handle_sync_device_name(
            cli,
            selected_paths,
            wiki.as_deref(),
            device_id,
            None,
            *dry_run,
        );
    }
    let (wiki, target) = match command {
        SyncDeviceCommand::List { wiki, target, .. }
        | SyncDeviceCommand::Register { wiki, target, .. }
        | SyncDeviceCommand::Revoke { wiki, target, .. }
        | SyncDeviceCommand::Unregister { wiki, target, .. }
        | SyncDeviceCommand::Fetch { wiki, target, .. }
        | SyncDeviceCommand::Remove { wiki, target, .. } => (wiki.as_deref(), target),
        SyncDeviceCommand::SetName { .. } | SyncDeviceCommand::ClearName { .. } => unreachable!(),
    };
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let options = SyncDeviceOptions {
        remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
        live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
    };
    match command {
        SyncDeviceCommand::List { offline, .. } => {
            let report = list_sync_device_backups_with_observation(&paths, &options, !offline)
                .map_err(CliError::operation)?;
            print_sync_device_list(cli.output, &report)
        }
        SyncDeviceCommand::Register {
            public_key,
            label,
            dry_run,
            ..
        } => {
            let text = read_public_key_input(public_key)?;
            let report =
                register_placeholder(&paths, &options.remote, &text, label.as_deref(), *dry_run)
                    .map_err(CliError::operation)?;
            print_registration_change(cli.output, &report)
        }
        SyncDeviceCommand::Revoke {
            device_id, dry_run, ..
        } => {
            let report = revoke_registration(&paths, &options.remote, device_id, *dry_run)
                .map_err(CliError::operation)?;
            print_registration_change(cli.output, &report)
        }
        SyncDeviceCommand::Unregister {
            device_id, dry_run, ..
        } => {
            let report = unregister_registration(&paths, &options.remote, device_id, *dry_run)
                .map_err(CliError::operation)?;
            print_registration_change(cli.output, &report)
        }
        SyncDeviceCommand::Fetch {
            device_id, dry_run, ..
        } => {
            let report = fetch_sync_device_backup(&paths, &options, device_id, *dry_run)
                .map_err(CliError::operation)?;
            print_sync_device_fetch(cli.output, &report)
        }
        SyncDeviceCommand::Remove {
            device_id, dry_run, ..
        } => {
            let report = remove_sync_device_backup(&paths, &options, device_id, *dry_run)
                .map_err(CliError::operation)?;
            print_sync_device_remove(cli.output, &report)
        }
        SyncDeviceCommand::SetName { .. } | SyncDeviceCommand::ClearName { .. } => unreachable!(),
    }
}

fn handle_sync_device_name(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    device_id: &str,
    name: Option<&str>,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    set_sync_device_name(&paths, device_id, name, dry_run).map_err(CliError::operation)?;
    if cli.output == OutputFormat::Json {
        return print_json(&serde_json::json!({
            "device_id": device_id,
            "name": name,
            "dry_run": dry_run,
        }));
    }
    let action = match (name, dry_run) {
        (Some(_), true) => "Would set",
        (Some(_), false) => "Set",
        (None, true) => "Would clear",
        (None, false) => "Cleared",
    };
    match name {
        Some(name) => println!("{action} device name {name} for {device_id}."),
        None => println!("{action} device name for {device_id}."),
    }
    Ok(())
}

fn print_sync_device_list(
    output: OutputFormat,
    report: &SyncDeviceListReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Device inventory (remote {}, profile {})",
        report.remote, report.profile
    );
    match report.remote_observation.state {
        SyncDeviceRemoteObservationState::Available => {
            println!("Remote device safety backups observed: {}", report.count);
            println!("Remote observation: available now.");
        }
        SyncDeviceRemoteObservationState::Unavailable => {
            println!("Remote device safety backups: unknown");
            println!("Remote observation: unavailable; showing local inventory only.");
            if let Some(error) = &report.remote_observation.error {
                println!("  Error: {error}");
            }
        }
        SyncDeviceRemoteObservationState::NotRequested => {
            println!("Remote device safety backups: not requested (--offline); unknown.");
            println!("Remote observation: not requested; showing local inventory only.");
        }
    }
    if let Some(device_id) = &report.current_device_id {
        println!("This device: {device_id}");
    }
    if report.backups.is_empty() {
        if report.remote_observation.state == SyncDeviceRemoteObservationState::Available {
            println!("No device backups found. A successful non-dry-run sync creates one.");
        } else if report.remote_observation.state == SyncDeviceRemoteObservationState::NotRequested
        {
            println!("No remote backups were requested; remote backup state is unknown.");
        } else {
            println!("No remote backups were observed; remote backup state is unknown.");
        }
    } else {
        for backup in &report.backups {
            println!(
                "\n{}{}",
                backup.name.as_deref().unwrap_or("(unnamed)"),
                if backup.current_device {
                    " (this device)"
                } else {
                    ""
                }
            );
            println!("  ID: {}", backup.device_id);
            println!("  ID kind: {}", device_id_kind_label(backup.identity_kind));
            println!(
                "  Remote backup: {} at {}",
                backup.revision, backup.remote_ref
            );
            match backup.recovery_status {
                SyncDeviceRecoveryStatus::NotFetched => {
                    println!("  Local recovery: not fetched");
                }
                SyncDeviceRecoveryStatus::Current => {
                    println!("  Local recovery: current at {}", backup.recovery_ref);
                }
                SyncDeviceRecoveryStatus::Stale => {
                    println!(
                        "  Local recovery: stale at {} in {}",
                        backup.recovery_revision.as_deref().unwrap_or("unknown"),
                        backup.recovery_ref
                    );
                }
            }
        }
    }
    print_sync_device_local_inventory(report);
    if !report.backups.is_empty() {
        println!("\nRecover another device with: vulcan sync devices fetch <device-id>");
    }
    if let Some(registrations) = &report.registrations {
        print_registrations(registrations);
    } else if let Some(error) = &report.registrations_error {
        println!("\nRegistrations unavailable: {error}");
    }
    Ok(())
}

fn print_sync_device_local_inventory(report: &SyncDeviceListReport) {
    if !report.retained_recovery.is_empty() {
        if report.remote_observation.state == SyncDeviceRemoteObservationState::Available {
            println!("\nRetained local recovery copies without a remote backup:");
        } else if report.remote_observation.state == SyncDeviceRemoteObservationState::NotRequested
        {
            println!("\nRetained local recovery copies (remote backup not requested):");
        } else {
            println!("\nRetained local recovery copies (remote backup status unknown):");
        }
        for recovery in &report.retained_recovery {
            println!(
                "\n{}{}",
                recovery.name.as_deref().unwrap_or("(unnamed)"),
                if recovery.current_device {
                    " (this device)"
                } else {
                    ""
                }
            );
            println!("  ID: {}", recovery.device_id);
            println!(
                "  ID kind: {}",
                device_id_kind_label(recovery.identity_kind)
            );
            println!(
                "  Local recovery: {} at {}",
                recovery.revision, recovery.recovery_ref
            );
        }
    }
    let named_without_recovery =
        if report.remote_observation.state == SyncDeviceRemoteObservationState::NotRequested {
            &report.named_without_local_recovery
        } else {
            &report.named_without_backup
        };
    if !named_without_recovery.is_empty() {
        if report.remote_observation.state == SyncDeviceRemoteObservationState::Available {
            println!("\nNamed devices without a backup or local recovery copy:");
        } else if report.remote_observation.state == SyncDeviceRemoteObservationState::NotRequested
        {
            println!(
                "\nNamed devices without a local recovery copy (remote backup not requested):"
            );
        } else {
            println!(
                "\nNamed devices without a local recovery copy (remote backup status unknown):"
            );
        }
        for device in named_without_recovery {
            println!(
                "\n{}{}\n  ID: {}",
                device.name,
                if device.current_device {
                    " (this device)"
                } else {
                    ""
                },
                device.device_id
            );
            println!("  ID kind: {}", device_id_kind_label(device.identity_kind));
        }
    }
}

fn device_id_kind_label(kind: GitSyncDeviceIdKind) -> &'static str {
    match kind {
        GitSyncDeviceIdKind::LegacyUlid => "legacy_ulid",
        GitSyncDeviceIdKind::SshKeyV1 => "ssh_key_v1 (key-shaped ID; unverified)",
    }
}

fn print_sync_device_fetch(
    output: OutputFormat,
    report: &SyncDeviceFetchReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.dry_run {
        println!(
            "Would fetch device {} at {} into {}.",
            report.device_id, report.revision, report.local_device_ref
        );
        if let Some(local_live) = &report.local_live_ref {
            println!("Would also fetch accepted live into {local_live} for comparison.");
        }
        return Ok(());
    }
    println!(
        "Fetched device {} at {} into durable local ref {}.",
        report.device_id, report.revision, report.local_device_ref
    );
    if let Some(relation) = report.relation {
        println!(
            "Relationship to accepted live: {}.",
            device_relation_label(relation)
        );
    }
    let changed_paths = report.changed_paths.as_deref().unwrap_or_default();
    if changed_paths.is_empty() {
        println!("Changed paths versus accepted live: none.");
    } else {
        println!(
            "Changed paths versus accepted live ({}): {}",
            changed_paths.len(),
            changed_paths.join(", ")
        );
    }
    if let Some(local_live) = &report.local_live_ref {
        println!(
            "Review: git diff {}..{}",
            local_live, report.local_device_ref
        );
    }
    println!(
        "Isolate for conflict resolution: git worktree add --detach <new-directory> {}",
        report.local_device_ref
    );
    Ok(())
}

fn print_sync_device_remove(
    output: OutputFormat,
    report: &SyncDeviceRemoveReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.dry_run {
        println!(
            "Safe to prune integrated remote safety backup {} at {} ({}).",
            report.device_id,
            report.revision,
            device_relation_label(report.relation)
        );
        println!(
            "Run again without --dry-run to delete remote ref {}; device, Git, and vault access will remain unchanged.",
            report.remote_device_ref
        );
    } else if report.removed {
        println!(
            "Pruned remote safety backup {}. Local recovery ref {} remains available. Device, Git, and vault access are unchanged.",
            report.remote_device_ref, report.local_recovery_retained
        );
    } else {
        println!("Remote safety backup was already absent; local recovery ref remains available. Device, Git, and vault access are unchanged.");
    }
    Ok(())
}

const fn device_relation_label(relation: SyncDeviceRelation) -> &'static str {
    match relation {
        SyncDeviceRelation::LiveUninitialized => "accepted live is uninitialized",
        SyncDeviceRelation::Same => "identical to accepted live",
        SyncDeviceRelation::Integrated => "fully integrated into accepted live",
        SyncDeviceRelation::ContainsLive => "contains accepted live plus device-only commits",
        SyncDeviceRelation::Diverged => "diverged and contains device-only history",
    }
}

fn run_sync_conflicts_archive(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    older_than_days: u64,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let older_than = std::time::Duration::from_secs(older_than_days.saturating_mul(24 * 60 * 60));
    let report = vulcan_app::sync_conflicts::archive_sync_conflicts(&paths, older_than, dry_run)
        .map_err(CliError::operation)?;
    if cli.output == OutputFormat::Json {
        return print_json(&report);
    }
    let verb = if dry_run { "Would archive" } else { "Archived" };
    println!(
        "{verb} {} closed conflict(s); the archive holds {}.",
        report.archived.len(),
        report.archived_total
    );
    for id in &report.archived {
        println!("  {id}");
    }
    Ok(())
}

fn run_sync_conflicts(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    conflict_id: Option<&str>,
    path_offset: usize,
    path_limit: Option<usize>,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    if conflict_id.is_none() && (path_offset != 0 || path_limit.is_some()) {
        return Err(CliError::operation(
            "--path-offset and --path-limit require a conflict ID",
        ));
    }
    if let Some(conflict_id) = conflict_id {
        let report = if let Some(limit) = path_limit {
            vulcan_app::sync_conflicts::get_sync_conflict_page(
                &paths,
                conflict_id,
                path_offset,
                limit,
            )
        } else {
            get_sync_conflict(&paths, conflict_id)
        }
        .map_err(CliError::operation)?;
        print_sync_conflict_detail(cli.output, &report, wiki)
    } else {
        let report = list_sync_conflicts(&paths).map_err(CliError::operation)?;
        print_sync_conflict_list(cli.output, &report, wiki)
    }
}

fn print_sync_conflict_list(
    output: OutputFormat,
    report: &SyncConflictListReport,
    wiki: Option<&str>,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!("Active unresolved sync conflicts: {}", report.count);
    if report.superseded_count > 0 {
        println!(
            "Superseded conflict records retained as history: {}",
            report.superseded_count
        );
    }
    if report.archived_count > 0 {
        println!(
            "Archived closed conflicts (readable by ID): {}",
            report.archived_count
        );
    }
    for conflict in &report.conflicts {
        println!(
            "{}\t{:?}\t{} path(s), {} pending group(s)\t{}",
            conflict.id,
            conflict.scope,
            conflict.path_count,
            conflict.pending_group_count,
            if conflict.paths_complete {
                conflict.paths.join(", ")
            } else {
                format!(
                    "{} … ({} of {})",
                    conflict.paths.join(", "),
                    conflict.paths_returned,
                    conflict.path_count
                )
            }
        );
        println!(
            "  Inspect: vulcan sync conflicts {}{}",
            conflict.id,
            sync_wiki_selector(wiki)
        );
        println!(
            "  Resolve interactively: vulcan sync resolve {}{} --editor",
            conflict.id,
            sync_wiki_selector(wiki)
        );
    }
    if report.count > 0 {
        println!("Resolution choices:");
        println!("  --editor       review every conflicted file in $VISUAL/$EDITOR");
        println!("  --side local   keep the preserved local version of every conflicted path");
        println!("  --side remote  keep the preserved remote version of every conflicted path");
        println!("Add --dry-run to validate a choice without applying it.");
    } else {
        println!("No conflict resolution is currently required.");
    }
    Ok(())
}

fn print_sync_conflict_detail(
    output: OutputFormat,
    report: &SyncConflictDetailReport,
    wiki: Option<&str>,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!("Conflict {} ({:?})", report.record.id, report.resolution);
    println!("Scope:  {:?}", report.record.scope);
    println!("Local:  {}", report.record.local_revision);
    println!("Remote: {}", report.record.remote_revision);
    if let Some(base) = &report.record.base_revision {
        println!("Base:   {base}");
    }
    println!(
        "Progress: {} pending, {} prepared, {} published, {} applied, {} need rebase ({} total groups)",
        report.progress.pending_groups,
        report.progress.prepared_groups,
        report.progress.published_groups,
        report.progress.applied_groups,
        report.progress.needs_rebase_groups,
        report.progress.total_groups,
    );
    for path in &report.record.paths {
        if let Some(classification) = &path.classification {
            println!(
                "Path:   {} [group {} {:?}] ({:?}; {:?})",
                path.path,
                path.group_id,
                path.group_kind,
                classification.class,
                classification.effective_resolution
            );
        } else {
            println!(
                "Path:   {} [group {} {:?}]",
                path.path, path.group_id, path.group_kind
            );
        }
    }
    if let Some(page) = &report.path_page {
        println!(
            "Path page: offset {}, returned {}, total {}{}",
            page.offset,
            report.record.paths.len(),
            page.total,
            page.next_offset
                .map_or_else(String::new, |offset| format!(", next offset {offset}"))
        );
    }
    if let Some(original) = &report.record.carried_from {
        println!(
            "Carried forward from conflict {original}: its files changed again on the live branch; this record compares your version with what is live now."
        );
    }
    if report.resolution == SyncConflictResolutionState::Superseded {
        println!("Action: none; later synchronization superseded this immutable history record.");
        if let Some(supersession) = &report.supersession {
            println!("Current revision: {}", supersession.current_revision);
            if let Some(replacement) = &supersession.replacement_conflict_id {
                println!("Replacement conflict: {replacement}");
            }
        }
        return Ok(());
    }
    let selector = sync_wiki_selector(wiki);
    println!("Next steps:");
    println!(
        "  Interactive review: vulcan sync resolve {}{} --editor --dry-run",
        report.record.id, selector
    );
    println!(
        "  Keep local paths:   vulcan sync resolve {}{} --side local --dry-run",
        report.record.id, selector
    );
    println!(
        "  Keep remote paths:  vulcan sync resolve {}{} --side remote --dry-run",
        report.record.id, selector
    );
    println!("Remove --dry-run from the chosen command to review/apply the resolution.");
    Ok(())
}

fn sync_wiki_selector(wiki: Option<&str>) -> String {
    wiki.map_or_else(String::new, |wiki| format!(" --wiki {wiki}"))
}

fn print_sync_doctor_report(
    output: OutputFormat,
    report: &SyncDoctorReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Sync doctor: {} ({})",
        report.vault.display(),
        if report.healthy {
            "no errors"
        } else {
            "errors found"
        }
    );
    for check in &report.checks {
        let severity = match check.severity {
            SyncDoctorSeverity::Pass => "pass",
            SyncDoctorSeverity::Info => "info",
            SyncDoctorSeverity::Warning => "warning",
            SyncDoctorSeverity::Error => "error",
        };
        println!("{severity}\t{}\t{}", check.code, check.message);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AutomaticSyncReport {
    action: &'static str,
    dry_run: bool,
    wiki: WikiRegistration,
}

fn set_automatic_sync(
    output: OutputFormat,
    paths: &VaultPaths,
    wiki: Option<&str>,
    paused: bool,
    dry_run: bool,
) -> Result<(), CliError> {
    let registry = WikiRegistry::user_default().map_err(CliError::operation)?;
    let id = match wiki {
        Some(wiki) => WikiId::parse(wiki).map_err(CliError::operation)?,
        None => {
            registry
                .find_by_path(paths.vault_root())
                .map_err(CliError::operation)?
                .id
        }
    };
    let wiki = registry
        .update(
            &id,
            &UpdateWikiRequest {
                groups_to_add: Vec::new(),
                groups_to_remove: Vec::new(),
                permissions_profile: None,
                sync_paused: Some(paused),
                profile: None,
            },
            dry_run,
        )
        .map_err(CliError::operation)?;
    let report = AutomaticSyncReport {
        action: if paused { "pause" } else { "resume" },
        dry_run,
        wiki,
    };
    if output == OutputFormat::Json {
        print_json(&report)
    } else {
        println!(
            "Automatic sync {} for wiki `{}`{}.",
            if paused { "paused" } else { "resumed" },
            report.wiki.id,
            if dry_run { " (dry run)" } else { "" }
        );
        Ok(())
    }
}

fn registered_selection(
    selection: &SyncSelectionArgs,
) -> Result<Option<RegisteredSyncSelection>, CliError> {
    if selection.all {
        Ok(Some(RegisteredSyncSelection::All))
    } else if let Some(group) = &selection.group {
        WikiId::parse(group).map_err(CliError::operation)?;
        Ok(Some(RegisteredSyncSelection::Group(group.clone())))
    } else {
        selection
            .wiki
            .as_deref()
            .map(WikiId::parse)
            .transpose()
            .map(|id| id.map(RegisteredSyncSelection::Wiki))
            .map_err(CliError::operation)
    }
}

fn print_registered_sync_report(
    output: OutputFormat,
    report: &RegisteredSyncReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.dry_run {
        println!(
            "Registered sync preview {}: {} clear, {} with unresolved conflicts, {} incomplete, {} inspection failures (no changes applied)",
            report.selection,
            report.succeeded,
            report.conflicted,
            report.incomplete,
            report.failed
        );
    } else {
        println!(
            "Registered sync {}: {} succeeded, {} conflicted, {} incomplete, {} failed",
            report.selection, report.succeeded, report.conflicted, report.incomplete, report.failed
        );
    }
    for item in &report.items {
        if let Some(sync) = &item.report {
            let outcome = if sync.sync.dry_run {
                sync_preview_message(&sync.sync)
            } else {
                format!("{:?}", sync.sync.outcome)
            };
            let retained = item.retained_conflicts.map_or_else(String::new, |count| {
                if count == 0 {
                    String::new()
                } else {
                    format!(
                        "; {count} active unresolved conflict(s) (inspect resolution choices with `vulcan sync conflicts --wiki {}`)",
                        item.wiki_id
                    )
                }
            });
            let incomplete = item
                .retained_conflicts_error
                .as_deref()
                .map_or_else(String::new, |error| {
                    format!("; retained conflict status unavailable: {error}")
                });
            let backup = sync
                .sync
                .device_backup
                .as_ref()
                .map_or_else(String::new, |backup| {
                    format!(
                        "; device backup {} at {}",
                        device_backup_outcome_label(backup.outcome),
                        backup.reference
                    )
                });
            println!(
                "{}\t{}{}{}{}\t{}",
                item.wiki_id,
                outcome,
                retained,
                incomplete,
                backup,
                item.path.display()
            );
        } else if let Some(error) = &item.error {
            println!("{}\terror\t{}: {error}", item.wiki_id, item.path.display());
        }
    }
    Ok(())
}

fn print_sync_report(
    output: OutputFormat,
    verbose: bool,
    report: &VaultSyncReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let outcome = if report.sync.dry_run {
                format!(
                    "Sync preview: {} (no changes applied)",
                    sync_preview_message(&report.sync)
                )
            } else {
                sync_outcome_message(report.sync.outcome, report.sync.remote.as_str())
            };
            println!("{outcome}");
            if let Some(branch) = &report.sync.branch {
                if let Some(line) = branch_lane_message(branch) {
                    println!("{line}");
                }
                if let Some(line) = branch_push_message(branch) {
                    println!("{line}");
                }
            }
            if let Some(backup) = &report.sync.device_backup {
                println!(
                    "Device backup: snapshot {} is reachable from {} at {} ({}).",
                    backup.snapshot_revision,
                    report.sync.remote,
                    backup.reference,
                    device_backup_outcome_label(backup.outcome)
                );
            }
            if verbose {
                println!("Remote ref: {}", report.sync.refs.live);
                if let Some(accepted) = &report.sync.accepted {
                    println!("Accepted: {accepted}");
                }
                print_sync_operational_stats(&report.operational_stats);
            }
            if report
                .sync
                .actions
                .contains(&GitSyncAction::WorktreeApplied)
            {
                println!("Applied the accepted tree to the vault.");
            }
            if let Some(conflict) = &report.sync.conflict {
                println!(
                    "Conflict: local {} vs remote {}",
                    conflict.local, conflict.remote
                );
                if !conflict.diagnostics.is_empty() {
                    println!("{}", conflict.diagnostics);
                }
            }
            if let Some(pause) = &report.sync.pause {
                let detail = match pause.reason {
                    vulcan_app::sync::GitSyncPauseReason::HeadMoved => format!(
                        "HEAD moved from {} to {}",
                        pause
                            .expected_head
                            .as_ref()
                            .map_or("unborn", |oid| oid.as_str()),
                        pause
                            .actual_head
                            .as_ref()
                            .map_or("unborn", |oid| oid.as_str())
                    ),
                    vulcan_app::sync::GitSyncPauseReason::OperationInProgress => format!(
                        "Git {} operation is in progress",
                        pause.operation.as_deref().unwrap_or("unknown")
                    ),
                };
                println!("Paused: {detail}. Captured work remains reachable.");
            }
            if let Some(refresh) = &report.cache_refresh {
                println!(
                    "Cache refreshed: {} added, {} updated, {} deleted",
                    refresh.added, refresh.updated, refresh.deleted
                );
            }
            if let Some(error) = &report.cache_refresh_error {
                println!(
                    "Warning: the vault synchronized, but refreshing the rebuildable cache failed: {error}"
                );
            }
            if let Some(recovered) = &report.state.recovered_from {
                println!(
                    "Recovery: recaptured after interrupted transaction {} in {:?} state.",
                    recovered.transaction_id, recovered.phase
                );
            }
            if let Some(retained) = &report.state.retained {
                println!(
                    "Retained state: transaction {} is {:?} at {}.",
                    retained.transaction_id,
                    retained.phase,
                    report.state.journal_path.display()
                );
            }
            Ok(())
        }
    }
}

fn print_sync_operational_stats(stats: &vulcan_app::sync::SyncOperationalStats) {
    println!(
        "Operational stats: {} automatic, {} conflicted, {} groups, {} formatting candidates, {} preserved input bytes, {} Git subprocesses; {} ms total (backend {}, conflict state {}, cache {}).",
        stats.automatic_resolution_paths,
        stats.conflict_paths,
        stats.conflict_groups,
        stats.formatting_candidate_paths,
        stats.preserved_input_bytes,
        stats
            .git_subprocesses
            .map_or_else(|| "unavailable".to_string(), |count| count.to_string()),
        stats.elapsed_ms,
        stats.backend_cycle_ms,
        stats.conflict_state_ms,
        stats.cache_refresh_ms,
    );
}

fn sync_outcome_message(outcome: GitSyncOutcome, remote: &str) -> String {
    match outcome {
        GitSyncOutcome::Planned => {
            format!("Sync preview: inspected {remote} (no changes applied)")
        }
        GitSyncOutcome::Paused => format!("Sync: paused before reconciling with {remote}"),
        GitSyncOutcome::UpToDate => format!("Sync: up to date with {remote}"),
        GitSyncOutcome::Bootstrapped => format!("Sync: initialized {remote}"),
        GitSyncOutcome::Pushed => format!("Sync: pushed to {remote}"),
        GitSyncOutcome::Pulled => format!("Sync: pulled from {remote}"),
        GitSyncOutcome::Merged => format!("Sync: merged with {remote}"),
        GitSyncOutcome::Conflicted => format!("Sync: conflict with {remote} requires review"),
    }
}

fn sync_preview_message(report: &GitSyncReport) -> String {
    let branch = branch_preview_message(report.branch.as_ref());
    let files = report.preview.as_ref().map_or_else(
        || "file state unavailable".to_string(),
        |preview| match preview.file_state {
            GitSyncPreviewFileState::UpToDate => "file lane appears up to date".to_string(),
            GitSyncPreviewFileState::LocalChanges => {
                "local files changed since the last sync snapshot".to_string()
            }
            GitSyncPreviewFileState::RemoteDiffers => {
                "remote file lane differs; run sync to reconcile".to_string()
            }
            GitSyncPreviewFileState::LocalAndRemoteDiffer => {
                "local files and remote file lane both differ; run sync to reconcile".to_string()
            }
            GitSyncPreviewFileState::LocalMissing => {
                "local file snapshot is missing; run sync to initialize it".to_string()
            }
            GitSyncPreviewFileState::RemoteMissing => {
                "remote file lane is missing; run sync to publish it".to_string()
            }
            GitSyncPreviewFileState::Uninitialized => {
                "file lane is not initialized; run sync to initialize it".to_string()
            }
        },
    );
    format!("{branch}; {files}")
}

fn branch_preview_message(branch: Option<&GitBranchSync>) -> String {
    branch.map_or_else(
        || "branch not inspected".to_string(),
        |branch| {
            let name = branch
                .branch
                .as_str()
                .strip_prefix("refs/heads/")
                .unwrap_or(branch.branch.as_str());
            match branch.action {
                GitBranchSyncAction::UpToDate => format!("branch {name} is up to date"),
                GitBranchSyncAction::Planned => format!(
                    "branch {name}: {}",
                    branch
                        .detail
                        .as_deref()
                        .unwrap_or("changes would be required")
                ),
                GitBranchSyncAction::Skipped => format!("branch {name} was skipped"),
                action => format!("branch {name} preview: {action:?}"),
            }
        },
    )
}

const fn device_backup_outcome_label(outcome: GitDeviceBackupOutcome) -> &'static str {
    match outcome {
        GitDeviceBackupOutcome::Current => "already protected",
        GitDeviceBackupOutcome::Published => "published",
        GitDeviceBackupOutcome::Bridged => "published with prior device history preserved",
    }
}

/// Renders the branch lane for human output. Steady states (up to date,
/// skipped for lack of upstream) stay quiet; everything else gets one line.
fn branch_lane_message(branch: &GitBranchSync) -> Option<String> {
    let name = branch
        .branch
        .as_str()
        .strip_prefix("refs/heads/")
        .unwrap_or(branch.branch.as_str());
    let revision = branch.after.as_ref().map(ToString::to_string);
    match branch.action {
        GitBranchSyncAction::UpToDate | GitBranchSyncAction::Skipped => None,
        GitBranchSyncAction::FastForwarded => Some(format!(
            "Branch {name} fast-forwarded to {}.",
            revision.as_deref().unwrap_or("unknown"),
        )),
        GitBranchSyncAction::Merged => Some(format!(
            "Branch {name} merged at {}.",
            revision.as_deref().unwrap_or("unknown"),
        )),
        GitBranchSyncAction::Rebased => Some(format!(
            "Branch {name} rebased at {}.",
            revision.as_deref().unwrap_or("unknown"),
        )),
        GitBranchSyncAction::Adopted => Some(format!(
            "Branch {name} adopted {} already synchronized through live.",
            revision.as_deref().unwrap_or("unknown"),
        )),
        GitBranchSyncAction::Paused => Some(format!(
            "Branch {name} paused: {}.",
            branch.detail.as_deref().unwrap_or("unknown reason"),
        )),
        GitBranchSyncAction::Deferred => Some(format!(
            "Branch {name} deferred: {}.",
            branch.detail.as_deref().unwrap_or("unknown reason"),
        )),
        GitBranchSyncAction::Failed => Some(format!(
            "Branch {name} failed: {}.",
            branch.detail.as_deref().unwrap_or("unknown reason"),
        )),
        GitBranchSyncAction::Planned => Some(format!(
            "Branch {name} would pull: {}.",
            branch.detail.as_deref().unwrap_or("unknown plan"),
        )),
    }
}

/// Renders a branch publication success or failure. A lane that did not need
/// publication stays quiet.
fn branch_push_message(branch: &GitBranchSync) -> Option<String> {
    let name = branch
        .branch
        .as_str()
        .strip_prefix("refs/heads/")
        .unwrap_or(branch.branch.as_str());
    if branch.pushed {
        Some(format!(
            "Pushed branch {name} to {}.",
            branch.remote.as_ref().map_or("unknown", GitRemote::as_str),
        ))
    } else {
        branch
            .push_detail
            .as_deref()
            .map(|detail| format!("Branch {name} was not pushed: {detail}."))
    }
}

#[cfg(test)]
mod sync_report_tests {
    use super::*;

    #[test]
    fn sync_outcome_messages_are_compact_and_human_readable() {
        assert_eq!(
            sync_outcome_message(GitSyncOutcome::Planned, "origin"),
            "Sync preview: inspected origin (no changes applied)"
        );
        assert_eq!(
            sync_outcome_message(GitSyncOutcome::UpToDate, "origin"),
            "Sync: up to date with origin"
        );
        assert_eq!(
            sync_outcome_message(GitSyncOutcome::Pushed, "backup"),
            "Sync: pushed to backup"
        );
        assert_eq!(
            sync_outcome_message(GitSyncOutcome::Conflicted, "origin"),
            "Sync: conflict with origin requires review"
        );
    }

    fn branch_lane(action: GitBranchSyncAction, detail: Option<&str>) -> GitBranchSync {
        GitBranchSync {
            branch: GitRefName::parse("refs/heads/main").expect("branch"),
            remote: Some(GitRemote::parse("origin").expect("remote")),
            upstream: None,
            tracking: None,
            before: None,
            after: None,
            action,
            detail: detail.map(str::to_string),
            pushed: false,
            push_detail: None,
        }
    }

    #[test]
    fn branch_lane_messages_stay_quiet_when_steady() {
        assert_eq!(
            branch_lane_message(&branch_lane(GitBranchSyncAction::UpToDate, None)),
            None
        );
        assert_eq!(
            branch_lane_message(&branch_lane(GitBranchSyncAction::Skipped, None)),
            None
        );
        assert!(
            branch_lane_message(&branch_lane(GitBranchSyncAction::FastForwarded, None))
                .is_some_and(|line| line.contains("main") && line.contains("fast-forwarded"))
        );
        assert!(
            branch_lane_message(&branch_lane(GitBranchSyncAction::Adopted, None))
                .is_some_and(|line| line.contains("main") && line.contains("adopted"))
        );
        assert!(branch_lane_message(&branch_lane(
            GitBranchSyncAction::Paused,
            Some("branch diverged and pull.ff=only refuses non-fast-forward"),
        ))
        .is_some_and(|line| line.contains("paused") && line.contains("pull.ff=only")));
        let mut pushed = branch_lane(GitBranchSyncAction::UpToDate, None);
        assert_eq!(branch_push_message(&pushed), None);
        pushed.pushed = true;
        assert_eq!(
            branch_push_message(&pushed),
            Some("Pushed branch main to origin.".to_string())
        );
        pushed.pushed = false;
        pushed.push_detail = Some("remote advanced first; retry later".to_string());
        assert_eq!(
            branch_push_message(&pushed),
            Some("Branch main was not pushed: remote advanced first; retry later.".to_string())
        );
    }

    #[test]
    fn branch_preview_message_names_an_up_to_date_branch() {
        assert_eq!(
            branch_preview_message(Some(&branch_lane(GitBranchSyncAction::UpToDate, None))),
            "branch main is up to date"
        );
    }

    #[cfg(unix)]
    #[test]
    fn subscribe_url_file_must_be_private_bounded_and_single_line() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let temporary_root = temporary
            .path()
            .canonicalize()
            .expect("canonical temporary directory");
        let source = temporary_root.join("subscribe-url");
        std::fs::write(&source, "https://patch.example/h/private?pubsub=true\n")
            .expect("secret file");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600))
            .expect("private permissions");
        assert_eq!(
            read_subscribe_url(&source).expect("URL"),
            "https://patch.example/h/private?pubsub=true"
        );

        std::fs::write(&source, "https://one.example\nhttps://two.example\n")
            .expect("multiline secret");
        assert!(read_subscribe_url(&source)
            .expect_err("multiple lines must fail")
            .to_string()
            .contains("exactly one line"));

        std::fs::write(&source, vec![b'x'; MAX_SUBSCRIBE_URL_INPUT_BYTES + 1])
            .expect("oversized secret");
        assert!(read_subscribe_url(&source)
            .expect_err("oversized input must fail")
            .to_string()
            .contains("4096-byte limit"));

        let target = temporary_root.join("target");
        std::fs::write(&target, "https://patch.example/h/symlink?pubsub=true")
            .expect("symlink target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
            .expect("private target permissions");
        let symlink = temporary_root.join("subscribe-url-symlink");
        std::os::unix::fs::symlink(&target, &symlink).expect("secret symlink");
        assert!(
            read_subscribe_url(&symlink).is_err(),
            "the checked path must not be followed after validation"
        );
        let real_directory = temporary_root.join("real-directory");
        std::fs::create_dir(&real_directory).expect("real secret directory");
        let nested_target = real_directory.join("subscribe-url");
        std::fs::write(
            &nested_target,
            "https://patch.example/h/parent-symlink?pubsub=true",
        )
        .expect("nested secret");
        std::fs::set_permissions(&nested_target, std::fs::Permissions::from_mode(0o600))
            .expect("private nested permissions");
        let linked_directory = temporary_root.join("linked-directory");
        std::os::unix::fs::symlink(&real_directory, &linked_directory)
            .expect("secret directory symlink");
        assert!(
            read_subscribe_url(&linked_directory.join("subscribe-url")).is_err(),
            "intermediate path components must not redirect the secret read"
        );

        std::fs::write(&source, "https://patch.example/h/private").expect("valid secret again");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644))
            .expect("public permissions");
        assert!(read_subscribe_url(&source)
            .expect_err("public secret file must fail")
            .to_string()
            .contains("group or other users"));
    }
}

fn map_network_mode(mode: NetworkNotificationModeArg) -> NetworkNotificationMode {
    match mode {
        NetworkNotificationModeArg::Immediate => NetworkNotificationMode::Immediate,
        NetworkNotificationModeArg::Ignore => NetworkNotificationMode::Ignore,
        NetworkNotificationModeArg::Count => NetworkNotificationMode::Count,
        NetworkNotificationModeArg::Duration => NetworkNotificationMode::Duration,
    }
}

fn transport_paths(paths: &VaultPaths, wiki: Option<&str>) -> Result<VaultPaths, CliError> {
    let Some(id) = wiki else {
        return Ok(paths.clone());
    };
    let registry = WikiRegistry::user_default().map_err(CliError::operation)?;
    let registration = registry
        .show(&WikiId::parse(id).map_err(CliError::operation)?)
        .map_err(CliError::operation)?
        .registration;
    Ok(VaultPaths::new(&registration.path))
}

/// Binds every registered Git vault whose remote accepts the device key.
fn handle_bind_all(
    cli: &Cli,
    remote: &str,
    mode: vulcan_app::sync_transport::GitConfigMode,
    dry_run: bool,
) -> Result<(), CliError> {
    use std::path::Path;
    use vulcan_app::device_config::DeviceConfigStore;
    use vulcan_app::device_identity::DeviceIdentityStore;
    use vulcan_app::sync_state::SyncStateStore;
    use vulcan_app::sync_transport::probe_device_key;
    use vulcan_app::sync_transport_all::{bind_all, BindAllEnvironment, BindOutcome, BindVault};
    let remote = GitRemote::parse(remote).map_err(CliError::operation)?;
    let vaults = WikiRegistry::user_default()
        .map_err(CliError::operation)?
        .list(None)
        .map_err(CliError::operation)?
        .into_iter()
        .filter(|status| {
            status.available && status.registration.sync_backend.as_deref() == Some("git")
        })
        .map(|status| {
            let registration = status.registration;
            let paths = VaultPaths::new(&registration.path);
            let profile = cli
                .permissions
                .as_deref()
                .or(registration.permissions_profile.as_deref());
            let blocked = resolve_permission_profile(&paths, profile)
                .map_err(|error| error.to_string())
                .and_then(|selection| {
                    ProfilePermissionGuard::new(&paths, selection)
                        .check_git()
                        .map_err(|error| error.to_string())
                })
                .err();
            BindVault {
                wiki: registration.id.to_string(),
                paths,
                blocked,
            }
        })
        .collect::<Vec<_>>();
    let device_config = DeviceConfigStore::user_default()
        .and_then(|store| store.load())
        .map_err(CliError::operation)?;
    let state = SyncStateStore::user_default().map_err(CliError::operation)?;
    let executable = std::env::current_exe().map_err(CliError::operation)?;
    let identity = DeviceIdentityStore::user_default().map_err(CliError::operation)?;
    let probe = |remote: &str, dir: &Path| probe_device_key(&identity, remote, Some(dir));
    let environment = BindAllEnvironment {
        device_config: &device_config,
        identity: &identity,
        state: &state,
        executable: &executable,
        probe: &probe,
        remote_url_override: None,
    };
    let report =
        bind_all(&environment, &vaults, &remote, mode, dry_run).map_err(CliError::operation)?;
    if cli.output == OutputFormat::Json {
        print_json(&report)?;
    } else {
        for entry in &report.wikis {
            let verb = match (entry.outcome, report.dry_run) {
                (BindOutcome::Bound, true) => "would bind",
                (BindOutcome::Bound, false) => "bound",
                (BindOutcome::Already, _) => "already bound",
                (BindOutcome::Skipped, _) => "skipped",
                (BindOutcome::Failed, _) => "FAILED",
            };
            println!("{}: {verb}", entry.wiki);
            if let Some(reason) = &entry.reason {
                println!("  {reason}");
            }
        }
        println!(
            "\n{} bound, {} already bound, {} skipped, {} failed",
            report.bound, report.already, report.skipped, report.failed
        );
    }
    if report.failed > 0 {
        return Err(CliError::operation(format!(
            "{} of {} vaults failed; the others were not affected",
            report.failed,
            report.wikis.len()
        )));
    }
    Ok(())
}

fn handle_sync_transport(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncTransportCommand,
) -> Result<(), CliError> {
    use vulcan_app::sync_transport::{bind_transport, transport_status, unbind_transport};
    match command {
        SyncTransportCommand::Bind {
            wiki,
            all_wikis,
            remote,
            git_config,
            no_git_config,
            dry_run,
        } => {
            let mode = match (git_config, no_git_config) {
                (_, true) => vulcan_app::sync_transport::GitConfigMode::Skip,
                (true, false) => vulcan_app::sync_transport::GitConfigMode::Require,
                (false, false) => vulcan_app::sync_transport::GitConfigMode::Auto,
            };
            if *all_wikis {
                return handle_bind_all(cli, remote, mode, *dry_run);
            }
            let paths = transport_paths(paths, wiki.as_deref())?;
            let report =
                bind_transport(&paths, remote, mode, *dry_run).map_err(CliError::operation)?;
            match cli.output {
                OutputFormat::Json => print_json(&report),
                OutputFormat::Human | OutputFormat::Markdown => {
                    let verb = match (report.dry_run, report.changed) {
                        (_, false) => "already bound",
                        (true, true) => "would bind",
                        (false, true) => "bound",
                    };
                    println!("{verb} Git transport to device key {}", report.device_id);
                    if report.git_config_written {
                        println!("core.sshCommand set, so plain `git` in this repository uses the device key too");
                    }
                    if report.git_config_removed {
                        println!("removed the Vulcan-owned core.sshCommand");
                    }
                    if let Some(note) = &report.git_config_skipped {
                        println!("note: {note}");
                    }
                    Ok(())
                }
            }
        }
        SyncTransportCommand::Status { wiki } => {
            let paths = transport_paths(paths, wiki.as_deref())?;
            let report = transport_status(&paths).map_err(CliError::operation)?;
            match cli.output {
                OutputFormat::Json => print_json(&report),
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!("transport: {:?}", report.state);
                    if let Some(id) = &report.bound_device_id {
                        println!("device: {id}");
                    }
                    println!("core.sshCommand: {:?}", report.git_config);
                    if let Some(diagnostic) = &report.diagnostic {
                        println!("note: {diagnostic}");
                    }
                    Ok(())
                }
            }
        }
        SyncTransportCommand::Unbind { wiki, dry_run } => {
            let paths = transport_paths(paths, wiki.as_deref())?;
            let report = unbind_transport(&paths, *dry_run).map_err(CliError::operation)?;
            match cli.output {
                OutputFormat::Json => print_json(&report),
                OutputFormat::Human | OutputFormat::Markdown => {
                    if report.was_bound {
                        println!(
                            "{} Git transport binding{}",
                            if report.dry_run {
                                "would remove"
                            } else {
                                "removed"
                            },
                            if report.git_config_removed {
                                " and Vulcan-owned core.sshCommand"
                            } else {
                                ""
                            }
                        );
                    } else {
                        println!("no Git transport binding");
                    }
                    Ok(())
                }
            }
        }
    }
}

fn read_public_key_input(source: &std::path::Path) -> Result<String, CliError> {
    use std::io::Read;
    const LIMIT: u64 = 8 * 1024;
    let mut text = String::new();
    let read = if source == std::path::Path::new("-") {
        std::io::stdin().take(LIMIT + 1).read_to_string(&mut text)
    } else {
        std::fs::File::open(source).and_then(|file| file.take(LIMIT + 1).read_to_string(&mut text))
    };
    read.map_err(CliError::operation)?;
    if text.len() as u64 > LIMIT {
        return Err(CliError::operation("public key input is too large"));
    }
    Ok(text)
}

fn print_registration_change(
    output: OutputFormat,
    report: &RegistrationChangeReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    let verb = match (report.action, report.dry_run) {
        (RegistrationAction::Created, true) => "would register",
        (RegistrationAction::Created, false) => "registered",
        (RegistrationAction::Updated, true) => "would update",
        (RegistrationAction::Updated, false) => "updated",
        (RegistrationAction::Unchanged, _) => "unchanged:",
        (RegistrationAction::Removed, true) => "would remove",
        (RegistrationAction::Removed, false) => "removed",
    };
    println!("{verb} {}", report.device_id);
    if let Some(fingerprint) = &report.fingerprint {
        println!("  Key fingerprint: {fingerprint}");
    }
    if let Some(status) = report.status {
        println!("  Status: {}", registration_status_label(status));
    }
    Ok(())
}

fn registration_status_label(status: RegistrationStatus) -> &'static str {
    match status {
        RegistrationStatus::Placeholder => "placeholder (device has not synced yet)",
        RegistrationStatus::Registered => "registered",
        RegistrationStatus::Revoked => "revoked",
    }
}

/// The status plus whether the device's own signature backs the record.
fn entry_status_label(entry: &vulcan_app::sync_registration::RegistrationSummary) -> String {
    let base = registration_status_label(entry.status);
    match (entry.status, entry.signed) {
        (RegistrationStatus::Registered, true) => format!("{base}, signed by the device"),
        (RegistrationStatus::Registered, false) => {
            format!("{base}, unsigned (the device signs it on its next sync)")
        }
        _ => base.to_owned(),
    }
}

fn print_registrations(report: &RegistrationListReport) {
    println!();
    match report.observation {
        RegistrationObservation::Observed => println!("Registrations (remote): {}", report.count),
        RegistrationObservation::NotRequested => {
            println!(
                "Registrations (last fetched copies, --offline): {}",
                report.count
            );
        }
        RegistrationObservation::Unavailable => println!(
            "Registrations (remote unavailable; last fetched copies): {}",
            report.count
        ),
    }
    for entry in &report.registrations {
        println!(
            "  {}{} — {}",
            entry.device_id,
            if entry.current_device {
                " (this device)"
            } else {
                ""
            },
            entry_status_label(entry)
        );
        if let Some(label) = &entry.label {
            println!("    Label: {label}");
        }
        println!("    Key fingerprint: {}", entry.fingerprint);
    }
    for rejected in &report.rejected {
        println!("  ignored {}: {}", rejected.reference, rejected.reason);
    }
}

#[derive(Serialize)]
struct ForgeShowReport {
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<vulcan_app::sync_forge::ForgeConfig>,
    /// Whether the configured token variable is currently set; never its value.
    token_env_set: bool,
    /// Local OAuth login state; never a token.
    #[cfg(feature = "web")]
    #[serde(skip_serializing_if = "Option::is_none")]
    oauth: Option<vulcan_app::sync_forge::ForgeOAuthStatus>,
}

/// Explicit forge settings from the command line.
struct ForgeSetArgs<'a> {
    kind: crate::cli::ForgeKindArg,
    url: &'a str,
    repo: &'a str,
    token_env: Option<&'a str>,
    oauth_client_id: Option<&'a str>,
    dry_run: bool,
}

fn forge_kind(kind: crate::cli::ForgeKindArg) -> vulcan_app::sync_forge::ForgeKind {
    match kind {
        crate::cli::ForgeKindArg::Forgejo => vulcan_app::sync_forge::ForgeKind::Forgejo,
    }
}

fn handle_forge_set(
    cli: &Cli,
    paths: &VaultPaths,
    args: &ForgeSetArgs<'_>,
) -> Result<(), CliError> {
    use vulcan_app::sync_forge::{set_forge_config, ForgeConfig};
    let config = ForgeConfig::new(
        forge_kind(args.kind),
        args.url,
        args.repo,
        args.token_env,
        args.oauth_client_id,
    )
    .map_err(CliError::operation)?;
    let report = set_forge_config(paths, &config, args.dry_run).map_err(CliError::operation)?;
    match cli.output {
        OutputFormat::Json => print_json(&report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{} forge settings for {} at {}",
                match (report.dry_run, report.changed) {
                    (_, false) => "unchanged:",
                    (true, true) => "would save",
                    (false, true) => "saved",
                },
                config.repo,
                config.url
            );
            print_forge_credentials(&config);
            Ok(())
        }
    }
}

fn print_forge_credentials(config: &vulcan_app::sync_forge::ForgeConfig) {
    if let Some(client_id) = &config.oauth_client_id {
        println!("OAuth client ID: {client_id}");
    }
    if let Some(name) = &config.token_env {
        println!("API token is read from ${name}");
    }
}

fn handle_forge_init(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    target: &crate::SyncTargetArgs,
    request: &vulcan_app::sync_forge::ForgeInitRequest,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    let remote = GitRemote::parse(&target.remote).map_err(CliError::operation)?;
    let report = vulcan_app::sync_forge::forge_init(&paths, &remote, request)
        .map_err(CliError::operation)?;
    if cli.output == OutputFormat::Json {
        return print_json(&report);
    }
    match &report.derived {
        Some(derived) => println!(
            "Git remote `{}` implies forge {} and repository {}",
            report.remote, derived.url, derived.repo
        ),
        None => println!("Git remote `{}` does not imply a forge.", report.remote),
    }
    if let Some(shared) = &report.shared {
        println!("Shared settings on the remote (published by anyone who can push):");
        println!("  kind: {:?}", shared.settings.kind);
        if let Some(url) = &shared.settings.api_url {
            println!("  API URL: {url}");
        }
        if let Some(client_id) = &shared.settings.oauth_client_id {
            println!("  OAuth client ID: {client_id}");
        }
    }
    if let Some(config) = &report.config {
        println!(
            "{} forge settings: {:?} {} at {}",
            if report.saved {
                "Saved"
            } else if report.dry_run {
                "Would save"
            } else {
                "Already saved:"
            },
            config.kind,
            config.repo,
            config.url
        );
        print_forge_credentials(config);
    }
    if report.published {
        println!("Published the shared settings to the remote.");
    }
    if let Some(note) = &report.note {
        println!("{note}");
    }
    Ok(())
}

fn handle_forge_show(cli: &Cli, paths: &VaultPaths) -> Result<(), CliError> {
    use vulcan_app::sync_forge::show_forge_config;
    let config = show_forge_config(paths).map_err(CliError::operation)?;
    let report = ForgeShowReport {
        version: 1,
        token_env_set: config.as_ref().is_some_and(|config| {
            config
                .token_env
                .as_ref()
                .is_some_and(|name| std::env::var_os(name).is_some())
        }),
        #[cfg(feature = "web")]
        oauth: config.as_ref().and_then(|config| {
            vulcan_app::sync_forge::forge_oauth_status(config)
                .ok()
                .flatten()
        }),
        config,
    };
    match (cli.output, &report.config) {
        (OutputFormat::Json, _) => print_json(&report),
        (_, None) => {
            println!("No forge is configured. Use `vulcan sync forge set`.");
            Ok(())
        }
        (_, Some(config)) => {
            println!("forgejo {} at {}", config.repo, config.url);
            if let Some(client_id) = &config.oauth_client_id {
                println!("OAuth client ID: {client_id}");
                #[cfg(feature = "web")]
                match &report.oauth {
                    Some(status) if status.logged_in && status.access_token_expired => println!(
                        "OAuth login: access token expired{}",
                        if status.has_refresh_token {
                            " (refreshes automatically)"
                        } else {
                            "; run `vulcan sync forge login`"
                        }
                    ),
                    Some(status) if status.logged_in => println!(
                        "OAuth login: active, access token valid for {} more seconds",
                        status.expires_in_seconds.unwrap_or(0)
                    ),
                    _ => println!("OAuth login: not logged in; run `vulcan sync forge login`"),
                }
            }
            if let Some(name) = &config.token_env {
                println!(
                    "Token variable ${name}: {}",
                    if report.token_env_set {
                        "set"
                    } else {
                        "not set"
                    }
                );
            }
            Ok(())
        }
    }
}

#[cfg(feature = "web")]
fn handle_forge_login(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    no_browser: bool,
    timeout_seconds: u64,
) -> Result<(), CliError> {
    use vulcan_app::sync_forge::{forge_login, open_in_browser, show_forge_config};
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    let config = show_forge_config(&paths)
        .map_err(CliError::operation)?
        .ok_or_else(|| {
            CliError::operation(
                "no forge is configured for this vault; run `vulcan sync forge init`",
            )
        })?;
    let profile = cli
        .permissions
        .as_deref()
        .or(registration_profile.as_deref());
    let selection = resolve_permission_profile(&paths, profile).map_err(CliError::operation)?;
    ProfilePermissionGuard::new(&paths, selection)
        .check_network(&config.url)
        .map_err(CliError::operation)?;
    let announce = |url: &str| {
        // Progress goes to stderr so JSON output stays machine-readable.
        eprintln!("Approve the login in your browser. If it does not open, visit:\n  {url}\nWaiting for the redirect to 127.0.0.1 ...");
        if !no_browser {
            let _ = open_in_browser(url);
        }
    };
    let report = forge_login(&config, Duration::from_secs(timeout_seconds), &announce)
        .map_err(CliError::operation)?;
    match cli.output {
        OutputFormat::Json => print_json(&report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Logged in to {} (access token valid for {} s{}).",
                report.origin,
                report.expires_in_seconds,
                if report.has_refresh_token {
                    ", refreshes automatically"
                } else {
                    ""
                }
            );
            Ok(())
        }
    }
}

#[cfg(not(feature = "web"))]
fn handle_forge_login(
    _cli: &Cli,
    _selected_paths: &VaultPaths,
    _wiki: Option<&str>,
    _no_browser: bool,
    _timeout_seconds: u64,
) -> Result<(), CliError> {
    Err(CliError::operation(
        "this build has no forge support; rebuild with the `web` feature",
    ))
}

#[cfg(feature = "web")]
fn handle_forge_logout(cli: &Cli, paths: &VaultPaths) -> Result<(), CliError> {
    use vulcan_app::sync_forge::{forge_logout, show_forge_config};
    let config = show_forge_config(paths)
        .map_err(CliError::operation)?
        .ok_or_else(|| CliError::operation("no forge is configured for this vault"))?;
    let report = forge_logout(&config).map_err(CliError::operation)?;
    match cli.output {
        OutputFormat::Json => print_json(&report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{}",
                if report.removed {
                    "Removed the local login."
                } else {
                    "There was no local login."
                }
            );
            println!("{}", report.note);
            Ok(())
        }
    }
}

#[cfg(not(feature = "web"))]
fn handle_forge_logout(_cli: &Cli, _paths: &VaultPaths) -> Result<(), CliError> {
    Err(CliError::operation(
        "this build has no forge support; rebuild with the `web` feature",
    ))
}

fn handle_forge_clear(cli: &Cli, paths: &VaultPaths, dry_run: bool) -> Result<(), CliError> {
    use vulcan_app::sync_forge::clear_forge_config;
    let report = clear_forge_config(paths, dry_run).map_err(CliError::operation)?;
    match cli.output {
        OutputFormat::Json => print_json(&report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{}",
                match (report.dry_run, report.changed) {
                    (_, false) => "no forge settings were saved",
                    (true, true) => "would remove the saved forge settings",
                    (false, true) => "removed the saved forge settings",
                }
            );
            Ok(())
        }
    }
}

/// Unpacks `sync forge init` and runs it.
fn handle_forge_init_command(
    cli: &Cli,
    selected_paths: &VaultPaths,
    command: &SyncForgeCommand,
) -> Result<(), CliError> {
    let SyncForgeCommand::Init {
        kind,
        url,
        repo,
        token_env,
        oauth_client_id,
        adopt,
        publish,
        allow_other_host,
        wiki,
        target,
        dry_run,
    } = command
    else {
        unreachable!("only `sync forge init` is routed here")
    };
    handle_forge_init(
        cli,
        selected_paths,
        wiki.as_deref(),
        target,
        &vulcan_app::sync_forge::ForgeInitRequest {
            kind: kind.map(forge_kind),
            url: url.clone(),
            repo: repo.clone(),
            token_env: token_env.clone(),
            oauth_client_id: oauth_client_id.clone(),
            adopt: *adopt,
            publish: *publish,
            allow_other_host: *allow_other_host,
            dry_run: *dry_run,
        },
    )
}

fn handle_sync_forge(
    cli: &Cli,
    selected_paths: &VaultPaths,
    command: &SyncForgeCommand,
) -> Result<(), CliError> {
    match command {
        SyncForgeCommand::Set {
            kind,
            url,
            repo,
            token_env,
            oauth_client_id,
            wiki,
            dry_run,
        } => {
            let paths = transport_paths(selected_paths, wiki.as_deref())?;
            handle_forge_set(
                cli,
                &paths,
                &ForgeSetArgs {
                    kind: *kind,
                    url,
                    repo,
                    token_env: token_env.as_deref(),
                    oauth_client_id: oauth_client_id.as_deref(),
                    dry_run: *dry_run,
                },
            )
        }
        SyncForgeCommand::Init { .. } => handle_forge_init_command(cli, selected_paths, command),
        SyncForgeCommand::Show { wiki } => {
            let paths = transport_paths(selected_paths, wiki.as_deref())?;
            handle_forge_show(cli, &paths)
        }
        SyncForgeCommand::Login {
            wiki,
            no_browser,
            timeout_seconds,
        } => handle_forge_login(
            cli,
            selected_paths,
            wiki.as_deref(),
            *no_browser,
            *timeout_seconds,
        ),
        SyncForgeCommand::AuthorizeSelf {
            wiki,
            label,
            dry_run,
        } => handle_forge_authorize_self(
            cli,
            selected_paths,
            wiki.as_deref(),
            label.as_deref(),
            *dry_run,
        ),
        SyncForgeCommand::Logout { wiki } => {
            let paths = transport_paths(selected_paths, wiki.as_deref())?;
            handle_forge_logout(cli, &paths)
        }
        SyncForgeCommand::Clear { wiki, dry_run } => {
            let paths = transport_paths(selected_paths, wiki.as_deref())?;
            handle_forge_clear(cli, &paths, *dry_run)
        }
        SyncForgeCommand::Sync {
            wiki,
            all_wikis,
            target,
            dry_run,
        } => handle_forge_sync(
            cli,
            selected_paths,
            wiki.as_deref(),
            *all_wikis,
            target,
            *dry_run,
        ),
    }
}

#[cfg(feature = "web")]
fn handle_forge_sync(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    all_wikis: bool,
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    if all_wikis {
        return handle_forge_sync_all(cli, target, dry_run);
    }
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    let report = forge_sync_one(
        cli,
        &paths,
        registration_profile.as_deref(),
        target,
        dry_run,
    )?;
    print_forge_sync(cli.output, &report)
}

/// One vault's forge sync: its own settings, permission profile, and token.
#[cfg(feature = "web")]
fn forge_sync_one(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<vulcan_app::sync_forge::ForgeSyncReport, CliError> {
    let (config, adapter) = forge_adapter(cli, paths, registration_profile, true)?;
    let remote = GitRemote::parse(&target.remote).map_err(CliError::operation)?;
    vulcan_app::sync_forge::forge_sync(paths, &remote, &adapter, &config.repo, dry_run)
        .map_err(CliError::operation)
}

/// The vault's forge settings and an authenticated adapter: permissions are
/// checked, then the credential chain (OAuth login first, then the token
/// variable) is resolved. `check_git` is false for commands that never touch
/// the Git remote.
#[cfg(feature = "web")]
pub(crate) fn forge_adapter(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
    check_git: bool,
) -> Result<
    (
        vulcan_app::sync_forge::ForgeConfig,
        vulcan_app::sync_forge::ForgejoDeployKeys,
    ),
    CliError,
> {
    use vulcan_app::sync_forge::{resolve_forge_credential, show_forge_config, ForgejoDeployKeys};
    let config = show_forge_config(paths)
        .map_err(CliError::operation)?
        .ok_or_else(|| {
            CliError::operation(
                "no forge is configured for this vault; run `vulcan sync forge init`",
            )
        })?;
    let profile = cli.permissions.as_deref().or(registration_profile);
    let selection = resolve_permission_profile(paths, profile).map_err(CliError::operation)?;
    let guard = ProfilePermissionGuard::new(paths, selection);
    if check_git {
        guard.check_git().map_err(CliError::operation)?;
    }
    guard
        .check_network(&config.url)
        .map_err(CliError::operation)?;
    let credential = resolve_forge_credential(&config, &|name| std::env::var(name).ok())
        .map_err(CliError::operation)?;
    let adapter = match config.kind {
        vulcan_app::sync_forge::ForgeKind::Forgejo => {
            ForgejoDeployKeys::new(&config, credential.token.as_str(), Duration::from_secs(30))
                .map_err(CliError::operation)?
        }
    };
    Ok((config, adapter))
}

#[cfg(feature = "web")]
fn handle_forge_authorize_self(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    label: Option<&str>,
    dry_run: bool,
) -> Result<(), CliError> {
    use vulcan_app::device_identity::{DeviceIdentityStatus, DeviceIdentityStore};
    use vulcan_app::sync_forge::AuthorizeAction;
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    let identity = DeviceIdentityStore::user_default().map_err(CliError::operation)?;
    let report = identity.inspect();
    let (DeviceIdentityStatus::Ready, Some(device_id)) = (report.status, report.device_id) else {
        return Err(CliError::operation(
            "this installation has no usable device identity; run `vulcan device init`",
        ));
    };
    let public_key = identity.public_key().map_err(CliError::operation)?;
    let (config, adapter) = forge_adapter(cli, &paths, registration_profile.as_deref(), false)?;
    let report = vulcan_app::sync_forge::authorize_device(
        &adapter,
        &config.repo,
        &device_id,
        &public_key,
        label,
        dry_run,
    )
    .map_err(CliError::operation)?;
    if cli.output == OutputFormat::Json {
        return print_json(&report);
    }
    println!(
        "{} for {}: {} ({})",
        match (report.action, report.dry_run) {
            (AuthorizeAction::AlreadyPresent, _) => "Already authorized",
            (AuthorizeAction::Added, true) => "Would authorize",
            (AuthorizeAction::Added, false) => "Authorized",
            (AuthorizeAction::Replaced, true) => "Would replace a read-only key",
            (AuthorizeAction::Replaced, false) => "Replaced a read-only key",
        },
        report.repo,
        report.device_id,
        report.fingerprint
    );
    Ok(())
}

#[cfg(not(feature = "web"))]
fn handle_forge_authorize_self(
    _cli: &Cli,
    _selected_paths: &VaultPaths,
    _wiki: Option<&str>,
    _label: Option<&str>,
    _dry_run: bool,
) -> Result<(), CliError> {
    Err(CliError::operation(
        "this build has no forge support; rebuild with the `web` feature",
    ))
}

#[cfg(feature = "web")]
#[derive(Serialize)]
struct ForgeSyncAllEntry {
    wiki_id: String,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<vulcan_app::sync_forge::ForgeSyncReport>,
}

#[cfg(feature = "web")]
#[derive(Serialize)]
struct ForgeSyncAllReport {
    version: u32,
    dry_run: bool,
    wikis: Vec<ForgeSyncAllEntry>,
    synced: usize,
    skipped: usize,
    failed: usize,
}

/// Iterates registered Git vaults. Each is independent: a failure in one never
/// affects another, and no vault's settings or approvals apply to another.
#[cfg(feature = "web")]
fn handle_forge_sync_all(
    cli: &Cli,
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    use vulcan_app::sync_forge::show_forge_config;
    let registrations = WikiRegistry::user_default()
        .map_err(CliError::operation)?
        .list(None)
        .map_err(CliError::operation)?;
    let mut wikis = Vec::new();
    for status in registrations {
        let registration = status.registration;
        let mut entry = ForgeSyncAllEntry {
            wiki_id: registration.id.to_string(),
            outcome: "skipped",
            reason: None,
            report: None,
        };
        if !status.available {
            entry.reason = Some("registered vault path is unavailable".to_owned());
        } else if registration.sync_backend.as_deref() != Some("git") {
            entry.reason = Some("registration does not use the Git sync backend".to_owned());
        } else {
            let paths = VaultPaths::new(&registration.path);
            match show_forge_config(&paths) {
                Ok(None) => entry.reason = Some("no forge is configured".to_owned()),
                Err(error) => {
                    entry.outcome = "failed";
                    entry.reason = Some(error.to_string());
                }
                Ok(Some(_)) => match forge_sync_one(
                    cli,
                    &paths,
                    registration.permissions_profile.as_deref(),
                    target,
                    dry_run,
                ) {
                    Ok(report) => {
                        entry.outcome = if report.failed > 0 {
                            "failed"
                        } else {
                            "synced"
                        };
                        entry.report = Some(report);
                    }
                    Err(error) => {
                        entry.outcome = "failed";
                        entry.reason = Some(error.to_string());
                    }
                },
            }
        }
        wikis.push(entry);
    }
    let count = |outcome: &str| {
        wikis
            .iter()
            .filter(|entry| entry.outcome == outcome)
            .count()
    };
    let report = ForgeSyncAllReport {
        version: 1,
        dry_run,
        synced: count("synced"),
        skipped: count("skipped"),
        failed: count("failed"),
        wikis,
    };
    if cli.output == OutputFormat::Json {
        print_json(&report)?;
    } else {
        for entry in &report.wikis {
            println!("\nWiki {} — {}", entry.wiki_id, entry.outcome);
            if let Some(reason) = &entry.reason {
                println!("  {reason}");
            }
            if let Some(single) = &entry.report {
                print_forge_sync(OutputFormat::Human, single)?;
            }
        }
        println!(
            "\n{} synced, {} skipped, {} failed",
            report.synced, report.skipped, report.failed
        );
    }
    if report.failed > 0 {
        return Err(CliError::operation(format!(
            "{} of {} vaults failed; the others were not affected",
            report.failed,
            report.wikis.len()
        )));
    }
    Ok(())
}

#[cfg(not(feature = "web"))]
fn handle_forge_sync(
    _cli: &Cli,
    _selected_paths: &VaultPaths,
    _wiki: Option<&str>,
    _all_wikis: bool,
    _target: &crate::SyncTargetArgs,
    _dry_run: bool,
) -> Result<(), CliError> {
    Err(CliError::operation(
        "this build has no forge support; rebuild with the `web` feature",
    ))
}

#[cfg(feature = "web")]
fn print_forge_sync(
    output: OutputFormat,
    report: &vulcan_app::sync_forge::ForgeSyncReport,
) -> Result<(), CliError> {
    use vulcan_app::sync_forge::{ForgeSyncAction, ForgeSyncResult};
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Deploy keys for {} {}",
        report.repo,
        if report.dry_run { "(dry run)" } else { "" }
    );
    if report.entries.is_empty() {
        println!("No device registrations to install.");
    }
    for entry in &report.entries {
        let action = match (entry.action, entry.result) {
            (ForgeSyncAction::Add, ForgeSyncResult::Planned) => "would add",
            (ForgeSyncAction::Add, ForgeSyncResult::Applied) => "added",
            (ForgeSyncAction::Replace, ForgeSyncResult::Planned) => "would replace",
            (ForgeSyncAction::Replace, ForgeSyncResult::Applied) => "replaced",
            (ForgeSyncAction::Remove, ForgeSyncResult::Planned) => "would remove",
            (ForgeSyncAction::Remove, ForgeSyncResult::Applied) => "removed",
            (_, ForgeSyncResult::Failed) => "FAILED",
            (ForgeSyncAction::Present, _) => "present",
            (ForgeSyncAction::AlreadyAbsent, _) => "already absent",
            (ForgeSyncAction::ForeignKeyKept, _) => "left alone",
            _ => "unchanged",
        };
        println!("  {action}: {} ({})", entry.device_id, entry.fingerprint);
        if let Some(detail) = &entry.detail {
            println!("    {detail}");
        }
    }
    for orphan in &report.orphans {
        println!(
            "  orphan (not removed): key {} \"{}\" matches no registration; revoke or unregister the device, or remove it at the forge",
            orphan.key_id, orphan.title
        );
    }
    if report.foreign_keys > 0 {
        println!(
            "{} other deploy key(s) are not managed by Vulcan and were not touched.",
            report.foreign_keys
        );
    }
    if report.ignored_registrations > 0 {
        println!(
            "{} malformed registration(s) were ignored.",
            report.ignored_registrations
        );
    }
    if report.failed > 0 {
        println!(
            "{} key change(s) failed; re-run to converge.",
            report.failed
        );
    }
    Ok(())
}

#[cfg(all(test, feature = "web"))]
mod semantic_agent_selection_tests {
    use super::{select_semantic_agent, SemanticAgentSelection};
    use vulcan_daemon::registry::DaemonAgentConfig;

    fn configured() -> DaemonAgentConfig {
        DaemonAgentConfig {
            base_url: "https://openrouter.ai/api/v1".to_string(),
            model: "anthropic/claude-haiku-5.5".to_string(),
            api_key_env: Some("OPENROUTER_API_KEY".to_string()),
        }
    }

    #[test]
    fn configured_semantic_agent_supplies_every_omitted_value() {
        let agent = configured();
        assert_eq!(
            select_semantic_agent(None, None, None, Some(&agent)).expect("selection"),
            SemanticAgentSelection {
                base_url: agent.base_url.clone(),
                model: agent.model.clone(),
                api_key_env: agent.api_key_env.clone(),
            }
        );
        let overridden =
            select_semantic_agent(None, Some("other/model"), Some("OTHER_KEY"), Some(&agent))
                .expect("selection");
        assert_eq!(overridden.base_url, agent.base_url);
        assert_eq!(overridden.model, "other/model");
        assert_eq!(overridden.api_key_env.as_deref(), Some("OTHER_KEY"));
    }

    #[test]
    fn explicit_base_url_never_inherits_the_configured_agent() {
        let agent = configured();
        assert!(select_semantic_agent(Some("http://other/v1"), None, None, Some(&agent)).is_err());
        let explicit =
            select_semantic_agent(Some("http://other/v1"), Some("local"), None, Some(&agent))
                .expect("selection");
        assert_eq!(explicit.base_url, "http://other/v1");
        assert_eq!(explicit.api_key_env, None);
    }

    #[test]
    fn unconfigured_agent_requires_a_model_and_defaults_to_local() {
        assert!(select_semantic_agent(None, None, None, None).is_err());
        let local = select_semantic_agent(None, Some("llama"), None, None).expect("selection");
        assert_eq!(local.base_url, "http://localhost:11434/v1");
    }
}
