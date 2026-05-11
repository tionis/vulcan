use crate::output::print_json;
use crate::{AnsiPalette, CliError, OutputFormat};
use vulcan_app::browse::{build_vault_status_report, VaultStatusReport};
use vulcan_core::VaultPaths;

pub(crate) fn run_status_command(paths: &VaultPaths) -> Result<VaultStatusReport, CliError> {
    build_vault_status_report(paths).map_err(CliError::operation)
}

pub(crate) fn print_status_report(
    output: OutputFormat,
    report: &VaultStatusReport,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let palette = AnsiPalette::new(use_color);
            println!("Vault:      {}", report.vault_root);
            println!(
                "Notes:      {}  attachments: {}",
                report.note_count, report.attachment_count
            );
            println!("Cache:      {} bytes", report.cache_bytes);
            if let Some(last_scan) = &report.last_scan {
                println!("Last scan:  {last_scan}");
            } else {
                println!("Last scan:  {}", palette.dim("never"));
            }
            if let Some(confidence) = &report.graph_confidence {
                println!(
                    "Confidence: {} EXTRACTED, {} INFERRED, {} AMBIGUOUS",
                    confidence.extracted, confidence.inferred, confidence.ambiguous
                );
            }
            if let Some(branch) = &report.git_branch {
                let dirty_flag = if report.git_dirty {
                    format!(
                        " {}",
                        palette.yellow(&format!(
                            "(dirty: {} staged, {} unstaged, {} untracked)",
                            report.git_staged, report.git_unstaged, report.git_untracked
                        ))
                    )
                } else {
                    format!(" {}", palette.green("(clean)"))
                };
                println!("Git:        {branch}{dirty_flag}");
            } else {
                println!("Git:        {}", palette.dim("not a git repository"));
            }
            Ok(())
        }
    }
}
