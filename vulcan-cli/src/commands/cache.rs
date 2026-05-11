use crate::output::print_json;
use crate::{CacheCommand, CliError, OutputFormat};
use vulcan_core::{
    cache_vacuum, inspect_cache, verify_cache, CacheInspectReport, CacheVacuumQuery,
    CacheVacuumReport, CacheVerifyReport, VaultPaths,
};

pub(crate) fn handle_cache_command(
    paths: &VaultPaths,
    output: OutputFormat,
    command: &CacheCommand,
) -> Result<(), CliError> {
    match command {
        CacheCommand::Inspect => {
            let report = inspect_cache(paths).map_err(CliError::operation)?;
            print_cache_inspect_report(output, &report)
        }
        CacheCommand::Verify { fail_on_errors } => {
            let report = verify_cache(paths).map_err(CliError::operation)?;
            print_cache_verify_report(output, &report)?;
            if *fail_on_errors && !report.healthy {
                Err(CliError::issues("cache verification failed"))
            } else {
                Ok(())
            }
        }
        CacheCommand::Vacuum { dry_run } => {
            let report = cache_vacuum(paths, &CacheVacuumQuery { dry_run: *dry_run })
                .map_err(CliError::operation)?;
            print_cache_vacuum_report(output, &report)
        }
    }
}

fn print_cache_inspect_report(
    output: OutputFormat,
    report: &CacheInspectReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Cache: {}", report.cache_path);
            println!("Bytes: {}", report.database_bytes);
            println!("Documents: {}", report.documents);
            println!("Notes: {}", report.notes);
            println!("Attachments: {}", report.attachments);
            println!("Bases: {}", report.bases);
            println!("Links: {}", report.links);
            println!("Chunks: {}", report.chunks);
            println!("Diagnostics: {}", report.diagnostics);
            println!("Search rows: {}", report.search_rows);
            println!("Vector rows: {}", report.vector_rows);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_cache_verify_report(
    output: OutputFormat,
    report: &CacheVerifyReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Cache healthy: {}", report.healthy);
            for check in &report.checks {
                println!(
                    "- {} [{}] {}",
                    check.name,
                    if check.ok { "ok" } else { "fail" },
                    check.detail
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_cache_vacuum_report(
    output: OutputFormat,
    report: &CacheVacuumReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Dry run: cache is {} bytes", report.before_bytes);
            } else {
                println!(
                    "Vacuumed cache: {} -> {} bytes (reclaimed {})",
                    report.before_bytes,
                    report.after_bytes.unwrap_or(report.before_bytes),
                    report.reclaimed_bytes.unwrap_or(0)
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}
