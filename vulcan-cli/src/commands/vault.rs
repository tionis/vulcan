use crate::cli::ManagedDirectoryProfileArg;
use crate::output::print_json;
use crate::{Cli, CliError, ClonePlatformArg, OutputFormat, VaultCommand};
use serde::Serialize;
use std::path::Path;
use vulcan_app::sync::GitPlatformProfile;
use vulcan_core::vault_discovery::VAULT_POINTER_FILE_NAME;
use vulcan_daemon::clone::{
    clone_registered_wiki_with_ssh_command, recover_registered_wiki_git, CloneWikiReport,
    CloneWikiRequest, RecoverWikiGitReport, RecoverWikiGitRequest,
};
use vulcan_daemon::registry::{
    AddWikiRequest, ManagedDirectoryProfile, UpdateWikiRequest, WikiId, WikiRegistration,
    WikiRegistrationStatus, WikiRegistry,
};

#[derive(Debug, Serialize)]
struct VaultMutationReport<'a> {
    action: &'static str,
    dry_run: bool,
    registry_path: &'a Path,
    wiki: &'a WikiRegistration,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<vulcan_daemon::registry::ManagedDirectoryCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enroll: Option<&'a vulcan_app::vault_enroll::EnrollReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enroll_error: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct VaultListReport<'a> {
    registry_path: &'a Path,
    group: Option<&'a str>,
    wikis: &'a [WikiRegistrationStatus],
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
            let wiki = registry
                .update(
                    &parse_id(id)?,
                    &UpdateWikiRequest {
                        groups_to_add: group.clone(),
                        groups_to_remove: remove_group.clone(),
                        permissions_profile: permissions,
                        sync_paused: None,
                        profile: profile.map(managed_profile),
                    },
                    *dry_run,
                )
                .map_err(CliError::operation)?;
            print_mutation(cli.output, "set", *dry_run, &registry, &wiki, None, None)
        }
        VaultCommand::Remove { id, dry_run } => {
            let wiki = registry
                .remove(&parse_id(id)?, *dry_run)
                .map_err(CliError::operation)?;
            print_mutation(cli.output, "remove", *dry_run, &registry, &wiki, None, None)
        }
    }
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
        login,
        dry_run,
    } = command
    else {
        unreachable!("add handler requires an add command")
    };
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
    let (enroll, enroll_error) =
        if *dry_run || *no_device_key || wiki.sync_backend.as_deref() != Some("git") {
            (None, None)
        } else {
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
                Ok(report) => (Some(report), None),
                Err(error) => (None, Some(error.to_string())),
            }
        };
    print_mutation(
        cli.output,
        "add",
        *dry_run,
        registry,
        &wiki,
        enroll.as_ref(),
        enroll_error.as_deref(),
    )
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
            login: *login,
            dry_run: *dry_run,
        },
    )
}

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
    let (mut enroll, mut enroll_error) = (None, None);
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
                Ok(done) => enroll = Some(done),
                Err(error) => enroll_error = Some(error.to_string()),
            }
        }
    }
    print_clone(
        cli.output,
        &report,
        transport.as_ref(),
        enroll.as_ref(),
        enroll_error.as_deref(),
    )
}

#[derive(Serialize)]
struct CloneOutput<'a> {
    #[serde(flatten)]
    clone: &'a CloneWikiReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<&'a vulcan_app::vault_enroll::CloneTransportReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enroll: Option<&'a vulcan_app::vault_enroll::EnrollReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enroll_error: Option<&'a str>,
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
    enroll: Option<&vulcan_app::vault_enroll::EnrollReport>,
    enroll_error: Option<&str>,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(&CloneOutput {
            clone: report,
            transport,
            enroll,
            enroll_error,
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
    print_enrollment(
        enroll,
        enroll_error,
        &report.proposed_registration.id.to_string(),
    );
    Ok(())
}

fn print_list(
    output: OutputFormat,
    registry: &WikiRegistry,
    group: Option<&str>,
    wikis: &[WikiRegistrationStatus],
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(&VaultListReport {
            registry_path: registry.path(),
            group,
            wikis,
        });
    }
    if wikis.is_empty() {
        println!("No registered wikis.");
    } else {
        for wiki in wikis {
            let state = if wiki.available {
                "available"
            } else {
                "missing"
            };
            let profile = match wiki.registration.profile {
                ManagedDirectoryProfile::Knowledge => "",
                ManagedDirectoryProfile::FilesOnly => "\tfiles-only",
            };
            println!(
                "{}\t{}{profile}\t{state}",
                wiki.registration.id,
                wiki.registration.path.display()
            );
        }
    }
    Ok(())
}

fn print_show(output: OutputFormat, wiki: &WikiRegistrationStatus) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(wiki);
    }
    println!("Wiki: {}", wiki.registration.id);
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
    enroll: Option<&vulcan_app::vault_enroll::EnrollReport>,
    enroll_error: Option<&str>,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(&VaultMutationReport {
            action,
            dry_run,
            registry_path: registry.path(),
            wiki,
            capabilities: (wiki.profile == ManagedDirectoryProfile::FilesOnly)
                .then(|| wiki.capabilities()),
            enroll,
            enroll_error,
        }),
        OutputFormat::Human | OutputFormat::Markdown => {
            let qualifier = if dry_run { "Would update" } else { "Updated" };
            println!("{qualifier} wiki `{}`: {}", wiki.id, wiki.path.display());
            print_work_tree(wiki);
            print_enrollment(enroll, enroll_error, &wiki.id.to_string());
            Ok(())
        }
    }
}
