use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::{
    selected_permission_guard, warn_auto_commit_if_needed, Cli, CliError, OutputFormat,
    WikiPackageCommand,
};
use vulcan_app::wiki_package::{
    export_wiki_package, import_wiki_package, WikiPackageExportRequest, WikiPackageImportRequest,
};
use vulcan_core::wiki_package::{inspect_wiki_package, WikiPackage};
use vulcan_core::{PermissionGuard, VaultPaths};

#[allow(clippy::too_many_lines)]
pub(crate) fn handle_wiki_package_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &WikiPackageCommand,
) -> Result<(), CliError> {
    match command {
        WikiPackageCommand::Inspect { package } => print_inspection(
            cli.output,
            &inspect_wiki_package(package).map_err(CliError::operation)?,
        ),
        WikiPackageCommand::Validate { package } => {
            let package = inspect_wiki_package(package).map_err(CliError::operation)?;
            print_inspection(cli.output, &package)?;
            if package.valid {
                Ok(())
            } else {
                Err(CliError::issues("wiki package validation errors detected"))
            }
        }
        WikiPackageCommand::Import {
            package,
            destination,
            dry_run,
            no_commit,
        } => {
            selected_permission_guard(cli, paths)?
                .check_refactor_path(destination)
                .map_err(CliError::operation)?;
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = import_wiki_package(
                paths,
                &WikiPackageImportRequest {
                    package: package.clone(),
                    destination: destination.clone(),
                    dry_run: *dry_run,
                },
            )
            .map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(
                        paths,
                        "wiki-package-import",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            match cli.output {
                OutputFormat::Json => print_json(&report),
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!(
                        "{} wiki {} into {} ({} notes, {} assets)",
                        if report.dry_run {
                            "Would import"
                        } else {
                            "Imported"
                        },
                        report.package_identity,
                        report.destination_root,
                        report.notes,
                        report.assets
                    );
                    Ok(())
                }
            }
        }
        WikiPackageCommand::Export {
            package_output,
            title,
            dry_run,
        } => {
            selected_permission_guard(cli, paths)?
                .check_read_path("")
                .map_err(CliError::operation)?;
            let report = export_wiki_package(
                paths,
                &WikiPackageExportRequest {
                    output: package_output.clone(),
                    title: title.clone(),
                    dry_run: *dry_run,
                },
            )
            .map_err(CliError::operation)?;
            match cli.output {
                OutputFormat::Json => print_json(&report),
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!(
                        "{} wiki {} to {} ({} notes, {} assets)",
                        if report.dry_run {
                            "Would export"
                        } else {
                            "Exported"
                        },
                        report.identity,
                        report.output_path,
                        report.notes,
                        report.assets
                    );
                    Ok(())
                }
            }
        }
    }
}

fn print_inspection(output: OutputFormat, package: &WikiPackage) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(package),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Wiki package: {}", package.package_path.display());
            println!("Identity: {}", package.identity);
            println!("Valid: {}", package.valid);
            if let Some(manifest) = &package.manifest {
                println!("Members: {}", manifest.members.len());
            }
            for diagnostic in &package.diagnostics {
                println!("{}: {}", diagnostic.code, diagnostic.message);
            }
            Ok(())
        }
    }
}
