use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::{
    selected_permission_guard, warn_auto_commit_if_needed, Cli, CliError, OutputFormat,
    WikiPackageCommand, WikiSourceLocatorsArg,
};
use vulcan_app::wiki_package::{
    export_wiki_package, import_wiki_package, WikiPackageExportRequest, WikiPackageImportRequest,
    WikiSourceLocators,
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
            source_locators,
            dry_run,
            no_commit,
        } => {
            selected_permission_guard(cli, paths)?
                .check_refactor_path(destination)
                .map_err(CliError::operation)?;
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.verbosity());
            let report = import_wiki_package(
                paths,
                &WikiPackageImportRequest {
                    package: package.clone(),
                    destination: destination.clone(),
                    source_locators: match source_locators {
                        WikiSourceLocatorsArg::Summary => WikiSourceLocators::Summary,
                        WikiSourceLocatorsArg::Full => WikiSourceLocators::Full,
                    },
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
                        cli.verbosity(),
                    )
                    .map_err(CliError::operation)?;
            }
            match cli.output {
                OutputFormat::Json => print_json(&report),
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!(
                        "{} wiki {} into {} ({} notes, {} assets, {} with source locators)",
                        if report.dry_run {
                            "Would import"
                        } else {
                            "Imported"
                        },
                        report.package_identity,
                        report.destination_root,
                        report.notes,
                        report.assets,
                        report.annotated_notes.len()
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
            let summary = &package.summary;
            println!("Wiki package: {}", package.package_path.display());
            if let Some(version) = package.version {
                println!("Format version: {version}");
            }
            println!("Identity: {}", package.identity);
            println!("Valid: {}", package.valid);
            if package.manifest.is_some() {
                println!("Notes: {}, assets: {}", summary.notes, summary.assets);
            }
            if package.version.is_some_and(|version| version >= 2) {
                println!(
                    "Sources: {}, provenance activities: {}",
                    summary.sources, summary.provenance_activities
                );
                println!(
                    "Source map: {} mappings, {} references",
                    summary.source_mappings, summary.source_references
                );
                println!(
                    "Knowledge: {} entities ({} accepted), {} claims ({} accepted)",
                    summary.entities,
                    summary.accepted_entities,
                    summary.claims,
                    summary.accepted_claims
                );
            }
            for diagnostic in &package.diagnostics {
                let path = diagnostic
                    .path
                    .as_deref()
                    .map_or_else(String::new, |path| format!(" [{path}]"));
                println!("{}{path}: {}", diagnostic.code, diagnostic.message);
            }
            Ok(())
        }
    }
}
