use crate::cli::ManagedDirectoryProfileArg;
use crate::output::print_json;
use crate::{Cli, CliError, ClonePlatformArg, OutputFormat, VaultCommand};
use serde::Serialize;
use std::path::Path;
use vulcan_app::sync::GitPlatformProfile;
use vulcan_app::sync_device_files::{ignore_device_files_at_setup, IgnoreDeviceFilesReport};
use vulcan_core::vault_discovery::VAULT_POINTER_FILE_NAME;
use vulcan_core::VaultPaths;
use vulcan_daemon::clone::{
    clone_registered_wiki_with_ssh_command, recover_registered_wiki_git, CloneWikiReport,
    CloneWikiRequest, RecoverWikiGitReport, RecoverWikiGitRequest,
};
use vulcan_daemon::registry::{
    AddWikiRequest, ManagedDirectoryProfile, UpdateWikiRequest, WikiId, WikiRegistration,
    WikiRegistrationStatus, WikiRegistry,
};
use vulcan_daemon::vault_health::{vault_health, VaultHealth};

#[derive(Debug, Serialize)]
struct VaultMutationReport<'a> {
    action: &'static str,
    dry_run: bool,
    registry_path: &'a Path,
    wiki: &'a WikiRegistration,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<vulcan_daemon::registry::ManagedDirectoryCapabilities>,
    #[serde(flatten)]
    setup: &'a SetupSteps,
}

/// The steps that follow registering a vault for sync. None of them fails
/// the add or clone; an error is reported next to the registration.
#[derive(Debug, Default, Serialize)]
struct SetupSteps {
    #[serde(skip_serializing_if = "Option::is_none")]
    enroll: Option<vulcan_app::vault_enroll::EnrollReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enroll_error: Option<String>,
    /// `.gitignore` entries for Obsidian's per-device files.
    #[serde(skip_serializing_if = "Option::is_none")]
    device_files: Option<IgnoreDeviceFilesReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_files_error: Option<String>,
}

impl SetupSteps {
    /// Ignores Obsidian's per-device files in a vault that is set up for Git
    /// sync, unless `keep` asks to go on syncing them. Setting up sync is the
    /// explicit moment for this; later syncs never edit the `.gitignore`.
    fn ignore_device_files(&mut self, wiki: &WikiRegistration, keep: bool, dry_run: bool) {
        if keep || wiki.sync_backend.as_deref() != Some("git") {
            return;
        }
        match ignore_device_files_at_setup(&VaultPaths::new(&wiki.path), dry_run) {
            Ok(report) => self.device_files = report,
            Err(error) => self.device_files_error = Some(error.to_string()),
        }
    }

    fn print(&self, wiki: &str) {
        print_enrollment(self.enroll.as_ref(), self.enroll_error.as_deref(), wiki);
        if let Some(report) = self.device_files.as_ref().filter(|r| !r.added.is_empty()) {
            println!(
                "{} {} to {} so each device keeps its own Obsidian layout (--keep-device-files skips this).",
                if report.dry_run { "Would add" } else { "Added" },
                report.added.join(", "),
                report.gitignore
            );
        }
        if let Some(error) = &self.device_files_error {
            println!("Could not ignore per-device Obsidian files: {error}");
            println!("Run `vulcan sync doctor {wiki} --fix` to retry.");
        }
    }
}

#[derive(Debug, Serialize)]
struct VaultListReport<'a> {
    registry_path: &'a Path,
    group: Option<&'a str>,
    /// Whether a daemon answered; without one, health comes from durable
    /// state alone.
    daemon_running: bool,
    wikis: Vec<VaultWithHealth<'a>>,
}

#[derive(Debug, Serialize)]
struct VaultWithHealth<'a> {
    #[serde(flatten)]
    wiki: &'a WikiRegistrationStatus,
    health: VaultHealth,
}

/// Each registration's health, read from the daemon's operational status:
/// live from a running daemon, otherwise reconstructed from its ledger and
/// sync state. When even that cannot be read, health rests on the
/// registration alone.
fn registration_health(wikis: &[WikiRegistrationStatus]) -> (bool, Vec<VaultHealth>) {
    let status = vulcan_daemon::process::DaemonProcessContext::user_default()
        .ok()
        .and_then(|context| vulcan_daemon::process::daemon_status(&context).ok());
    let running = status.as_ref().is_some_and(|status| status.running);
    let health = wikis
        .iter()
        .map(|wiki| {
            let operational = status.as_ref().and_then(|status| {
                status
                    .wiki_statuses
                    .iter()
                    .find(|candidate| candidate.wiki_id == wiki.registration.id.as_str())
            });
            vault_health(wiki, operational)
        })
        .collect();
    (running, health)
}

