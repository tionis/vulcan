use crate::editor::open_paths_in_editor;
use crate::output::print_json;
use crate::{
    selected_permission_guard, Cli, CliError, OutputFormat, SemanticGroupingArg,
    SyncCheckpointKindArg, SyncCommand, SyncConflictSideArg, SyncSelectionArgs, TermuxNetworkArg,
};
use serde::Serialize;
use vulcan_app::sync::{
    doctor_git_vault_for_platform, sync_git_vault, GitPlatformProfile, GitRefName, GitRemote,
    GitSyncAction, GitSyncOptions, SyncDoctorReport, SyncDoctorSeverity, VaultSyncReport,
};
use vulcan_app::sync_checkpoints::{
    create_sync_checkpoint, SyncCheckpointKind, SyncCheckpointOptions, SyncCheckpointReport,
};
use vulcan_app::sync_conflicts::{
    get_sync_conflict, list_sync_conflicts, resolve_sync_conflict, ResolveSyncConflictOptions,
    ResolveSyncConflictReport, SyncConflictDetailReport, SyncConflictListReport,
    SyncConflictResolutionSide,
};
use vulcan_app::sync_proposals::{
    approve_resolution_proposal, create_resolution_proposal, prepare_editor_resolution,
    preview_patch_resolution, preview_supplied_resolution, reject_resolution_proposal,
    resolution_paths_from_patch, ApproveResolutionProposalOptions, ApproveResolutionProposalReport,
    EditorResolutionPlan, PatchResolutionPreviewReport, RejectResolutionProposalReport,
    ResolutionAgentPathOutput, ResolutionProposalOptions, SuppliedResolutionPreviewReport,
    SuppliedResolutionProvider,
};
#[cfg(feature = "web")]
use vulcan_app::sync_proposals::{
    create_and_auto_accept_resolution_proposal, AutoAcceptResolutionProposalReport,
    OpenAiCompatibleResolutionProvider, ResolutionProposal,
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
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_daemon::registry::{UpdateWikiRequest, WikiId, WikiRegistration, WikiRegistry};
use vulcan_daemon::sync::{sync_registered_wikis, RegisteredSyncReport, RegisteredSyncSelection};
use vulcan_daemon::termux_scheduler::{
    apply_termux_sync, load_termux_sync_plan, plan_termux_sync, TermuxNetwork, TermuxSyncAction,
    TermuxSyncInstallOptions, TermuxSyncReport,
};

pub(crate) fn handle_sync_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Result<(), CliError> {
    if let Some(result) = handle_non_cycle_sync_command(cli, paths, command) {
        return result;
    }
    let (options, selection) = match command {
        SyncCommand::Run {
            selection,
            target,
            max_retries,
            dry_run,
        } => (
            GitSyncOptions {
                remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
                live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
                max_retries: *max_retries,
                dry_run: *dry_run,
                ..GitSyncOptions::default()
            },
            registered_selection(selection)?,
        ),
        SyncCommand::Status { selection, target } => (
            GitSyncOptions {
                remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
                live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
                dry_run: true,
                ..GitSyncOptions::default()
            },
            registered_selection(selection)?,
        ),
        _ => unreachable!(),
    };
    if let Some(selection) = selection {
        let registry = WikiRegistry::user_default().map_err(CliError::operation)?;
        let report =
            sync_registered_wikis(&registry, &selection, &options, cli.permissions.as_deref())
                .map_err(CliError::operation)?;
        print_registered_sync_report(cli.output, &report)?;
        if report.failed > 0 || report.conflicted > 0 {
            return Err(CliError::issues(format!(
                "{} registered sync operation(s) failed and {} remain conflicted",
                report.failed, report.conflicted
            )));
        }
        return Ok(());
    }
    selected_permission_guard(cli, paths)?
        .check_git()
        .map_err(CliError::operation)?;
    let report = sync_git_vault(paths, &options).map_err(CliError::operation)?;
    print_sync_report(cli.output, &report)
}

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
    if let Some(result) = handle_termux_sync_command(cli, command) {
        return Some(result);
    }
    let result = match command {
        SyncCommand::Pause { wiki, dry_run } => {
            set_automatic_sync(cli.output, paths, wiki.as_deref(), true, *dry_run)
        }
        SyncCommand::Resume { wiki, dry_run } => {
            set_automatic_sync(cli.output, paths, wiki.as_deref(), false, *dry_run)
        }
        SyncCommand::Doctor { wiki, target } => {
            run_sync_doctor(cli, paths, wiki.as_deref(), target)
        }
        SyncCommand::Conflicts { conflict_id, wiki } => {
            run_sync_conflicts(cli, paths, wiki.as_deref(), conflict_id.as_deref())
        }
        SyncCommand::Propose {
            conflict_id,
            wiki,
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
            base_url,
            model,
            api_key_env.as_deref(),
            context,
            *allow_broad_context,
            *auto_accept,
            target,
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
        SyncCommand::Run { .. } | SyncCommand::Status { .. } => return None,
        SyncCommand::TermuxInstall { .. } | SyncCommand::TermuxUninstall { .. } => {
            unreachable!("Termux commands are dispatched before the general sync match")
        }
        SyncCommand::RetentionPlan { .. } | SyncCommand::RetentionApply { .. } => {
            unreachable!("retention commands are dispatched before the general sync match")
        }
    };
    Some(result)
}

fn handle_termux_sync_command(cli: &Cli, command: &SyncCommand) -> Option<Result<(), CliError>> {
    match command {
        SyncCommand::TermuxInstall {
            wiki,
            period_minutes,
            network,
            charging,
            allow_low_battery,
            no_persist,
            job_id,
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
            },
            *dry_run,
        )),
        SyncCommand::TermuxUninstall { wiki, dry_run } => {
            Some(uninstall_termux_sync(cli.output, wiki, *dry_run))
        }
        _ => None,
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
            base_url,
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
            base_url,
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
        let proposal = create_resolution_proposal(
            &paths,
            conflict_id,
            &proposal_options,
            &provider,
            &cancellation,
        )
        .map_err(CliError::operation)?;
        print_resolution_proposal(cli.output, &proposal)
    }
}

