use crate::cli::{EditorCommand, EditorPluginCommand};
use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat};
use std::path::{Path, PathBuf};
use vulcan_app::editor_plugins::{
    EditorPlugin, PluginReport, PluginState, PluginStatus, NVIM, VIM,
};

pub(crate) fn handle_editor_command(cli: &Cli, command: &EditorCommand) -> Result<(), CliError> {
    let (plugin, command) = match command {
        EditorCommand::Nvim { command } => (NVIM, command),
        EditorCommand::Vim { command } => (VIM, command),
    };
    match command {
        EditorPluginCommand::Install {
            dir,
            force,
            dry_run,
        } => {
            let dir = install_dir(plugin, dir.as_deref())?;
            let report = plugin.install(&dir, *force, *dry_run)?;
            print_report(cli.output, plugin, &report, "Installed", "Would install")
        }
        EditorPluginCommand::Uninstall {
            dir,
            force,
            dry_run,
        } => {
            let dir = install_dir(plugin, dir.as_deref())?;
            let report = plugin.uninstall(&dir, *force, *dry_run)?;
            print_report(cli.output, plugin, &report, "Removed", "Would remove")
        }
        EditorPluginCommand::Status { dir } => {
            let status = plugin.status(&install_dir(plugin, dir.as_deref())?)?;
            print_status(cli.output, plugin, &status)
        }
        EditorPluginCommand::Path => {
            let dir = install_dir(plugin, None)?;
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

fn install_dir(plugin: EditorPlugin, dir: Option<&Path>) -> Result<PathBuf, CliError> {
    match dir {
        Some(dir) => Ok(dir.to_path_buf()),
        None => plugin.default_install_dir().ok_or_else(|| {
            CliError::operation(format!(
                "cannot find the editor's package directory for {}; pass --dir",
                plugin.name
            ))
        }),
    }
}

/// The editor's subcommand word (`nvim` or `vim`), for messages.
fn editor_word(plugin: EditorPlugin) -> &'static str {
    plugin.name.trim_start_matches("vulcan.")
}

fn state_label(state: PluginState) -> &'static str {
    match state {
        PluginState::Missing => "not installed",
        PluginState::Current => "up to date",
        PluginState::Outdated => "outdated",
        PluginState::Modified => "installed by Vulcan, with local changes",
        PluginState::Unmanaged => "not installed by Vulcan",
    }
}

fn print_status(
    output: OutputFormat,
    plugin: EditorPlugin,
    status: &PluginStatus,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(status);
    }
    let editor = editor_word(plugin);
    println!(
        "{} at {}: {}",
        plugin.name,
        status.path.display(),
        state_label(status.state)
    );
    match status.state {
        PluginState::Outdated => println!(
            "Installed from {}, this binary bundles {}. Run `vulcan editor {editor} install` to refresh.",
            status.installed_version.as_deref().unwrap_or("?"),
            status.version
        ),
        PluginState::Modified => println!(
            "Changed files: {}. `vulcan editor {editor} install --force` replaces them.",
            status.modified_files.join(", ")
        ),
        PluginState::Missing => println!("Run `vulcan editor {editor} install` to install it."),
        PluginState::Current | PluginState::Unmanaged => {}
    }
    Ok(())
}

fn print_report(
    output: OutputFormat,
    plugin: EditorPlugin,
    report: &PluginReport,
    done: &str,
    preview: &str,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    let verb = if report.dry_run { preview } else { done };
    let changed = report.written.len() + report.removed.len();
    if changed == 0 {
        println!(
            "{} at {} is up to date.",
            plugin.name,
            report.path.display()
        );
        return Ok(());
    }
    println!("{verb} {} at {}:", plugin.name, report.path.display());
    for path in &report.written {
        println!("  write  {path}");
    }
    for path in &report.removed {
        println!("  remove {path}");
    }
    if !report.dry_run && done == "Installed" && report.previous == PluginState::Missing {
        if plugin.name == VIM.name {
            println!(
                "Vim loads it from there at startup. It needs yegappan/lsp; check with :Vulcan health."
            );
        } else {
            println!(
                "Neovim loads it from there. Call require('vulcan').setup() in your config, or use it from a plugin manager: {{ dir = '{}', opts = {{}} }}",
                report.path.display()
            );
        }
    }
    Ok(())
}
