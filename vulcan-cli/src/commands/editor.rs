use crate::cli::{EditorCommand, EditorPluginCommand, HelixCommand, HelixConfigFileArg};
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
        EditorCommand::Helix { command } => return handle_helix_command(cli, command),
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

fn handle_helix_command(cli: &Cli, command: &HelixCommand) -> Result<(), CliError> {
    use vulcan_app::helix_config;
    let directory = |dir: &Option<PathBuf>| {
        dir.clone()
            .or_else(helix_config::default_config_dir)
            .ok_or_else(|| CliError::operation("cannot find Helix's config directory; pass --dir"))
    };
    let (report, verb) = match command {
        HelixCommand::PrintConfig { only } => return print_helix_config(cli, *only),
        HelixCommand::Status { dir } => (helix_config::status(&directory(dir)?)?, None),
        HelixCommand::Install {
            dir,
            force,
            dry_run,
        } => (
            helix_config::install(&directory(dir)?, *force, *dry_run)?,
            Some("install"),
        ),
        HelixCommand::Uninstall {
            dir,
            force,
            dry_run,
        } => (
            helix_config::uninstall(&directory(dir)?, *force, *dry_run)?,
            Some("uninstall"),
        ),
    };
    if cli.output == OutputFormat::Json {
        return print_json(&report);
    }
    print_helix_report(&report, verb);
    Ok(())
}

fn print_helix_config(cli: &Cli, only: Option<HelixConfigFileArg>) -> Result<(), CliError> {
    let config = vulcan_app::helix_config::helix_config();
    if cli.output == OutputFormat::Json {
        return print_json(&config);
    }
    let directory = config.config_dir.as_ref().map_or_else(
        || "Helix's config directory".to_string(),
        |dir| dir.display().to_string(),
    );
    match only {
        Some(HelixConfigFileArg::Languages) => print!("{}", config.languages_toml),
        Some(HelixConfigFileArg::Keys) => print!("{}", config.config_toml),
        None => {
            println!("# ---- {directory}/languages.toml ----\n");
            print!("{}", config.languages_toml);
            println!("\n# ---- {directory}/config.toml ----\n");
            print!("{}", config.config_toml);
        }
    }
    Ok(())
}

fn print_helix_report(report: &vulcan_app::helix_config::HelixReport, verb: Option<&str>) {
    use vulcan_app::helix_config::{EntryAction, EntryState};
    let state = |state: EntryState| match state {
        EntryState::Missing => "missing",
        EntryState::Current => "current",
        EntryState::Outdated => "outdated",
        EntryState::Modified => "changed since Vulcan wrote it",
        EntryState::Removed => "removed since Vulcan wrote it",
        EntryState::Foreign => "set by you",
    };
    println!("Helix configuration in {}:", report.config_dir.display());
    for entry in &report.entries {
        let action = match entry.action {
            EntryAction::None => String::new(),
            EntryAction::Write => " -> write".to_string(),
            EntryAction::Update => " -> update".to_string(),
            EntryAction::Remove => " -> remove".to_string(),
            EntryAction::Skip => " -> kept (--force to change)".to_string(),
        };
        println!(
            "  {} {}: {}{action}",
            entry.file,
            entry.key,
            state(entry.state)
        );
    }
    if report.dry_run && verb.is_some() {
        for diff in report.diffs.values() {
            print!("\n{diff}");
        }
        if report.diffs.is_empty() {
            println!("Nothing would change.");
        }
        return;
    }
    if verb.is_some() {
        if report.diffs.is_empty() {
            println!("Nothing changed.");
        } else {
            println!(
                "Changed {}.",
                report
                    .diffs
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" and ")
            );
        }
        for backup in &report.backups {
            println!("Kept the previous version as {}.", backup.display());
        }
        if verb == Some("install") && !report.diffs.is_empty() {
            println!("Restart Helix (or run :config-reload) to load it.");
        }
    } else if !report.installed {
        println!("Not installed; run `vulcan editor helix install`.");
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