fn health_label(health: &VaultHealth) -> String {
    let state = serde_json::to_value(health.state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
        .replace('_', " ");
    if health.unresolved_conflicts > 0 {
        format!("{state} ({} conflicts)", health.unresolved_conflicts)
    } else {
        state
    }
}

pub(crate) fn handle_vault_command(cli: &Cli, command: &VaultCommand) -> Result<(), CliError> {
    let registry = WikiRegistry::user_default().map_err(CliError::operation)?;
    match command {
        VaultCommand::Clone { .. } => handle_clone(cli, &registry, command),
        VaultCommand::Enroll { .. } => {
            crate::commands::enroll::handle_vault_enroll(cli, &registry, command)
        }
        VaultCommand::RecoverGit {
            id,
            remote,
            dry_run,
        } => {
            let report = recover_registered_wiki_git(
                &registry,
                &RecoverWikiGitRequest {
                    id: parse_id(id)?,
                    source: remote.clone(),
                },
                *dry_run,
            )
            .map_err(CliError::operation)?;
            print_recovery(cli.output, &report)
        }
        VaultCommand::Add { .. } => handle_add(cli, &registry, command),
        VaultCommand::List { group } => {
            let wikis = registry
                .list(group.as_deref())
                .map_err(CliError::operation)?;
            print_list(cli.output, &registry, group.as_deref(), &wikis)
        }
        VaultCommand::Discover { dirs } => {
            let report = vulcan_daemon::vault_scan::discover_vaults(&registry, dirs)
                .map_err(CliError::operation)?;
            print_discovery(cli.output, &registry, dirs.is_empty(), &report)
        }
        VaultCommand::ScanDir(command) => handle_scan_dir_command(cli, &registry, command),
        VaultCommand::Show { id } => {
            let wiki = registry.show(&parse_id(id)?).map_err(CliError::operation)?;
            print_show(cli.output, &wiki)
        }
        VaultCommand::Set {
            id,
            profile,
            group,
            remove_group,
            permissions_profile,
            clear_permissions_profile,
            dry_run,
        } => {
            let permissions = if *clear_permissions_profile {
                Some(None)
            } else {
                permissions_profile.clone().map(Some)
            };
            if let Some(name) = permissions_profile.as_deref() {
                let wiki = registry.show(&parse_id(id)?).map_err(CliError::operation)?;
                check_device_profile(&wiki.registration.path, name)?;
            }
            let wiki = registry
                .update(
                    &parse_id(id)?,
                    &UpdateWikiRequest {
                        groups_to_add: group.clone(),
                        groups_to_remove: remove_group.clone(),
                        permissions_profile: permissions,
                        sync_paused: None,
                        profile: profile.map(managed_profile),
                        execution_trust: None,
                    },
                    *dry_run,
                )
                .map_err(CliError::operation)?;
            print_mutation(
                cli.output,
                "set",
                *dry_run,
                &registry,
                &wiki,
                &SetupSteps::default(),
            )
        }
        VaultCommand::Token(command) => handle_token_command(cli, &registry, command),
        VaultCommand::Remove { id, dry_run } => {
            let wiki = registry
                .remove(&parse_id(id)?, *dry_run)
                .map_err(CliError::operation)?;
            print_mutation(
                cli.output,
                "remove",
                *dry_run,
                &registry,
                &wiki,
                &SetupSteps::default(),
            )
        }
    }
}

/// A token in a listing, with the wiki it serves now.
#[derive(Debug, serde::Serialize)]
struct TokenListing {
    #[serde(flatten)]
    token: vulcan_daemon::api_tokens::ApiTokenSummary,
    /// The registration this token was issued for no longer exists.
    orphaned: bool,
}

fn handle_token_command(
    cli: &Cli,
    registry: &WikiRegistry,
    command: &crate::cli::VaultTokenCommand,
) -> Result<(), CliError> {
    use crate::cli::VaultTokenCommand;
    use vulcan_daemon::api_tokens::ApiTokenStore;

    let context = vulcan_daemon::process::DaemonProcessContext::user_default()
        .map_err(CliError::operation)?;
    let store = ApiTokenStore::at(&context.state_root);
    match command {
        VaultTokenCommand::Create { id, label, dry_run } => {
            let wiki = registry.show(&parse_id(id)?).map_err(CliError::operation)?;
            let created = store
                .create(
                    wiki.registration.registration_id,
                    wiki.registration.id.as_str(),
                    label.as_deref(),
                    *dry_run,
                )
                .map_err(CliError::operation)?;
            if cli.output == OutputFormat::Json {
                return print_json(&created);
            }
            match &created.token {
                Some(token) => {
                    println!(
                        "Created API token {} for `{}`.",
                        created.summary.id, wiki.registration.id
                    );
                    println!("{token}");
                    println!(
                        "This is the only time the token is shown; store it in the client now."
                    );
                }
                None => println!("Would create an API token for `{}`.", wiki.registration.id),
            }
            Ok(())
        }
        VaultTokenCommand::List { id } => list_tokens(cli, registry, &store, id.as_deref()),
        VaultTokenCommand::Revoke { token_id, dry_run } => {
            let id = ulid::Ulid::from_string(token_id).map_err(|error| {
                CliError::operation(format!("invalid API token ID `{token_id}`: {error}"))
            })?;
            let revoked = store.revoke(id, *dry_run).map_err(CliError::operation)?;
            if cli.output == OutputFormat::Json {
                return print_json(&serde_json::json!({
                    "revoked": revoked,
                    "dry_run": dry_run,
                }));
            }
            if *dry_run {
                println!(
                    "Would revoke API token {} for `{}`.",
                    revoked.id, revoked.wiki_id
                );
            } else {
                println!(
                    "Revoked API token {} for `{}`.",
                    revoked.id, revoked.wiki_id
                );
            }
            Ok(())
        }
    }
}

fn list_tokens(
    cli: &Cli,
    registry: &WikiRegistry,
    store: &vulcan_daemon::api_tokens::ApiTokenStore,
    id: Option<&str>,
) -> Result<(), CliError> {
    use vulcan_daemon::api_tokens::ApiTokenSummary;

    let registrations = registry.load().map_err(CliError::operation)?.vaults;
    let only = id.map(parse_id).transpose()?;
    let tokens = store
        .list()
        .map_err(CliError::operation)?
        .iter()
        .map(|record| {
            let current = registrations
                .iter()
                .find(|wiki| wiki.registration_id == record.registration_id);
            let mut token = ApiTokenSummary::from(record);
            if let Some(current) = current {
                token.wiki_id = current.id.to_string();
            }
            TokenListing {
                token,
                orphaned: current.is_none(),
            }
        })
        .filter(|listing| {
            only.as_ref()
                .is_none_or(|id| listing.token.wiki_id == id.as_str())
        })
        .collect::<Vec<_>>();
    if cli.output == OutputFormat::Json {
        return print_json(&tokens);
    }
    if tokens.is_empty() {
        println!("No API tokens.");
    }
    for listing in &tokens {
        println!(
            "{} {}{}{}",
            listing.token.id,
            listing.token.wiki_id,
            listing
                .token
                .label
                .as_deref()
                .map(|label| format!(" ({label})"))
                .unwrap_or_default(),
            if listing.orphaned {
                " [registration removed]"
            } else {
                ""
            }
        );
    }
    Ok(())
}

fn print_recovery(output: OutputFormat, report: &RecoverWikiGitReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.dry_run {
        println!(
            "Would recreate the detached Git directory for wiki `{}` after preserving {}",
            report.wiki.id,
            report.wiki.work_tree().display()
        );
    } else if let Some(recovery) = &report.recovery {
        println!(
            "Recovered wiki `{}`; preserved the materialized vault at {}",
            report.wiki.id, recovery.recovery_ref
        );
    }
    println!("Warning: {}", report.warning);
    Ok(())
}

/// A registration's profile is this device's limit for the vault, so it must
/// resolve from built-ins and the vault's `config.local.toml` alone.
fn check_device_profile(vault: &std::path::Path, name: &str) -> Result<(), CliError> {
    vulcan_core::resolve_device_permission_profile(&vulcan_core::VaultPaths::new(vault), Some(name))
        .map(|_| ())
        .map_err(CliError::operation)
}

fn handle_add(cli: &Cli, registry: &WikiRegistry, command: &VaultCommand) -> Result<(), CliError> {
    let VaultCommand::Add {
        id,
        path,
        profile,
        group,
        git_dir,
        permissions_profile,
        sync_backend,
        no_sync,
        no_device_key,
        keep_device_files,
        login,
        dry_run,
    } = command
    else {
        unreachable!("add handler requires an add command")
    };
    if let Some(name) = permissions_profile.as_deref() {
        check_device_profile(path, name)?;
    }
    let request = AddWikiRequest {
        id: parse_id(id)?,
        path: path.clone(),
        profile: Some(managed_profile(*profile)),
        groups: group.clone(),
        git_dir: git_dir.clone(),
        permissions_profile: permissions_profile.clone(),
        sync_backend: if *no_sync {
            Some("none".to_string())
        } else {
            Some(sync_backend.clone().unwrap_or_else(|| "git".to_string()))
        },
        platform_profile: None,
    };
    let wiki = registry
        .add(&request, *dry_run)
        .map_err(CliError::operation)?;
    // A registered Git vault is enrolled with the device key when policy
    // says so; this never fails the add.
    let mut setup = SetupSteps::default();
    if !(*dry_run || *no_device_key || wiki.sync_backend.as_deref() != Some("git")) {
        match crate::commands::enroll::enroll_registered(
            cli,
            &wiki,
            &vulcan_app::vault_enroll::EnrollRequest {
                wiki: wiki.id.to_string(),
                remote: vulcan_app::sync::GitRemote::parse("origin")
                    .map_err(CliError::operation)?,
                no_device_key: false,
                login: crate::commands::enroll::login_policy(*login),
                dry_run: false,
            },
        ) {
            Ok(report) => setup.enroll = Some(report),
            Err(error) => setup.enroll_error = Some(error.to_string()),
        }
    }
    setup.ignore_device_files(&wiki, *keep_device_files, *dry_run);
    print_mutation(cli.output, "add", *dry_run, registry, &wiki, &setup)
}

fn handle_clone(
    cli: &Cli,
    registry: &WikiRegistry,
    command: &VaultCommand,
) -> Result<(), CliError> {
    let VaultCommand::Clone {
        remote,
        path,
        id,
        profile,
        group,
        git_dir,
        platform,
        permissions_profile,
        no_device_key,
        keep_device_files,
        login,
        dry_run,
    } = command
    else {
        unreachable!("clone handler requires a clone command")
    };
    let id = id.as_deref().map_or_else(
        || {
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    CliError::operation("cannot derive a wiki ID from the destination; pass --id")
                })
                .and_then(parse_id)
        },
        parse_id,
    )?;
    clone_wiki(
        cli,
        registry,
        CloneCliRequest {
            id,
            profile: managed_profile(*profile),
            remote,
            path,
            groups: group,
            git_dir: git_dir.as_deref(),
            platform: match platform {
                ClonePlatformArg::Native => GitPlatformProfile::native(),
                ClonePlatformArg::AndroidShared => GitPlatformProfile::AndroidShared,
            },
            permissions_profile: permissions_profile.as_deref(),
            no_device_key: *no_device_key,
            keep_device_files: *keep_device_files,
            login: *login,
            dry_run: *dry_run,
        },
    )
}

