use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::{
    selected_permission_guard, warn_auto_commit_if_needed, Cli, CliError, OutputFormat,
    TextBundleCommand,
};
use vulcan_app::textbundle::{
    export_text_bundle, import_text_bundle, TextBundleExportReport, TextBundleExportRequest,
    TextBundleImportReport, TextBundleImportRequest,
};
use vulcan_core::textbundle::{inspect_text_bundle, TextBundle};
use vulcan_core::{PermissionGuard, VaultPaths};

pub(crate) fn handle_textbundle_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &TextBundleCommand,
) -> Result<(), CliError> {
    match command {
        TextBundleCommand::Inspect { package } => {
            let package = inspect_text_bundle(package).map_err(CliError::operation)?;
            print_inspection(cli.output, &package)
        }
        TextBundleCommand::Validate { package } => {
            let package = inspect_text_bundle(package).map_err(CliError::operation)?;
            print_inspection(cli.output, &package)?;
            if package.valid {
                Ok(())
            } else {
                Err(CliError::issues("TextBundle validation errors detected"))
            }
        }
        TextBundleCommand::Import {
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
            let report = import_text_bundle(
                paths,
                &TextBundleImportRequest {
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
                        "textbundle-import",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_import(cli.output, &report)
        }
        TextBundleCommand::Export {
            note,
            package_output,
            dry_run,
        } => {
            selected_permission_guard(cli, paths)?
                .check_read_path(note)
                .map_err(CliError::operation)?;
            let report = export_text_bundle(
                paths,
                &TextBundleExportRequest {
                    note: note.clone(),
                    output: package_output.clone(),
                    dry_run: *dry_run,
                },
            )
            .map_err(CliError::operation)?;
            print_export(cli.output, &report)
        }
    }
}

fn print_inspection(output: OutputFormat, package: &TextBundle) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(package),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("TextBundle: {}", package.package_path.display());
            println!("Identity: {}", package.identity);
            println!("Valid: {}", package.valid);
            println!("Text: {}", package.text_path.as_deref().unwrap_or("-"));
            println!("Assets: {}", package.assets.len());
            for diagnostic in &package.diagnostics {
                println!("{}: {}", diagnostic.code, diagnostic.message);
            }
            Ok(())
        }
    }
}

fn print_import(output: OutputFormat, report: &TextBundleImportReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{} TextBundle {} into {} ({} assets)",
                if report.dry_run {
                    "Would import"
                } else {
                    "Imported"
                },
                report.package_identity,
                report.destination_root,
                report.assets.len()
            );
            Ok(())
        }
    }
}

fn print_export(output: OutputFormat, report: &TextBundleExportReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{} {} as {} ({} assets)",
                if report.dry_run {
                    "Would export"
                } else {
                    "Exported"
                },
                report.note_path,
                report.output_path,
                report.assets.len()
            );
            Ok(())
        }
    }
}
