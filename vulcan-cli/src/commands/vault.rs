use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat, VaultCommand};
use serde::Serialize;
use std::path::Path;
use vulcan_daemon::clone::{clone_registered_wiki, CloneWikiReport, CloneWikiRequest};
use vulcan_daemon::registry::{
    AddWikiRequest, UpdateWikiRequest, WikiId, WikiRegistration, WikiRegistrationStatus,
    WikiRegistry,
};

#[derive(Debug, Serialize)]
struct VaultMutationReport<'a> {
    action: &'static str,
    dry_run: bool,
    registry_path: &'a Path,
    wiki: &'a WikiRegistration,
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
        VaultCommand::Add {
            id,
            path,
            group,
            git_dir,
            permissions_profile,
            sync_backend,
            dry_run,
        } => {
            let request = AddWikiRequest {
                id: parse_id(id)?,
                path: path.clone(),
                groups: group.clone(),
                git_dir: git_dir.clone(),
                permissions_profile: permissions_profile.clone(),
                sync_backend: Some(sync_backend.clone()),
            };
            let wiki = registry
                .add(&request, *dry_run)
                .map_err(CliError::operation)?;
            print_mutation(cli.output, "add", *dry_run, &registry, &wiki)
        }
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
            group,
            remove_group,
            permissions_profile,
            clear_permissions_profile,
            dry_run,
        } => {
            let profile = if *clear_permissions_profile {
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
                        permissions_profile: profile,
                        sync_paused: None,
                    },
                    *dry_run,
                )
                .map_err(CliError::operation)?;
            print_mutation(cli.output, "set", *dry_run, &registry, &wiki)
        }
        VaultCommand::Remove { id, dry_run } => {
            let wiki = registry
                .remove(&parse_id(id)?, *dry_run)
                .map_err(CliError::operation)?;
            print_mutation(cli.output, "remove", *dry_run, &registry, &wiki)
        }
    }
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
        group,
        git_dir,
        permissions_profile,
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
    let report = clone_registered_wiki(
        registry,
        &CloneWikiRequest {
            id,
            source: remote.clone(),
            work_tree: path.clone(),
            git_dir: git_dir.clone(),
            groups: group.clone(),
            permissions_profile: permissions_profile.clone(),
        },
        *dry_run,
    )
    .map_err(CliError::operation)?;
    print_clone(cli.output, &report)
}

fn print_clone(output: OutputFormat, report: &CloneWikiReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    let verb = if report.dry_run {
        "Would clone and register"
    } else {
        "Cloned and registered"
    };
    println!(
        "{verb} wiki `{}` at {}",
        report.proposed_registration.id,
        report.proposed_registration.path.display()
    );
    if let Some(git_dir) = &report.proposed_registration.git_dir {
        println!("Git directory: {}", git_dir.display());
    }
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
            println!(
                "{}\t{}\t{state}",
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
    println!("Available: {}", wiki.available);
    println!("Indexed: {}", wiki.indexed);
    println!("Git repository: {}", wiki.git_repository);
    if !wiki.registration.groups.is_empty() {
        println!("Groups: {}", wiki.registration.groups.join(", "));
    }
    Ok(())
}

fn parse_id(id: &str) -> Result<WikiId, CliError> {
    WikiId::parse(id).map_err(CliError::operation)
}

fn print_mutation(
    output: OutputFormat,
    action: &'static str,
    dry_run: bool,
    registry: &WikiRegistry,
    wiki: &WikiRegistration,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(&VaultMutationReport {
            action,
            dry_run,
            registry_path: registry.path(),
            wiki,
        }),
        OutputFormat::Human | OutputFormat::Markdown => {
            let qualifier = if dry_run { "Would update" } else { "Updated" };
            println!("{qualifier} wiki `{}`: {}", wiki.id, wiki.path.display());
            Ok(())
        }
    }
}
