mod cli;

pub use cli::{Cli, Command, OutputFormat};

use clap::Parser;
use serde::Serialize;
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;
use vulcan_core::{
    doctor_vault, initialize_vault, scan_vault, DoctorDiagnosticIssue, DoctorLinkIssue,
    DoctorReport, InitSummary, ScanMode, ScanSummary, VaultPaths,
};

#[derive(Debug)]
pub struct CliError {
    exit_code: u8,
    message: String,
}

impl CliError {
    fn not_implemented(command: &str) -> Self {
        Self {
            exit_code: 2,
            message: format!("{command} is not implemented yet"),
        }
    }

    fn io(error: &io::Error) -> Self {
        Self {
            exit_code: 1,
            message: format!("failed to read current working directory: {error}"),
        }
    }

    fn operation(error: impl Display) -> Self {
        Self {
            exit_code: 1,
            message: error.to_string(),
        }
    }

    #[must_use]
    pub fn exit_code(&self) -> u8 {
        self.exit_code
    }
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

pub fn run() -> Result<(), CliError> {
    run_from(std::env::args_os())
}

pub fn run_from<I, T>(args: I) -> Result<(), CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    dispatch(&cli)
}

fn dispatch(cli: &Cli) -> Result<(), CliError> {
    let paths = VaultPaths::new(resolve_vault_root(&cli.vault)?);

    match cli.command {
        Command::Describe => Err(CliError::not_implemented("describe")),
        Command::Doctor => {
            let report = doctor_vault(&paths).map_err(CliError::operation)?;
            print_doctor_report(cli.output, &paths, &report)?;
            Ok(())
        }
        Command::Init => {
            let summary = initialize_vault(&paths).map_err(CliError::operation)?;
            print_init_summary(cli.output, &summary)?;
            Ok(())
        }
        Command::Scan { full } => {
            let summary = scan_vault(
                &paths,
                if full {
                    ScanMode::Full
                } else {
                    ScanMode::Incremental
                },
            )
            .map_err(CliError::operation)?;
            print_scan_summary(cli.output, &summary);
            Ok(())
        }
    }
}

fn print_init_summary(output: OutputFormat, summary: &InitSummary) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!(
                "Initialized {} (config {}, cache {})",
                summary.vault_root.display(),
                if summary.created_config {
                    "created"
                } else {
                    "existing"
                },
                if summary.created_cache {
                    "created"
                } else {
                    "existing"
                },
            );
            Ok(())
        }
        OutputFormat::Json => print_json(summary),
    }
}

fn print_scan_summary(output: OutputFormat, summary: &ScanSummary) {
    match output {
        OutputFormat::Human => {
            println!(
                "Scanned {} files: {} added, {} updated, {} unchanged, {} deleted",
                summary.discovered,
                summary.added,
                summary.updated,
                summary.unchanged,
                summary.deleted
            );
        }
        OutputFormat::Json => {
            print_json(summary).expect("scan summary JSON serialization should succeed");
        }
    }
}

fn print_doctor_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &DoctorReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Doctor summary for {}", paths.vault_root().display());
            println!("- unresolved links: {}", report.summary.unresolved_links);
            println!(
                "- ambiguous link targets: {}",
                report.summary.ambiguous_links
            );
            println!("- parse failures: {}", report.summary.parse_failures);
            println!("- stale index rows: {}", report.summary.stale_index_rows);
            println!(
                "- missing index rows: {}",
                report.summary.missing_index_rows
            );
            println!("- orphan notes: {}", report.summary.orphan_notes);
            println!("- HTML links: {}", report.summary.html_links);

            if report.summary == zero_summary() {
                println!("No issues found.");
                return Ok(());
            }

            print_link_section("Unresolved links", &report.unresolved_links);
            print_link_section("Ambiguous link targets", &report.ambiguous_links);
            print_diagnostic_section("Parse failures", &report.parse_failures);
            print_path_section("Stale index rows", &report.stale_index_rows);
            print_path_section("Missing index rows", &report.missing_index_rows);
            print_path_section("Orphan notes", &report.orphan_notes);
            print_diagnostic_section("HTML links", &report.html_links);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    println!(
        "{}",
        serde_json::to_string(value).map_err(CliError::operation)?
    );
    Ok(())
}

fn resolve_vault_root(vault: &PathBuf) -> Result<PathBuf, CliError> {
    if vault.is_absolute() {
        return Ok(vault.clone());
    }

    Ok(std::env::current_dir()
        .map_err(|error| CliError::io(&error))?
        .join(vault))
}

fn print_link_section(title: &str, issues: &[DoctorLinkIssue]) {
    if issues.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for issue in issues {
        let path = issue.document_path.as_deref().unwrap_or("<unknown>");
        if issue.matches.is_empty() {
            if let Some(target) = issue.target.as_deref() {
                println!("- {path}: {target} ({})", issue.message);
            } else {
                println!("- {path}: {}", issue.message);
            }
        } else if let Some(target) = issue.target.as_deref() {
            println!(
                "- {path}: {target} ({}) [{}]",
                issue.message,
                issue.matches.join(", ")
            );
        } else {
            println!("- {path}: {} [{}]", issue.message, issue.matches.join(", "));
        }
    }
}

fn print_diagnostic_section(title: &str, issues: &[DoctorDiagnosticIssue]) {
    if issues.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for issue in issues {
        let path = issue.document_path.as_deref().unwrap_or("<unknown>");
        if let Some(byte_range) = issue.byte_range.as_ref() {
            println!(
                "- {path}: {} (bytes {}-{})",
                issue.message, byte_range.start, byte_range.end
            );
        } else {
            println!("- {path}: {}", issue.message);
        }
    }
}

fn print_path_section(title: &str, paths: &[String]) {
    if paths.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for path in paths {
        println!("- {path}");
    }
}

fn zero_summary() -> vulcan_core::DoctorSummary {
    vulcan_core::DoctorSummary {
        unresolved_links: 0,
        ambiguous_links: 0,
        parse_failures: 0,
        stale_index_rows: 0,
        missing_index_rows: 0,
        orphan_notes: 0,
        html_links: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_defaults_for_doctor_command() {
        let cli = Cli::try_parse_from(["vulcan", "doctor"]).expect("cli should parse");

        assert_eq!(cli.vault, PathBuf::from("."));
        assert_eq!(cli.output, OutputFormat::Human);
        assert!(!cli.verbose);
        assert_eq!(cli.command, Command::Doctor);
    }

    #[test]
    fn parses_global_flags_and_scan_options() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "--vault",
            "/tmp/vault",
            "--output",
            "json",
            "--verbose",
            "scan",
            "--full",
        ])
        .expect("cli should parse");

        assert_eq!(cli.vault, PathBuf::from("/tmp/vault"));
        assert_eq!(cli.output, OutputFormat::Json);
        assert!(cli.verbose);
        assert_eq!(cli.command, Command::Scan { full: true });
    }

    #[test]
    fn resolves_relative_vault_path_against_current_directory() {
        let current_dir = std::env::current_dir().expect("cwd should be available");
        let resolved = resolve_vault_root(&PathBuf::from("tests/fixtures/vaults/basic"))
            .expect("path resolution should succeed");

        assert_eq!(resolved, current_dir.join("tests/fixtures/vaults/basic"));
    }
}
