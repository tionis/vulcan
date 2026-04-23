use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::{
    selected_permission_guard, warn_auto_commit_if_needed, AgentCommand, AgentImportArgs,
    AgentRuntimeArg, Cli, CliError, OutputFormat,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use vulcan_core::{
    assistant_config_summary, assistant_prompts_root, assistant_skills_root, assistant_tools_root,
    list_assistant_prompts, list_assistant_skills, read_vault_agents_file, PermissionGuard,
    VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentPrintConfigProfiles {
    write: String,
    readonly: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentPrintConfigFiles {
    agents_path: String,
    agents_present: bool,
    prompts_path: String,
    visible_prompt_count: usize,
    skills_path: String,
    visible_skill_count: usize,
    tools_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentPrintConfigCommands {
    describe_openai_tools: String,
    help_json: String,
    skill_list: String,
    skill_get: String,
    note_get: String,
    note_patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentPrintConfigSnippets {
    write_enabled: String,
    readonly: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentPrintConfigReport {
    runtime: String,
    vault_root: String,
    profiles: AgentPrintConfigProfiles,
    files: AgentPrintConfigFiles,
    commands: AgentPrintConfigCommands,
    snippets: AgentPrintConfigSnippets,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AgentImportMode {
    Copy,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AgentImportStatus {
    WouldCreate,
    WouldUpdate,
    Created,
    Updated,
    Kept,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentImportItemReport {
    source_layout: String,
    kind: String,
    source_path: String,
    destination_path: String,
    mode: AgentImportMode,
    status: AgentImportStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentImportReport {
    apply: bool,
    overwrite: bool,
    symlink: bool,
    detected_count: usize,
    imported_count: usize,
    kept_count: usize,
    conflict_count: usize,
    items: Vec<AgentImportItemReport>,
}

#[derive(Debug, Clone)]
struct DetectedAgentAsset {
    source_layout: &'static str,
    kind: &'static str,
    source_path: PathBuf,
    source_display: String,
    destination_path: PathBuf,
    destination_display: String,
}

pub(crate) fn handle_agent_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &AgentCommand,
) -> Result<(), CliError> {
    match command {
        AgentCommand::Install(args) => {
            let guard = selected_permission_guard(cli, paths)?;
            for path in crate::bundled_support_relative_paths(paths) {
                guard.check_write_path(&path).map_err(CliError::operation)?;
            }
            let report = crate::run_agent_install_command(paths, args)?;
            crate::print_agent_install_summary(cli.output, paths, &report)
        }
        AgentCommand::PrintConfig(args) => {
            let report = build_agent_print_config_report(cli, paths, args.runtime)?;
            print_agent_print_config_report(cli.output, &report)
        }
        AgentCommand::Import(args) => {
            let report = run_agent_import_command(cli, paths, args)?;
            print_agent_import_report(cli.output, &report)
        }
    }
}

fn build_agent_print_config_report(
    cli: &Cli,
    paths: &VaultPaths,
    runtime: AgentRuntimeArg,
) -> Result<AgentPrintConfigReport, CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    let config = assistant_config_summary(paths);
    let prompt_count = list_assistant_prompts(paths)
        .map_err(CliError::operation)?
        .into_iter()
        .filter(|prompt| {
            guard
                .check_read_path(&format!("{}/{}", config.prompts_folder, prompt.path))
                .is_ok()
        })
        .count();
    let skill_count = list_assistant_skills(paths)
        .map_err(CliError::operation)?
        .into_iter()
        .filter(|skill| {
            guard
                .check_read_path(&format!("{}/{}", config.skills_folder, skill.path))
                .is_ok()
        })
        .count();
    let agents_present = read_vault_agents_file(paths)
        .map_err(CliError::operation)?
        .is_some();
    let runtime_name = agent_runtime_name(runtime).to_string();
    let commands = agent_print_config_commands(paths);
    let snippets = AgentPrintConfigSnippets {
        write_enabled: runtime_snippet(runtime, &commands, "agent"),
        readonly: runtime_snippet(
            runtime,
            &agent_print_config_commands_for_profile(paths, "readonly"),
            "readonly",
        ),
    };

    Ok(AgentPrintConfigReport {
        runtime: runtime_name,
        vault_root: paths.vault_root().display().to_string(),
        profiles: AgentPrintConfigProfiles {
            write: "agent".to_string(),
            readonly: "readonly".to_string(),
        },
        files: AgentPrintConfigFiles {
            agents_path: path_to_forward_slashes(&paths.vault_root().join("AGENTS.md")),
            agents_present,
            prompts_path: path_to_forward_slashes(&assistant_prompts_root(paths)),
            visible_prompt_count: prompt_count,
            skills_path: path_to_forward_slashes(&assistant_skills_root(paths)),
            visible_skill_count: skill_count,
            tools_path: path_to_forward_slashes(&assistant_tools_root(paths)),
        },
        commands,
        snippets,
        notes: vec![
            "Pass `--permissions <profile>` on every `vulcan` invocation from the external runtime.".to_string(),
            "Prefer `skill list` up front and `skill get <name>` on demand instead of preloading every skill body.".to_string(),
            "Use `describe --format openai-tools` for the pinned tool registry and `help --output json` for detailed command docs.".to_string(),
            "Treat Vulcan as the only write path for vault mutations; avoid direct filesystem edits for note changes.".to_string(),
        ],
    })
}

fn print_agent_print_config_report(
    output: OutputFormat,
    report: &AgentPrintConfigReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Runtime: {}", report.runtime);
            println!("Vault: {}", report.vault_root);
            println!(
                "Profiles: write={}, readonly={}",
                report.profiles.write, report.profiles.readonly
            );
            println!();
            println!("Files:");
            println!(
                "- AGENTS.md: {} ({})",
                report.files.agents_path,
                if report.files.agents_present {
                    "present"
                } else {
                    "missing"
                }
            );
            println!(
                "- Prompts: {} ({} visible)",
                report.files.prompts_path, report.files.visible_prompt_count
            );
            println!(
                "- Skills: {} ({} visible)",
                report.files.skills_path, report.files.visible_skill_count
            );
            println!("- Tools: {}", report.files.tools_path);
            println!();
            println!("Core commands:");
            println!("- describe: {}", report.commands.describe_openai_tools);
            println!("- help: {}", report.commands.help_json);
            println!("- skill list: {}", report.commands.skill_list);
            println!("- skill get: {}", report.commands.skill_get);
            println!();
            println!("Write-enabled snippet:");
            println!("{}", report.snippets.write_enabled);
            println!();
            println!("Readonly snippet:");
            println!("{}", report.snippets.readonly);
            Ok(())
        }
    }
}

fn run_agent_import_command(
    cli: &Cli,
    paths: &VaultPaths,
    args: &AgentImportArgs,
) -> Result<AgentImportReport, CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    let detected = detect_agent_assets(paths, &guard)?;
    let preview = build_agent_import_preview(paths, &detected, args.symlink, args.overwrite)?;

    if !args.apply {
        return Ok(preview);
    }

    if preview.conflict_count > 0 {
        return Err(CliError::operation(
            "conflicting external agent assets target the same Vulcan path; run `vulcan agent import` to inspect the preview and resolve them first",
        ));
    }

    for asset in &detected {
        guard
            .check_write_path(&asset.destination_display)
            .map_err(CliError::operation)?;
    }

    let mode = if args.symlink {
        AgentImportMode::Symlink
    } else {
        AgentImportMode::Copy
    };
    let mut items = Vec::new();
    let mut changed_paths = Vec::new();
    for asset in detected {
        let item = apply_agent_asset(&asset, mode, args.overwrite)?;
        if matches!(
            item.status,
            AgentImportStatus::Created | AgentImportStatus::Updated
        ) {
            changed_paths.push(item.destination_path.clone());
        }
        items.push(item);
    }

    let report = AgentImportReport {
        apply: true,
        overwrite: args.overwrite,
        symlink: args.symlink,
        detected_count: items.len(),
        imported_count: items
            .iter()
            .filter(|item| {
                matches!(
                    item.status,
                    AgentImportStatus::Created | AgentImportStatus::Updated
                )
            })
            .count(),
        kept_count: items
            .iter()
            .filter(|item| item.status == AgentImportStatus::Kept)
            .count(),
        conflict_count: 0,
        items,
    };

    if !changed_paths.is_empty() {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, args.no_commit);
        warn_auto_commit_if_needed(&auto_commit, cli.quiet);
        auto_commit
            .commit(
                paths,
                "agent-import",
                &changed_paths,
                cli.permissions.as_deref(),
                cli.quiet,
            )
            .map_err(CliError::operation)?;
    }

    Ok(report)
}

fn build_agent_import_preview(
    paths: &VaultPaths,
    detected: &[DetectedAgentAsset],
    symlink: bool,
    overwrite: bool,
) -> Result<AgentImportReport, CliError> {
    let mode = if symlink {
        AgentImportMode::Symlink
    } else {
        AgentImportMode::Copy
    };
    let conflicts = conflicting_destinations(detected);
    let mut items = Vec::new();
    for asset in detected {
        if conflicts.contains_key(&asset.destination_display) {
            items.push(AgentImportItemReport {
                source_layout: asset.source_layout.to_string(),
                kind: asset.kind.to_string(),
                source_path: asset.source_display.clone(),
                destination_path: asset.destination_display.clone(),
                mode,
                status: AgentImportStatus::Conflict,
                message: conflicts.get(&asset.destination_display).cloned(),
            });
            continue;
        }
        items.push(preview_agent_asset(paths, asset, mode, overwrite)?);
    }
    Ok(AgentImportReport {
        apply: false,
        overwrite,
        symlink,
        detected_count: items.len(),
        imported_count: 0,
        kept_count: items
            .iter()
            .filter(|item| item.status == AgentImportStatus::Kept)
            .count(),
        conflict_count: items
            .iter()
            .filter(|item| item.status == AgentImportStatus::Conflict)
            .count(),
        items,
    })
}

fn print_agent_import_report(
    output: OutputFormat,
    report: &AgentImportReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.items.is_empty() {
                println!("No importable external agent assets found.");
                return Ok(());
            }
            if report.apply {
                println!(
                    "Imported {} agent asset{}.",
                    report.imported_count,
                    if report.imported_count == 1 { "" } else { "s" }
                );
            } else {
                println!(
                    "Detected {} importable agent asset{}.",
                    report.detected_count,
                    if report.detected_count == 1 { "" } else { "s" }
                );
            }
            for item in &report.items {
                println!(
                    "- {} -> {} [{}; {}; {}]",
                    item.source_path,
                    item.destination_path,
                    item.source_layout,
                    item.kind,
                    agent_import_status_label(item.status)
                );
                if let Some(message) = item.message.as_deref() {
                    println!("  {message}");
                }
            }
            if !report.apply {
                println!("Run `vulcan agent import --apply` to copy the detected files.");
            }
            Ok(())
        }
    }
}

fn detect_agent_assets(
    paths: &VaultPaths,
    guard: &impl PermissionGuard,
) -> Result<Vec<DetectedAgentAsset>, CliError> {
    let mut detected = Vec::new();
    for (layout, relative_path) in [
        ("claude", "CLAUDE.md"),
        ("codex", "CODEX.md"),
        ("gemini", "GEMINI.md"),
    ] {
        let source_path = paths.vault_root().join(relative_path);
        if source_path.is_file() && guard.check_read_path(relative_path).is_ok() {
            detected.push(DetectedAgentAsset {
                source_layout: layout,
                kind: "instructions",
                source_display: relative_path.to_string(),
                source_path,
                destination_display: "AGENTS.md".to_string(),
                destination_path: paths.vault_root().join("AGENTS.md"),
            });
        }
    }

    for (layout, relative_root, kind, predicate) in [
        (
            "claude",
            ".claude/commands",
            "prompt",
            AgentAssetPredicate::Markdown,
        ),
        (
            "codex",
            ".codex/prompts",
            "prompt",
            AgentAssetPredicate::Markdown,
        ),
        (
            "gemini",
            ".gemini/prompts",
            "prompt",
            AgentAssetPredicate::Markdown,
        ),
        (
            "claude",
            ".claude/skills",
            "skill",
            AgentAssetPredicate::Skill,
        ),
        (
            "codex",
            ".codex/skills",
            "skill",
            AgentAssetPredicate::Skill,
        ),
        (
            "gemini",
            ".gemini/skills",
            "skill",
            AgentAssetPredicate::Skill,
        ),
    ] {
        let root = paths.vault_root().join(relative_root);
        let destination_root = if kind == "prompt" {
            assistant_prompts_root(paths)
        } else {
            assistant_skills_root(paths)
        };
        for source_path in collect_agent_asset_files(&root, predicate)? {
            let source_display = path_to_forward_slashes(
                source_path
                    .strip_prefix(paths.vault_root())
                    .unwrap_or(&source_path),
            );
            if guard.check_read_path(&source_display).is_err() {
                continue;
            }
            let relative = source_path.strip_prefix(&root).map_err(|error| {
                CliError::operation(format!(
                    "failed to determine relative path for `{}`: {error}",
                    source_path.display()
                ))
            })?;
            let destination_path = destination_root.join(relative);
            let destination_display = path_to_forward_slashes(
                destination_path
                    .strip_prefix(paths.vault_root())
                    .unwrap_or(&destination_path),
            );
            detected.push(DetectedAgentAsset {
                source_layout: layout,
                kind,
                source_path,
                source_display,
                destination_path,
                destination_display,
            });
        }
    }

    detected.sort_by(|left, right| {
        left.destination_display
            .cmp(&right.destination_display)
            .then_with(|| left.source_display.cmp(&right.source_display))
    });
    Ok(detected)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentAssetPredicate {
    Markdown,
    Skill,
}

fn collect_agent_asset_files(
    root: &Path,
    predicate: AgentAssetPredicate,
) -> Result<Vec<PathBuf>, CliError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            CliError::operation(format!("failed to inspect `{}`: {error}", path.display()))
        })?;
        if metadata.is_dir() {
            let mut entries = fs::read_dir(&path)
                .map_err(|error| {
                    CliError::operation(format!("failed to read `{}`: {error}", path.display()))
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    CliError::operation(format!("failed to walk `{}`: {error}", path.display()))
                })?;
            entries.sort_by_key(std::fs::DirEntry::path);
            for entry in entries.into_iter().rev() {
                stack.push(entry.path());
            }
            continue;
        }
        let keep = match predicate {
            AgentAssetPredicate::Markdown => path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("md")),
            AgentAssetPredicate::Skill => path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("SKILL.md")),
        };
        if keep {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn conflicting_destinations(detected: &[DetectedAgentAsset]) -> BTreeMap<String, String> {
    let mut destinations = BTreeMap::<String, Vec<String>>::new();
    for asset in detected {
        destinations
            .entry(asset.destination_display.clone())
            .or_default()
            .push(asset.source_display.clone());
    }
    destinations
        .into_iter()
        .filter(|(_, sources)| sources.len() > 1)
        .map(|(destination, sources)| {
            let message = format!(
                "multiple sources map to `{destination}`: {}",
                sources.join(", ")
            );
            (destination, message)
        })
        .collect()
}

fn preview_agent_asset(
    _paths: &VaultPaths,
    asset: &DetectedAgentAsset,
    mode: AgentImportMode,
    overwrite: bool,
) -> Result<AgentImportItemReport, CliError> {
    let state = import_target_state(asset, mode).map_err(CliError::operation)?;
    let (status, message) = match state {
        ImportTargetState::Missing => (AgentImportStatus::WouldCreate, None),
        ImportTargetState::Unchanged => (AgentImportStatus::Kept, None),
        ImportTargetState::Different if overwrite => (AgentImportStatus::WouldUpdate, None),
        ImportTargetState::Different => (
            AgentImportStatus::Kept,
            Some("destination already exists; rerun with --overwrite to replace it".to_string()),
        ),
    };
    Ok(AgentImportItemReport {
        source_layout: asset.source_layout.to_string(),
        kind: asset.kind.to_string(),
        source_path: asset.source_display.clone(),
        destination_path: asset.destination_display.clone(),
        mode,
        status,
        message,
    })
}

fn apply_agent_asset(
    asset: &DetectedAgentAsset,
    mode: AgentImportMode,
    overwrite: bool,
) -> Result<AgentImportItemReport, CliError> {
    let state = import_target_state(asset, mode).map_err(CliError::operation)?;
    let status = match state {
        ImportTargetState::Missing => {
            write_agent_asset(asset, mode).map_err(CliError::operation)?;
            AgentImportStatus::Created
        }
        ImportTargetState::Unchanged => AgentImportStatus::Kept,
        ImportTargetState::Different if !overwrite => AgentImportStatus::Kept,
        ImportTargetState::Different => {
            replace_import_target(&asset.destination_path).map_err(CliError::operation)?;
            write_agent_asset(asset, mode).map_err(CliError::operation)?;
            AgentImportStatus::Updated
        }
    };
    Ok(AgentImportItemReport {
        source_layout: asset.source_layout.to_string(),
        kind: asset.kind.to_string(),
        source_path: asset.source_display.clone(),
        destination_path: asset.destination_display.clone(),
        mode,
        status,
        message: None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportTargetState {
    Missing,
    Unchanged,
    Different,
}

fn import_target_state(
    asset: &DetectedAgentAsset,
    mode: AgentImportMode,
) -> Result<ImportTargetState, std::io::Error> {
    let destination = &asset.destination_path;
    let metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ImportTargetState::Missing)
        }
        Err(error) => return Err(error),
    };
    match mode {
        AgentImportMode::Copy => {
            let source = fs::read(&asset.source_path)?;
            if metadata.is_file() && fs::read(destination)? == source {
                Ok(ImportTargetState::Unchanged)
            } else {
                Ok(ImportTargetState::Different)
            }
        }
        AgentImportMode::Symlink => {
            if !metadata.file_type().is_symlink() {
                return Ok(ImportTargetState::Different);
            }
            let current = fs::read_link(destination)?;
            let desired = desired_symlink_target(&asset.source_path, destination);
            if current == desired {
                Ok(ImportTargetState::Unchanged)
            } else {
                Ok(ImportTargetState::Different)
            }
        }
    }
}

fn write_agent_asset(
    asset: &DetectedAgentAsset,
    mode: AgentImportMode,
) -> Result<(), std::io::Error> {
    if let Some(parent) = asset.destination_path.parent() {
        fs::create_dir_all(parent)?;
    }
    match mode {
        AgentImportMode::Copy => {
            fs::copy(&asset.source_path, &asset.destination_path)?;
            Ok(())
        }
        AgentImportMode::Symlink => {
            let target = desired_symlink_target(&asset.source_path, &asset.destination_path);
            create_file_symlink(&target, &asset.destination_path)
        }
    }
}

fn replace_import_target(path: &Path) -> Result<(), std::io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn desired_symlink_target(source: &Path, destination: &Path) -> PathBuf {
    let Some(parent) = destination.parent() else {
        return source.to_path_buf();
    };
    diff_paths(source, parent).unwrap_or_else(|| source.to_path_buf())
}

fn diff_paths(target: &Path, base: &Path) -> Option<PathBuf> {
    let target_components = target.components().collect::<Vec<_>>();
    let base_components = base.components().collect::<Vec<_>>();
    if incompatible_path_roots(&target_components, &base_components) {
        return None;
    }

    let mut common = 0usize;
    while common < target_components.len()
        && common < base_components.len()
        && target_components[common] == base_components[common]
    {
        common += 1;
    }

    let mut relative = PathBuf::new();
    for component in &base_components[common..] {
        if !matches!(component, Component::CurDir) {
            relative.push("..");
        }
    }
    for component in &target_components[common..] {
        relative.push(component.as_os_str());
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Some(relative)
}

fn incompatible_path_roots(target: &[Component<'_>], base: &[Component<'_>]) -> bool {
    matches!(
        (target.first(), base.first()),
        (Some(Component::Prefix(left)), Some(Component::Prefix(right))) if left != right
    )
}

#[cfg(unix)]
fn create_file_symlink(target: &Path, path: &Path) -> Result<(), std::io::Error> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, path: &Path) -> Result<(), std::io::Error> {
    std::os::windows::fs::symlink_file(target, path)
}

fn agent_import_status_label(status: AgentImportStatus) -> &'static str {
    match status {
        AgentImportStatus::WouldCreate => "would_create",
        AgentImportStatus::WouldUpdate => "would_update",
        AgentImportStatus::Created => "created",
        AgentImportStatus::Updated => "updated",
        AgentImportStatus::Kept => "kept",
        AgentImportStatus::Conflict => "conflict",
    }
}