#[cfg(not(feature = "web"))]
#[allow(clippy::too_many_arguments)]
fn run_sync_propose(
    _cli: &Cli,
    _selected_paths: &VaultPaths,
    _wiki: Option<&str>,
    _conflict_id: &str,
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

#[cfg(feature = "web")]
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
    base_url: &str,
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
    base_url: &str,
    model: Option<&str>,
    api_key_env: Option<&str>,
) -> Result<SemanticPlanReport, CliError> {
    let model = model.ok_or_else(|| CliError::operation("--agent requires --model"))?;
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

#[cfg(not(feature = "web"))]
fn create_agent_semantic_plan(
    _cli: &Cli,
    _paths: &VaultPaths,
    _registration_profile: Option<&str>,
    _options: &SemanticPlanOptions,
    _base_url: &str,
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
    base_url: &str,
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
    base_url: &str,
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
    let model = model.ok_or_else(|| CliError::operation("--agent requires --model"))?;
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
    _base_url: &str,
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

fn run_sync_resolve(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    conflict_id: &str,
    resolution: CliResolution<'_>,
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
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
            target,
            dry_run,
        ),
        CliResolution::Patch(source) => run_patch_resolution(
            cli,
            &paths,
            registration_profile.as_deref(),
            conflict_id,
            source,
            target,
            dry_run,
        ),
        CliResolution::Editor => run_editor_resolution(
            cli,
            &paths,
            registration_profile.as_deref(),
            conflict_id,
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

fn run_file_resolution(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
    conflict_id: &str,
    specifications: &[String],
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    let (proposal_options, approval_options) =
        manual_resolution_options(cli, registration_profile, target, dry_run)?;
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
    )
}

fn run_patch_resolution(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
    conflict_id: &str,
    source: &str,
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    let patch = std::fs::read(source).map_err(|error| {
        CliError::operation(format!("cannot read resolution patch `{source}`: {error}"))
    })?;
    let (proposal_options, approval_options) =
        manual_resolution_options(cli, registration_profile, target, dry_run)?;
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
    let supplied = resolution_paths_from_patch(
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
        supplied,
    )
}

fn run_editor_resolution(
    cli: &Cli,
    paths: &VaultPaths,
    registration_profile: Option<&str>,
    conflict_id: &str,
    target: &crate::SyncTargetArgs,
    dry_run: bool,
) -> Result<(), CliError> {
    let (proposal_options, approval_options) =
        manual_resolution_options(cli, registration_profile, target, dry_run)?;
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
) -> Result<(), CliError> {
    let provider = SuppliedResolutionProvider::new(supplied);
    let cancellation = vulcan_app::sync::SyncCancellationToken::default();
    let proposal = create_resolution_proposal(
        paths,
        conflict_id,
        proposal_options,
        &provider,
        &cancellation,
    )
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
) -> Result<(), CliError> {
    let (paths, registration_profile, registration_platform) =
        resolve_sync_paths(selected_paths, wiki)?;
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
    let report = doctor_git_vault_for_platform(&paths, &options, platform);
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

fn run_sync_conflicts(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    conflict_id: Option<&str>,
) -> Result<(), CliError> {
    let (paths, registration_profile, _) = resolve_sync_paths(selected_paths, wiki)?;
    check_sync_permission(cli, &paths, registration_profile.as_deref())?;
    if let Some(conflict_id) = conflict_id {
        let report = get_sync_conflict(&paths, conflict_id).map_err(CliError::operation)?;
        print_sync_conflict_detail(cli.output, &report)
    } else {
        let report = list_sync_conflicts(&paths).map_err(CliError::operation)?;
        print_sync_conflict_list(cli.output, &report)
    }
}

fn print_sync_conflict_list(
    output: OutputFormat,
    report: &SyncConflictListReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!("Preserved sync conflicts: {}", report.count);
    for conflict in &report.conflicts {
        println!(
            "{}\t{:?}\t{}",
            conflict.id,
            conflict.resolution,
            conflict.paths.join(", ")
        );
    }
    Ok(())
}

fn print_sync_conflict_detail(
    output: OutputFormat,
    report: &SyncConflictDetailReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!("Conflict {} ({:?})", report.record.id, report.resolution);
    println!("Local:  {}", report.record.local_revision);
    println!("Remote: {}", report.record.remote_revision);
    if let Some(base) = &report.record.base_revision {
        println!("Base:   {base}");
    }
    for path in &report.record.paths {
        println!("Path:   {}", path.path);
    }
    Ok(())
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
    println!(
        "Registered sync {}: {} succeeded, {} conflicted, {} failed",
        report.selection, report.succeeded, report.conflicted, report.failed
    );
    for item in &report.items {
        if let Some(sync) = &item.report {
            println!(
                "{}\t{:?}\t{}",
                item.wiki_id,
                sync.sync.outcome,
                item.path.display()
            );
        } else if let Some(error) = &item.error {
            println!("{}\terror\t{}: {error}", item.wiki_id, item.path.display());
        }
    }
    Ok(())
}

fn print_sync_report(output: OutputFormat, report: &VaultSyncReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Sync {:?}: {} -> {}",
                report.sync.outcome, report.sync.remote, report.sync.refs.live
            );
            if let Some(accepted) = &report.sync.accepted {
                println!("Accepted: {accepted}");
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
                    vulcan_app::sync::GitSyncPauseReason::StagedChanges => {
                        "the normal Git index contains staged changes".to_string()
                    }
                };
                println!("Paused: {detail}. Captured work remains reachable.");
            }
            if let Some(refresh) = &report.cache_refresh {
                println!(
                    "Cache refreshed: {} added, {} updated, {} deleted",
                    refresh.added, refresh.updated, refresh.deleted
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