#[allow(clippy::struct_excessive_bools)] // Mirrors the clone command's flags.
pub(crate) struct CloneCliRequest<'a> {
    pub(crate) id: WikiId,
    pub(crate) profile: ManagedDirectoryProfile,
    pub(crate) remote: &'a str,
    pub(crate) path: &'a Path,
    pub(crate) groups: &'a [String],
    pub(crate) git_dir: Option<&'a Path>,
    pub(crate) platform: GitPlatformProfile,
    pub(crate) permissions_profile: Option<&'a str>,
    /// Treat the transport policy as ambient for this clone.
    pub(crate) no_device_key: bool,
    /// Go on syncing Obsidian's per-device files.
    pub(crate) keep_device_files: bool,
    pub(crate) login: bool,
    pub(crate) dry_run: bool,
}

pub(crate) fn clone_wiki(
    cli: &Cli,
    registry: &WikiRegistry,
    request: CloneCliRequest<'_>,
) -> Result<(), CliError> {
    use crate::commands::enroll::{enroll_registered, login_policy, prepare_clone};
    use vulcan_app::vault_enroll::{CloneTransportRequest, EnrollRequest};
    let wiki_id = request.id.to_string();
    let login = login_policy(request.login);

    // 1. Decide how to authenticate the clone. This never blocks the clone: any
    //    trouble means the user's own credentials are used, as before.
    let transport = if request.no_device_key {
        None
    } else {
        match prepare_clone(
            cli,
            &CloneTransportRequest {
                source: request.remote,
                wiki: &wiki_id,
                no_device_key: false,
                login,
                dry_run: request.dry_run,
            },
            request.path,
            request.permissions_profile,
        ) {
            Ok(report) => Some(report),
            Err(error) => {
                eprintln!(
                    "note: could not prepare device-key transport ({error}); cloning with your own credentials"
                );
                None
            }
        }
    };
    let ssh_command = transport
        .as_ref()
        .and_then(|report| report.ssh_command.clone());

    // 2. Clone and register, with the device key when it is accepted.
    let report = clone_registered_wiki_with_ssh_command(
        registry,
        &CloneWikiRequest {
            id: request.id,
            profile: request.profile,
            source: request.remote.to_string(),
            work_tree: request.path.to_path_buf(),
            git_dir: request.git_dir.map(Path::to_path_buf),
            platform: request.platform,
            groups: request.groups.to_vec(),
            permissions_profile: request.permissions_profile.map(str::to_string),
        },
        request.dry_run,
        ssh_command.as_deref(),
    )
    .map_err(CliError::operation)?;

    // 3. Finish enrollment (bind, register) or report what is pending. A failure
    //    here never undoes the clone.
    let mut setup = SetupSteps::default();
    if !request.dry_run && !request.no_device_key {
        if let Some(wiki) = &report.wiki {
            match enroll_registered(
                cli,
                wiki,
                &EnrollRequest {
                    wiki: wiki_id.clone(),
                    remote: vulcan_app::sync::GitRemote::parse("origin")
                        .map_err(CliError::operation)?,
                    no_device_key: false,
                    login,
                    dry_run: false,
                },
            ) {
                Ok(done) => setup.enroll = Some(done),
                Err(error) => setup.enroll_error = Some(error.to_string()),
            }
        }
    }
    // 4. Ignore Obsidian's per-device files. A dry run has no checkout to inspect.
    if let Some(wiki) = report.wiki.as_ref().filter(|_| !request.dry_run) {
        setup.ignore_device_files(wiki, request.keep_device_files, false);
    }
    print_clone(cli.output, &report, transport.as_ref(), &setup)
}