fn agent_print_config_commands(paths: &VaultPaths) -> AgentPrintConfigCommands {
    agent_print_config_commands_for_profile(paths, "agent")
}

fn agent_print_config_commands_for_profile(
    paths: &VaultPaths,
    profile: &str,
) -> AgentPrintConfigCommands {
    let base = runtime_base_command(paths, profile);
    AgentPrintConfigCommands {
        describe_openai_tools: format!("{base} describe --format openai-tools"),
        help_json: format!("{base} help assistant-integration"),
        skill_list: format!("{base} skill list"),
        skill_get: format!("{base} skill get <name>"),
        note_get: format!("{base} note get <note>"),
        note_patch: format!("{base} note patch <note> --find <text> --replace <text>"),
    }
}

fn runtime_base_command(paths: &VaultPaths, profile: &str) -> String {
    format!(
        "vulcan --vault {} --permissions {} --output json",
        shell_quote(paths.vault_root()),
        profile
    )
}

fn runtime_snippet(
    runtime: AgentRuntimeArg,
    commands: &AgentPrintConfigCommands,
    profile: &str,
) -> String {
    let runtime_name = agent_runtime_name(runtime);
    match runtime {
        AgentRuntimeArg::Generic => format!(
            "# Generic subprocess contract ({runtime_name}, profile `{profile}`)\n{}\n{}\n{}\n{}",
            commands.describe_openai_tools,
            commands.skill_list,
            commands.help_json,
            commands.note_get,
        ),
        AgentRuntimeArg::Pi => format!(
            "# pi wrapper contract ({profile})\n# 1. Read AGENTS.md if present\n# 2. Register the OpenAI tool export\n# 3. Load skills on demand\n{}\n{}\n{}",
            commands.describe_openai_tools,
            commands.skill_list,
            commands.skill_get,
        ),
        AgentRuntimeArg::Codex => format!(
            "# Codex wrapper contract ({profile})\n# Use Vulcan for vault reads/writes and keep skill bodies on demand.\n{}\n{}\n{}",
            commands.describe_openai_tools,
            commands.skill_list,
            commands.help_json,
        ),
        AgentRuntimeArg::ClaudeCode => format!(
            "# Claude Code wrapper contract ({profile})\n# Keep CLAUDE.md/AGENTS.md guidance compact and shell out to Vulcan for every vault mutation.\n{}\n{}\n{}",
            commands.describe_openai_tools,
            commands.skill_list,
            commands.note_patch,
        ),
        AgentRuntimeArg::GeminiCli => format!(
            "# Gemini CLI wrapper contract ({profile})\n# Discover tools through Vulcan and fetch detailed docs only when needed.\n{}\n{}\n{}",
            commands.describe_openai_tools,
            commands.help_json,
            commands.skill_get,
        ),
    }
}

fn agent_runtime_name(runtime: AgentRuntimeArg) -> &'static str {
    match runtime {
        AgentRuntimeArg::Generic => "generic",
        AgentRuntimeArg::Pi => "pi",
        AgentRuntimeArg::Codex => "codex",
        AgentRuntimeArg::ClaudeCode => "claude-code",
        AgentRuntimeArg::GeminiCli => "gemini-cli",
    }
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn path_to_forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
