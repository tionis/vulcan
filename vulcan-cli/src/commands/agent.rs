use crate::commands::config::{
    discover_config_importers, normalize_config_import_report, normalize_import_discovery_item,
    ConfigImportBatchReport, ConfigImportDiscoveryItem,
};
use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::{
    selected_permission_guard, warn_auto_commit_if_needed, AgentCommand, AgentImportArgs,
    AgentInstallArgs, AgentRuntimeArg, Cli, CliError, InitArgs, OutputFormat,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use vulcan_core::{
    all_importers, annotate_import_conflicts, assistant_config_summary, assistant_prompts_root,
    assistant_skills_root, initialize_vault, list_assistant_prompts, list_assistant_skills,
    load_vault_config, read_vault_agents_file, ImportTarget, InitSummary, PermissionGuard,
    VaultPaths,
};

struct BundledTextFile {
    kind: &'static str,
    relative_path: &'static str,
    contents: &'static str,
    target: BundledFileTarget,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BundledFileTarget {
    VaultRoot,
    SkillsFolder,
    PromptsFolder,
}

const BUNDLED_AGENT_TEMPLATE: BundledTextFile = BundledTextFile {
    kind: "agents_template",
    relative_path: "AGENTS.md",
    contents: include_str!("../../../docs/assistant/AGENTS.template.md"),
    target: BundledFileTarget::VaultRoot,
};

const BUNDLED_SKILL_FILES: &[BundledTextFile] = &[
    BundledTextFile {
        kind: "skill",
        relative_path: "note-operations/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/note-operations.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "vault-query/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/vault-query.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "js-api-guide/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/js-api-guide.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "skill-creator/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/skill-creator.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "graph-exploration/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/graph-exploration.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "link-curation/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/link-curation.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "daily-notes/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/daily-notes.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "properties-and-tags/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/properties-and-tags.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "refactoring/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/refactoring.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "web-research/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/web-research.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "git-workflow/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/git-workflow.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "task-management/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/task-management.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "configuration-and-permissions/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/configuration-and-permissions.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "mcp-setup/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/mcp-setup.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "index-maintenance/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/index-maintenance.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "dataview-and-bases/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/dataview-and-bases.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "templates-and-capture/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/templates-and-capture.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "publishing-and-export/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/publishing-and-export.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "plugin-authoring/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/plugin-authoring.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "diagnostics-and-repair/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/diagnostics-and-repair.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "conversation-export/SKILL.md",
        contents: include_str!("../../../docs/assistant/skills/conversation-export.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "conversation-export/scripts/export-conversation.js",
        contents: include_str!(
            "../../../docs/assistant/skills/conversation-export/export-conversation.js"
        ),
        target: BundledFileTarget::SkillsFolder,
    },
];

const BUNDLED_PROMPT_FILES: &[BundledTextFile] = &[
    BundledTextFile {
        kind: "prompt",
        relative_path: "summarize-note.md",
        contents: include_str!("../../../docs/assistant/prompts/summarize-note.md"),
        target: BundledFileTarget::PromptsFolder,
    },
    BundledTextFile {
        kind: "prompt",
        relative_path: "daily-review.md",
        contents: include_str!("../../../docs/assistant/prompts/daily-review.md"),
        target: BundledFileTarget::PromptsFolder,
    },
];

const BUNDLED_TOOL_FILES: &[BundledTextFile] = &[
    BundledTextFile {
        kind: "skill",
        relative_path: "summarize-note/SKILL.md",
        contents: r"---
name: summarize-note
description: Summarize one vault note.
license: UNLICENSED
compatibility:
  - vulcan
allowed-tools:
  - note_get
metadata:
  vulcan:
    commands:
      - id: summarize
        script: scripts/summarize.js
        sandbox: fs
        packs: [custom]
        expose: true
        input_schema:
          type: object
          required: [note]
          properties:
            note:
              type: string
        output_schema:
          type: object
          required: [note, summary]
          properties:
            note:
              type: string
            summary:
              type: string
---

# Summarize Note

Use this skill command as a minimal Agent Skills-compatible executable example.
",
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "summarize-note/scripts/summarize.js",
        contents: "#!/usr/bin/env -S vulcan skill exec\nfunction main(input) {\n  return {\n    note: input.note,\n    summary: `TODO: summarize ${input.note}`,\n  };\n}\n",
        target: BundledFileTarget::SkillsFolder,
    },
];

pub(crate) fn bundled_support_relative_paths(paths: &VaultPaths) -> Vec<String> {
    std::iter::once(&BUNDLED_AGENT_TEMPLATE)
        .chain(BUNDLED_SKILL_FILES.iter())
        .chain(BUNDLED_PROMPT_FILES.iter())
        .map(|file| bundled_text_file_display_path(paths, file))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SupportFileStatus {
    Created,
    Updated,
    Kept,
}

impl SupportFileStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Kept => "kept",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SupportFileReport {
    path: String,
    kind: String,
    status: SupportFileStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct InitReport {
    #[serde(flatten)]
    summary: InitSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    importable_sources: Vec<ConfigImportDiscoveryItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    support_files: Vec<SupportFileReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    imported: Option<ConfigImportBatchReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AgentInstallReport {
    support_files: Vec<SupportFileReport>,
}

#[allow(clippy::large_enum_variant)]
pub(crate) fn run_init_command(
    paths: &VaultPaths,
    args: &InitArgs,
) -> Result<InitReport, CliError> {
    let summary = initialize_vault(paths).map_err(CliError::operation)?;
    let support_files = if args.agent_files {
        write_bundled_support_files(paths, false, args.example_tool)?
    } else {
        Vec::new()
    };
    let importable_sources = if args.no_import {
        Vec::new()
    } else {
        discover_config_importers(paths)
            .into_iter()
            .filter_map(|(_, discovery)| discovery.detected.then_some(discovery))
            .collect()
    };
    let imported = if args.import {
        let target = ImportTarget::Shared;
        let mut reports = Vec::new();
        for importer in all_importers()
            .into_iter()
            .filter(|importer| importer.detect(paths))
        {
            reports.push(
                importer
                    .import(paths, target)
                    .map_err(CliError::operation)?,
            );
        }
        annotate_import_conflicts(&mut reports);
        Some(ConfigImportBatchReport {
            dry_run: false,
            target,
            detected_count: reports.len(),
            imported_count: reports.len(),
            updated_count: reports.iter().filter(|report| report.updated).count(),
            reports,
        })
    } else {
        None
    };
    Ok(InitReport {
        summary,
        importable_sources,
        support_files,
        imported,
    })
}

pub(crate) fn run_agent_install_command(
    paths: &VaultPaths,
    args: &AgentInstallArgs,
) -> Result<AgentInstallReport, CliError> {
    Ok(AgentInstallReport {
        support_files: write_bundled_support_files(paths, args.overwrite, args.example_tool)?,
    })
}

pub(crate) fn print_init_summary(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &InitReport,
) -> Result<(), CliError> {
    let normalized_importable = report
        .importable_sources
        .iter()
        .map(|item| normalize_import_discovery_item(paths, item))
        .collect::<Vec<_>>();
    let normalized_imported = report
        .imported
        .as_ref()
        .map(|batch| ConfigImportBatchReport {
            dry_run: batch.dry_run,
            target: batch.target,
            detected_count: batch.detected_count,
            imported_count: batch.imported_count,
            updated_count: batch.updated_count,
            reports: batch
                .reports
                .iter()
                .map(|item| normalize_config_import_report(paths, item))
                .collect(),
        });
    let normalized = InitReport {
        summary: report.summary.clone(),
        importable_sources: normalized_importable,
        support_files: report.support_files.clone(),
        imported: normalized_imported,
    };

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Initialized {} (config {}, cache {})",
                normalized.summary.vault_root.display(),
                if normalized.summary.created_config {
                    "created"
                } else {
                    "existing"
                },
                if normalized.summary.created_cache {
                    "created"
                } else {
                    "existing"
                },
            );
            if let Some(imported) = &normalized.imported {
                println!(
                    "Imported {} detected importer{} ({} updated).",
                    imported.imported_count,
                    if imported.imported_count == 1 {
                        ""
                    } else {
                        "s"
                    },
                    imported.updated_count
                );
            } else if !normalized.importable_sources.is_empty() {
                println!("Importable settings detected:");
                for importer in &normalized.importable_sources {
                    println!("- {} ({})", importer.plugin, importer.display_name);
                }
                println!("Run `vulcan config import --all` to import them.");
            }
            if !normalized.support_files.is_empty() {
                println!("Bundled agent support files:");
                print_support_file_reports(&normalized.support_files);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&normalized),
    }
}

pub(crate) fn print_agent_install_summary(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &AgentInstallReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Installed bundled agent support files for {}",
                paths.vault_root().display()
            );
            print_support_file_reports(&report.support_files);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_support_file_reports(files: &[SupportFileReport]) {
    for file in files {
        println!("- {} [{}; {}]", file.path, file.kind, file.status.label());
    }
}

fn write_bundled_support_files(
    paths: &VaultPaths,
    overwrite: bool,
    include_example_tool: bool,
) -> Result<Vec<SupportFileReport>, CliError> {
    let mut reports = Vec::new();
    reports.push(write_bundled_text_file(
        paths,
        &BUNDLED_AGENT_TEMPLATE,
        overwrite,
    )?);
    for file in BUNDLED_SKILL_FILES {
        reports.push(write_bundled_text_file(paths, file, overwrite)?);
    }
    for file in BUNDLED_PROMPT_FILES {
        reports.push(write_bundled_text_file(paths, file, overwrite)?);
    }
    if include_example_tool {
        for file in BUNDLED_TOOL_FILES {
            reports.push(write_bundled_text_file(paths, file, overwrite)?);
        }
    }
    Ok(reports)
}

fn write_bundled_text_file(
    paths: &VaultPaths,
    file: &BundledTextFile,
    overwrite: bool,
) -> Result<SupportFileReport, CliError> {
    let destination = bundled_text_file_destination(paths, file);
    let status = write_bundled_text_contents(&destination, file.contents, overwrite)?;
    #[cfg(unix)]
    if bundled_text_file_should_be_executable(file) && destination.exists() {
        set_executable_permissions(&destination)?;
    }
    Ok(SupportFileReport {
        path: bundled_text_file_display_path(paths, file),
        kind: file.kind.to_string(),
        status,
    })
}

#[cfg(unix)]
fn bundled_text_file_should_be_executable(file: &BundledTextFile) -> bool {
    file.target == BundledFileTarget::SkillsFolder
        && Path::new(file.relative_path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("js"))
}

fn bundled_text_file_destination(paths: &VaultPaths, file: &BundledTextFile) -> PathBuf {
    let config = load_vault_config(paths).config.assistant;
    match file.target {
        BundledFileTarget::VaultRoot => paths.vault_root().join(file.relative_path),
        BundledFileTarget::SkillsFolder => paths
            .vault_root()
            .join(config.skills_folder)
            .join(file.relative_path),
        BundledFileTarget::PromptsFolder => paths
            .vault_root()
            .join(config.prompts_folder)
            .join(file.relative_path),
    }
}

fn bundled_text_file_display_path(paths: &VaultPaths, file: &BundledTextFile) -> String {
    let destination = bundled_text_file_destination(paths, file);
    destination
        .strip_prefix(paths.vault_root())
        .unwrap_or(&destination)
        .to_string_lossy()
        .replace('\\', "/")
}

fn write_bundled_text_contents(
    path: &Path,
    contents: &str,
    overwrite: bool,
) -> Result<SupportFileStatus, CliError> {
    let rendered = if contents.ends_with('\n') {
        contents.as_bytes().to_vec()
    } else {
        format!("{contents}\n").into_bytes()
    };
    let existed_before = match fs::read(path) {
        Ok(existing) => {
            if existing == rendered {
                return Ok(SupportFileStatus::Kept);
            }
            if !overwrite {
                return Ok(SupportFileStatus::Kept);
            }
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(CliError::operation(error)),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    fs::write(path, &rendered).map_err(CliError::operation)?;
    Ok(if existed_before {
        SupportFileStatus::Updated
    } else {
        SupportFileStatus::Created
    })
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(CliError::operation)?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions).map_err(CliError::operation)
}

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
            for path in bundled_support_relative_paths(paths) {
                guard.check_write_path(&path).map_err(CliError::operation)?;
            }
            let report = run_agent_install_command(paths, args)?;
            print_agent_install_summary(cli.output, paths, &report)
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