#[derive(Serialize)]
struct CloneOutput<'a> {
    #[serde(flatten)]
    clone: &'a CloneWikiReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<&'a vulcan_app::vault_enroll::CloneTransportReport>,
    #[serde(flatten)]
    setup: &'a SetupSteps,
}

/// Human output for an enrollment that ran after a clone or add: quiet when
/// there was nothing to do, otherwise the steps and what is left.
fn print_enrollment(
    enroll: Option<&vulcan_app::vault_enroll::EnrollReport>,
    enroll_error: Option<&str>,
    wiki: &str,
) {
    use vulcan_app::vault_enroll::EnrollState;
    if let Some(error) = enroll_error {
        println!("Device-key enrollment did not finish: {error}");
        println!("Re-run it with: vulcan vault enroll {wiki}");
    }
    if let Some(report) = enroll.filter(|report| report.state != EnrollState::Skipped) {
        crate::commands::enroll::print_report(report);
    }
}

fn print_clone(
    output: OutputFormat,
    report: &CloneWikiReport,
    transport: Option<&vulcan_app::vault_enroll::CloneTransportReport>,
    setup: &SetupSteps,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(&CloneOutput {
            clone: report,
            transport,
            setup,
        });
    }
    let verb = if report.dry_run {
        "Would clone and register"
    } else {
        "Cloned and registered"
    };
    let path = report
        .wiki
        .as_ref()
        .map_or(&report.proposed_registration.path, |wiki| &wiki.path);
    println!(
        "{verb} wiki `{}` at {}",
        report.proposed_registration.id,
        path.display()
    );
    if let Some(wiki) = &report.wiki {
        print_work_tree(wiki);
    } else {
        println!(
            "A {VAULT_POINTER_FILE_NAME} in the cloned repository will select its vault directory."
        );
    }
    if let Some(git_dir) = &report.proposed_registration.git_dir {
        println!("Git directory: {}", git_dir.display());
    }
    if let Some(transport) = transport {
        if transport.device_key {
            println!(
                "{} with the device key.",
                if report.dry_run {
                    "Would clone"
                } else {
                    "Cloned"
                }
            );
        }
        for next in &transport.next_steps {
            println!("  next: {next}");
        }
    }
    setup.print(&report.proposed_registration.id.to_string());
    Ok(())
}

