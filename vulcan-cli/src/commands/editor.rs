use crate::cli::{EditorCommand, NvimCommand};
use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat};
use std::path::{Path, PathBuf};
use vulcan_app::nvim_plugin::{
    default_install_dir, install_plugin, plugin_status, uninstall_plugin, NvimPluginReport,
    NvimPluginState, NvimPluginStatus,
};

pub(crate) fn handle_editor_command(cli: &Cli, command: &EditorCommand) -> Result<(), CliError> {
    let EditorCommand::Nvim { command } = command;
    match command {
        NvimCommand::Install {
            dir,
            force,
            dry_run,
        } => {
            let dir = install_dir(dir.as_deref())?;
            let report = install_plugin(&dir, *force, *dry_run)?;
            print_report(cli.output, &report, "Installed", "Would install")
        }
        NvimCommand::Uninstall {
            dir,
            force,
            dry_run,
        } => {
            let dir = install_dir(dir.as_deref())?;
            let report = uninstall_plugin(&dir, *force, *dry_run)?;
            print_report(cli.output, &report, "Removed", "Would remove")
        }
        NvimCommand::Status { dir } => {
            let status = plugin_status(&install_dir(dir.as_deref())?)?;
            print_status(cli.output, &status)
        }
        NvimCommand::Path => {
            let dir = install_dir(None)?;
            match cli.output {
                OutputFormat::Json => print_json(&serde_json::json!({ "path": dir })),
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!("{}", dir.display());
                    Ok(())
                }
            }
        }
    }
}

fn install_dir(dir: Option<&Path>) -> Result<PathBuf, CliError> {
    match dir {
        Some(dir) => Ok(dir.to_path_buf()),
        None => default_install_dir()
            .ok_or_else(|| CliError::operation("cannot find Neovim's data directory; pass --dir")),
    }
}

fn state_label(state: NvimPluginState) -> &'static str {
    match state {
        NvimPluginState::Missing => "not installed",
        NvimPluginState::Current => "up to date",
        NvimPluginState::Outdated => "outdated",
        NvimPluginState::Modified => "installed by Vulcan, with local changes",
        NvimPluginState::Unmanaged => "not installed by Vulcan",
    }
}

fn print_status(output: OutputFormat, status: &NvimPluginStatus) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(status);
    }
    println!(
        "vulcan.nvim at {}: {}",
        status.path.display(),
        state_label(status.state)
    );
    match status.state {
        NvimPluginState::Outdated => println!(
            "Installed from {}, this binary bundles {}. Run `vulcan editor nvim install` to refresh.",
            status.installed_version.as_deref().unwrap_or("?"),
            status.version
        ),
        NvimPluginState::Modified => println!(
            "Changed files: {}. `vulcan editor nvim install --force` replaces them.",
            status.modified_files.join(", ")
        ),
        NvimPluginState::Missing => println!("Run `vulcan editor nvim install` to install it."),
        NvimPluginState::Current | NvimPluginState::Unmanaged => {}
    }
    Ok(())
}

fn print_report(
    output: OutputFormat,
    report: &NvimPluginReport,
    done: &str,
    preview: &str,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    let verb = if report.dry_run { preview } else { done };
    let changed = report.written.len() + report.removed.len();
    if changed == 0 {
        println!("vulcan.nvim at {} is up to date.", report.path.display());
        return Ok(());
    }
    println!("{verb} vulcan.nvim at {}:", report.path.display());
    for path in &report.written {
        println!("  write  {path}");
    }
    for path in &report.removed {
        println!("  remove {path}");
    }
    if !report.dry_run && done == "Installed" && report.previous == NvimPluginState::Missing {
        println!(
            "Neovim loads it from there. Call require('vulcan').setup() in your config, or use it from a plugin manager: {{ dir = '{}', opts = {{}} }}",
            report.path.display()
        );
    }
    Ok(())
}