fn print_list(
    output: OutputFormat,
    registry: &WikiRegistry,
    group: Option<&str>,
    wikis: &[WikiRegistrationStatus],
) -> Result<(), CliError> {
    let (daemon_running, health) = registration_health(wikis);
    if output == OutputFormat::Json {
        return print_json(&VaultListReport {
            registry_path: registry.path(),
            group,
            daemon_running,
            wikis: wikis
                .iter()
                .zip(health)
                .map(|(wiki, health)| VaultWithHealth { wiki, health })
                .collect(),
        });
    }
    if wikis.is_empty() {
        println!("No registered wikis.");
    } else {
        for (wiki, health) in wikis.iter().zip(&health) {
            let profile = match wiki.registration.profile {
                ManagedDirectoryProfile::Knowledge => "",
                ManagedDirectoryProfile::FilesOnly => "\tfiles-only",
            };
            println!(
                "{}\t{}{profile}\t{}",
                wiki.registration.id,
                wiki.registration.path.display(),
                health_label(health)
            );
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ScanDirReport<'a> {
    action: &'static str,
    dry_run: bool,
    registry_path: &'a Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a Path>,
    scan_dirs: Vec<std::path::PathBuf>,
}

fn handle_scan_dir_command(
    cli: &Cli,
    registry: &WikiRegistry,
    command: &crate::cli::VaultScanDirCommand,
) -> Result<(), CliError> {
    use crate::cli::VaultScanDirCommand;
    let (action, dry_run, path) = match command {
        VaultScanDirCommand::List => ("list", false, None),
        VaultScanDirCommand::Add { path, dry_run } => (
            "add",
            *dry_run,
            Some(
                registry
                    .add_scan_dir(path, *dry_run)
                    .map_err(CliError::operation)?,
            ),
        ),
        VaultScanDirCommand::Remove { path, dry_run } => (
            "remove",
            *dry_run,
            Some(
                registry
                    .remove_scan_dir(path, *dry_run)
                    .map_err(CliError::operation)?,
            ),
        ),
    };
    let mut scan_dirs = registry.load().map_err(CliError::operation)?.scan_dirs;
    // A dry run reports the list as it would be.
    if dry_run {
        match (action, &path) {
            ("add", Some(path)) if !scan_dirs.contains(path) => {
                scan_dirs.push(path.clone());
                scan_dirs.sort();
            }
            ("remove", Some(path)) => scan_dirs.retain(|dir| dir != path),
            _ => {}
        }
    }
    if cli.output == OutputFormat::Json {
        return print_json(&ScanDirReport {
            action,
            dry_run,
            registry_path: registry.path(),
            path: path.as_deref(),
            scan_dirs,
        });
    }
    let verb = match (action, dry_run) {
        ("add", false) => Some("Remembered"),
        ("add", true) => Some("Would remember"),
        ("remove", false) => Some("Forgot"),
        ("remove", true) => Some("Would forget"),
        _ => None,
    };
    if let (Some(verb), Some(path)) = (verb, &path) {
        println!("{verb} scan directory {}", path.display());
    }
    if scan_dirs.is_empty() {
        println!("No scan directories.");
    } else if action == "list" {
        for dir in &scan_dirs {
            println!("{}", dir.display());
        }
    }
    Ok(())
}

fn print_discovery(
    output: OutputFormat,
    registry: &WikiRegistry,
    remembered: bool,
    report: &vulcan_daemon::vault_scan::VaultDiscoveryReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.scan_dirs.is_empty() {
        println!(
            "No scan directories in {}. Pass directories, or remember one with `vulcan vault scan-dir add <dir>`.",
            registry.path().display()
        );
        return Ok(());
    }
    if remembered {
        println!(
            "Scanning {}.",
            report
                .scan_dirs
                .iter()
                .map(|dir| dir.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for dir in &report.scan_dirs {
        if let Some(error) = &dir.error {
            println!("Could not read {}: {error}", dir.path.display());
        }
    }
    let mut new = 0;
    for vault in report.unregistered() {
        new += 1;
        match &vault.suggested_id {
            Some(id) => println!(
                "{}\n  vulcan vault add {id} {}",
                vault.path.display(),
                shell_quote(&vault.path.display().to_string())
            ),
            None => println!(
                "{}\n  vulcan vault add <id> {}  (choose an ID; the name gives none)",
                vault.path.display(),
                shell_quote(&vault.path.display().to_string())
            ),
        }
    }
    for vault in report.vaults.iter().filter(|vault| vault.error.is_some()) {
        println!(
            "{}: cannot be registered: {}",
            vault.path.display(),
            vault.error.as_deref().unwrap_or_default()
        );
    }
    let registered = report
        .vaults
        .iter()
        .filter(|vault| vault.registered_as.is_some())
        .count();
    if new == 0 {
        println!("No unregistered vaults found.");
    }
    if registered > 0 {
        println!("{registered} already registered (`vulcan vault list`).");
    }
    Ok(())
}

/// Quotes `value` for a POSIX shell when it holds anything but safe
/// characters.
fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn print_show(output: OutputFormat, wiki: &WikiRegistrationStatus) -> Result<(), CliError> {
    let (_, health) = registration_health(std::slice::from_ref(wiki));
    let health = health.into_iter().next().expect("one registration");
    if output == OutputFormat::Json {
        return print_json(&VaultWithHealth { wiki, health });
    }
    println!("Wiki: {}", wiki.registration.id);
    match &health.detail {
        Some(detail) => println!("Health: {} ({detail})", health_label(&health)),
        None => println!("Health: {}", health_label(&health)),
    }
    println!("Path: {}", wiki.registration.path.display());
    if let Some(work_tree) = &wiki.registration.work_tree {
        println!("Work tree: {}", work_tree.display());
    }
    println!("Available: {}", wiki.available);
    println!("Indexed: {}", wiki.indexed);
    println!("Git repository: {}", wiki.git_repository);
    if wiki.registration.profile == ManagedDirectoryProfile::FilesOnly {
        println!("Profile: files-only");
    }
    if !wiki.registration.groups.is_empty() {
        println!("Groups: {}", wiki.registration.groups.join(", "));
    }
    Ok(())
}

fn parse_id(id: &str) -> Result<WikiId, CliError> {
    WikiId::parse(id).map_err(CliError::operation)
}

pub(crate) fn managed_profile(profile: ManagedDirectoryProfileArg) -> ManagedDirectoryProfile {
    match profile {
        ManagedDirectoryProfileArg::Knowledge => ManagedDirectoryProfile::Knowledge,
        ManagedDirectoryProfileArg::FilesOnly => ManagedDirectoryProfile::FilesOnly,
    }
}

fn print_work_tree(wiki: &WikiRegistration) {
    if let Some(work_tree) = &wiki.work_tree {
        println!(
            "Vault is nested in the Git work tree {}; sync replicates the whole work tree.",
            work_tree.display()
        );
    }
}

fn print_mutation(
    output: OutputFormat,
    action: &'static str,
    dry_run: bool,
    registry: &WikiRegistry,
    wiki: &WikiRegistration,
    setup: &SetupSteps,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(&VaultMutationReport {
            action,
            dry_run,
            registry_path: registry.path(),
            wiki,
            capabilities: (wiki.profile == ManagedDirectoryProfile::FilesOnly)
                .then(|| wiki.capabilities()),
            setup,
        }),
        OutputFormat::Human | OutputFormat::Markdown => {
            let qualifier = if dry_run { "Would update" } else { "Updated" };
            println!("{qualifier} wiki `{}`: {}", wiki.id, wiki.path.display());
            print_work_tree(wiki);
            setup.print(&wiki.id.to_string());
            Ok(())
        }
    }
}
