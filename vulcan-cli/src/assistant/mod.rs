use crate::cli::{McpToolPackArg, McpToolPackModeArg};
use crate::output::print_json;
use crate::OutputFormat;
use crate::{mcp, CliError, McpToolsReport};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use vulcan_app::assistant_session_export::export_assistant_session_file;
use vulcan_core::{
    list_assistant_skills, load_vault_config, read_vault_agents_file, AssistantSkillSummary,
    VaultPaths,
};

pub(crate) mod chat;
pub(crate) mod engine;
pub(crate) mod extension;
pub(crate) mod renderer;
pub(crate) mod rpc;
pub(crate) mod rpc_events;

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct AssistantCommandOptions {
    pub(crate) prompt: Vec<String>,
    pub(crate) doctor: bool,
    pub(crate) print_context: bool,
    pub(crate) list_sessions: bool,
    pub(crate) chat: bool,
    pub(crate) resume: bool,
    pub(crate) continue_session: bool,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) thinking: Option<String>,
    pub(crate) show_thinking: bool,
    pub(crate) assistant_pi_binary: Option<String>,
    pub(crate) assistant_permissions: Option<String>,
    pub(crate) ephemeral: bool,
    pub(crate) no_tools: bool,
    pub(crate) tool_pack: Vec<McpToolPackArg>,
    pub(crate) tool_pack_mode: McpToolPackModeArg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssistantHostOptions {
    pub(crate) runtime: String,
    pub(crate) pi_binary: String,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) thinking_level: Option<String>,
    pub(crate) permission_profile: Option<String>,
    pub(crate) sessions_dir: Option<PathBuf>,
    pub(crate) no_tools: bool,
    pub(crate) extension_entrypoint: Option<PathBuf>,
    pub(crate) extension_env: Vec<(String, String)>,
    pub(crate) resume_session: Option<PathBuf>,
    pub(crate) session_export: String,
    pub(crate) session_exports_dir: Option<PathBuf>,
}

impl AssistantHostOptions {
    pub(crate) fn from_config(paths: &VaultPaths) -> Self {
        let config = load_vault_config(paths).config.assistant;
        Self {
            runtime: config.runtime,
            pi_binary: config.pi_binary,
            provider: non_empty(config.provider),
            model: non_empty(config.model),
            thinking_level: non_empty(config.thinking_level),
            permission_profile: non_empty(config.permissions),
            sessions_dir: if config.sessions_dir.as_os_str().is_empty() {
                None
            } else {
                Some(config.sessions_dir)
            },
            no_tools: false,
            extension_entrypoint: None,
            extension_env: Vec::new(),
            resume_session: None,
            session_export: config.session_export,
            session_exports_dir: if config.session_exports_dir.as_os_str().is_empty() {
                None
            } else {
                Some(config.session_exports_dir)
            },
        }
    }

    pub(crate) fn resolved_sessions_dir(&self, vault_root: &Path) -> Option<PathBuf> {
        self.sessions_dir.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                vault_root.join(path)
            }
        })
    }

    pub(crate) fn resolved_session_exports_dir(&self, vault_root: &Path) -> Option<PathBuf> {
        self.session_exports_dir.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                vault_root.join(path)
            }
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AssistantHostContext {
    pub(crate) vault_root: String,
    pub(crate) permission_profile: String,
    pub(crate) agents_file: Option<String>,
    pub(crate) skills: Vec<AssistantContextSkill>,
    pub(crate) tools: Option<AssistantContextTools>,
    pub(crate) instructions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AssistantContextSkill {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) path: String,
    pub(crate) description: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AssistantContextTools {
    pub(crate) protocol_version: String,
    pub(crate) tool_pack_mode: String,
    pub(crate) selected_tool_packs: Vec<String>,
    pub(crate) tools: Vec<AssistantContextTool>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AssistantContextTool {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) tool_packs: Vec<String>,
    pub(crate) read_only: bool,
    pub(crate) destructive: bool,
}

pub(crate) fn build_host_context(
    paths: &VaultPaths,
    options: &AssistantHostOptions,
    tool_packs: &[McpToolPackArg],
    tool_pack_mode: McpToolPackModeArg,
) -> Result<AssistantHostContext, CliError> {
    let permission_profile = options
        .permission_profile
        .clone()
        .unwrap_or_else(|| "readonly".to_string());
    let agents_file = read_vault_agents_file(paths).map_err(CliError::operation)?;
    let skills = list_assistant_skills(paths)
        .map_err(CliError::operation)?
        .into_iter()
        .map(context_skill)
        .collect::<Vec<_>>();
    let tools = if options.no_tools {
        None
    } else {
        Some(context_tools(mcp::build_mcp_tool_definitions(
            paths,
            Some(&permission_profile),
            tool_packs,
            tool_pack_mode,
        )?))
    };

    Ok(AssistantHostContext {
        vault_root: paths.vault_root().display().to_string(),
        permission_profile,
        agents_file,
        skills,
        tools,
        instructions: vec![
            "Vulcan owns vault permissions, context assembly, and tool execution.".to_string(),
            "The managed engine is an RPC peer; do not assume it may bypass Vulcan permission profiles.".to_string(),
            "Prefer first-class Vulcan tools for notes, daily notes, tasks, search, and status.".to_string(),
        ],
    })
}

fn context_skill(skill: AssistantSkillSummary) -> AssistantContextSkill {
    AssistantContextSkill {
        name: skill.name,
        title: skill.title.unwrap_or_default(),
        path: skill.path,
        description: skill.description.unwrap_or_default(),
    }
}

fn context_tools(report: McpToolsReport) -> AssistantContextTools {
    AssistantContextTools {
        protocol_version: report.protocol_version,
        tool_pack_mode: report.tool_pack_mode,
        selected_tool_packs: report.selected_tool_packs,
        tools: report
            .tools
            .into_iter()
            .map(|tool| AssistantContextTool {
                name: tool.name,
                title: tool.title,
                description: tool.description,
                tool_packs: tool.tool_packs,
                read_only: tool.annotations.read_only_hint,
                destructive: tool.annotations.destructive_hint,
            })
            .collect(),
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

pub(crate) fn handle_assistant_command(
    paths: &VaultPaths,
    output: OutputFormat,
    args: &AssistantCommandOptions,
) -> Result<(), CliError> {
    let mut host = AssistantHostOptions::from_config(paths);
    apply_cli_overrides(&mut host, args);

    if args.doctor {
        return print_doctor_report(output, &engine::doctor(&host, paths.vault_root()));
    }

    if args.print_context {
        let context = build_host_context(paths, &host, &args.tool_pack, args.tool_pack_mode)?;
        return print_context_report(output, &context);
    }

    if args.list_sessions {
        let report = list_sessions(paths, &host)?;
        return print_session_report(output, &report);
    }

    if args.resume || args.continue_session {
        host.resume_session = newest_session_path(paths, &host)?;
    }

    prepare_extension(paths, &mut host, &args.tool_pack)?;
    if args.chat {
        let context = build_host_context(paths, &host, &args.tool_pack, args.tool_pack_mode)?;
        return chat::run_chat(
            paths,
            &host,
            &context,
            &args.prompt,
            args.show_thinking,
            output,
        );
    }

    let prompt = prompt_text(&args.prompt)?;
    let context = build_host_context(paths, &host, &args.tool_pack, args.tool_pack_mode)?;
    let mut process = engine::spawn_pi_rpc(&host, paths.vault_root())?;
    process.ensure_running()?;
    let mut configure = Map::new();
    configure.insert(
        "context".to_string(),
        serde_json::to_value(&context).map_err(CliError::operation)?,
    );
    let configure_result = process.client.command("configure", configure)?;
    ensure_success(&configure_result.response)?;

    let result = process.client.prompt(&prompt)?;
    ensure_success(&result.response)?;
    process.shutdown()?;
    export_session_after_run(paths, &host)?;

    if output == OutputFormat::Json {
        let report = render_events_to_report(&result.events, args.show_thinking, true)?;
        return print_json(&AssistantRunReport {
            response: result.response.data,
            render: report,
            events: result
                .events
                .iter()
                .map(renderer::event_output_value)
                .collect(),
        });
    }

    let report = render_events_to_report(&result.events, args.show_thinking, false)?;
    if output == OutputFormat::Markdown && !report.text.is_empty() {
        println!();
    }
    Ok(())
}

pub(crate) fn export_session_after_run(
    paths: &VaultPaths,
    host: &AssistantHostOptions,
) -> Result<(), CliError> {
    if !matches!(host.session_export.as_str(), "on_exit" | "always") {
        return Ok(());
    }
    let Some(session_path) = host
        .resume_session
        .clone()
        .or_else(|| newest_session_path(paths, host).ok().flatten())
    else {
        return Ok(());
    };
    let Some(export_dir) = host.resolved_session_exports_dir(paths.vault_root()) else {
        return Err(CliError::operation(
            "assistant session export is enabled but session_exports_dir is empty",
        ));
    };
    export_assistant_session_file(paths.vault_root(), &session_path, &export_dir)?;
    Ok(())
}

fn apply_cli_overrides(host: &mut AssistantHostOptions, args: &AssistantCommandOptions) {
    if let Some(provider) = args.provider.clone() {
        host.provider = Some(provider);
    }
    if let Some(model) = args.model.clone() {
        host.model = Some(model);
    }
    if let Some(thinking) = args.thinking.clone() {
        host.thinking_level = Some(thinking);
    }
    if let Some(binary) = args.assistant_pi_binary.clone() {
        host.pi_binary = binary;
    }
    if let Some(profile) = args.assistant_permissions.clone() {
        host.permission_profile = Some(profile);
    }
    if args.ephemeral {
        host.sessions_dir = None;
    }
    if args.no_tools {
        host.no_tools = true;
    }
}

fn prepare_extension(
    paths: &VaultPaths,
    host: &mut AssistantHostOptions,
    tool_packs: &[McpToolPackArg],
) -> Result<(), CliError> {
    if host.no_tools || host.runtime != "pi" {
        return Ok(());
    }
    let install = extension::materialize_extension(paths.vault_root())?;
    host.extension_entrypoint = Some(install.entrypoint);
    host.extension_env = extension::extension_environment(host, paths.vault_root(), tool_packs);
    Ok(())
}

fn prompt_text(args: &[String]) -> Result<String, CliError> {
    if !args.is_empty() {
        return Ok(args.join(" "));
    }
    if io::stdin().is_terminal() {
        return Err(CliError::operation(
            "assistant prompt is required unless stdin is piped",
        ));
    }
    let mut prompt = String::new();
    io::stdin()
        .read_to_string(&mut prompt)
        .map_err(CliError::operation)?;
    Ok(prompt.trim_end().to_string())
}

fn ensure_success(response: &rpc::RpcResponse) -> Result<(), CliError> {
    if response.success {
        Ok(())
    } else {
        Err(CliError::operation(response.error.clone().unwrap_or_else(
            || "managed assistant engine returned an error".to_string(),
        )))
    }
}

fn render_events_to_report(
    events: &[rpc::AssistantEvent],
    show_thinking: bool,
    json_events: bool,
) -> Result<renderer::AssistantRenderReport, CliError> {
    let writer: Box<dyn io::Write> = if json_events {
        Box::new(Vec::<u8>::new())
    } else {
        Box::new(io::stdout())
    };
    let mut renderer = renderer::AssistantRenderer::new(
        writer,
        renderer::RenderOptions {
            show_thinking,
            json_events,
        },
    );
    for event in events {
        renderer.render_event(event)?;
    }
    renderer.finish()
}

#[derive(Debug, Clone, Serialize)]
struct AssistantRunReport {
    response: Value,
    render: renderer::AssistantRenderReport,
    events: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AssistantSessionListReport {
    sessions_dir: Option<String>,
    sessions: Vec<AssistantSessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AssistantSessionSummary {
    path: String,
    modified_unix: Option<u64>,
    bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_count: Option<u64>,
}

fn list_sessions(
    paths: &VaultPaths,
    host: &AssistantHostOptions,
) -> Result<AssistantSessionListReport, CliError> {
    let Some(sessions_dir) = host.resolved_sessions_dir(paths.vault_root()) else {
        return Ok(AssistantSessionListReport {
            sessions_dir: None,
            sessions: Vec::new(),
        });
    };
    if !sessions_dir.exists() {
        return Ok(AssistantSessionListReport {
            sessions_dir: Some(sessions_dir.display().to_string()),
            sessions: Vec::new(),
        });
    }
    let mut sessions = fs::read_dir(&sessions_dir)
        .map_err(CliError::operation)?
        .filter_map(Result::ok)
        .filter_map(|entry| session_summary(&sessions_dir, &entry.path()).transpose())
        .collect::<Result<Vec<_>, CliError>>()?;
    sessions.sort_by(|left, right| right.modified_unix.cmp(&left.modified_unix));
    Ok(AssistantSessionListReport {
        sessions_dir: Some(sessions_dir.display().to_string()),
        sessions,
    })
}

fn newest_session_path(
    paths: &VaultPaths,
    host: &AssistantHostOptions,
) -> Result<Option<PathBuf>, CliError> {
    let sessions = list_sessions(paths, host)?;
    let Some(sessions_dir) = sessions.sessions_dir else {
        return Ok(None);
    };
    Ok(sessions
        .sessions
        .first()
        .map(|session| PathBuf::from(&sessions_dir).join(&session.path)))
}

fn session_summary(root: &Path, path: &Path) -> Result<Option<AssistantSessionSummary>, CliError> {
    let metadata = path.metadata().map_err(CliError::operation)?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let relative = path.strip_prefix(root).unwrap_or(path);
    let modified_unix = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    let header = read_session_header(path);
    Ok(Some(AssistantSessionSummary {
        path: relative.display().to_string(),
        modified_unix,
        bytes: metadata.len(),
        session_id: header
            .as_ref()
            .and_then(|header| string_field(header, &["session_id", "id"])),
        title: header
            .as_ref()
            .and_then(|header| string_field(header, &["title", "name", "session_name"])),
        message_count: header
            .as_ref()
            .and_then(|header| u64_field(header, &["message_count", "messages_count"])),
    }))
}

fn read_session_header(path: &Path) -> Option<Value> {
    let contents = fs::read_to_string(path).ok()?;
    contents
        .lines()
        .take(20)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| {
            value
                .get("session")
                .filter(|session| session.is_object())
                .cloned()
                .or_else(|| value.is_object().then_some(value))
        })
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(ToString::to_string)
}

fn u64_field(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
        .or_else(|| {
            value
                .get("messages")
                .and_then(Value::as_array)
                .map(|messages| messages.len() as u64)
        })
}

fn print_doctor_report(
    output: OutputFormat,
    report: &engine::EngineDoctorReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("runtime: {}", report.runtime);
            println!("configured binary: {}", report.configured_binary);
            println!(
                "resolved binary: {}",
                report.resolved_binary.as_deref().unwrap_or("(not found)")
            );
            println!("available: {}", report.available);
            if !report.launch_args.is_empty() {
                println!("launch args: {}", report.launch_args.join(" "));
            }
            for note in &report.notes {
                println!("note: {note}");
            }
            Ok(())
        }
    }
}

fn print_context_report(
    output: OutputFormat,
    context: &AssistantHostContext,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(context),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("vault: {}", context.vault_root);
            println!("permission profile: {}", context.permission_profile);
            println!("agents file: {}", context.agents_file.is_some());
            println!("skills: {}", context.skills.len());
            let tool_count = context.tools.as_ref().map_or(0, |tools| tools.tools.len());
            println!("tools: {tool_count}");
            Ok(())
        }
    }
}

fn print_session_report(
    output: OutputFormat,
    report: &AssistantSessionListReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "sessions dir: {}",
                report.sessions_dir.as_deref().unwrap_or("(ephemeral)")
            );
            for session in &report.sessions {
                let label = session.title.as_deref().unwrap_or(&session.path);
                let count = session
                    .message_count
                    .map_or_else(|| "?".to_string(), |count| count.to_string());
                println!("{label} [{} messages; {} bytes]", count, session.bytes);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn options_resolve_relative_sessions_dir_under_vault() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let mut options = AssistantHostOptions::from_config(&VaultPaths::new(temp_dir.path()));

        assert_eq!(
            options.resolved_sessions_dir(temp_dir.path()),
            Some(temp_dir.path().join("AI/Sessions"))
        );

        options.sessions_dir = Some(PathBuf::from("/tmp/vulcan-sessions"));
        assert_eq!(
            options.resolved_sessions_dir(temp_dir.path()),
            Some(PathBuf::from("/tmp/vulcan-sessions"))
        );
    }

    #[test]
    fn host_context_includes_agents_skills_and_filtered_tools() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault = temp_dir.path();
        fs::create_dir_all(vault.join(".agents/skills/daily-review"))
            .expect("skill dir should be created");
        fs::write(
            vault.join("AGENTS.md"),
            "# Vault Rules\n\nUse daily notes.\n",
        )
        .expect("agents file should be written");
        fs::write(
            vault.join(".agents/skills/daily-review/SKILL.md"),
            r"---
name: daily-review
title: Daily Review
description: Review today's notes.
---
# Daily Review
",
        )
        .expect("skill should be written");

        let paths = VaultPaths::new(vault);
        let options = AssistantHostOptions::from_config(&paths);
        let context = build_host_context(
            &paths,
            &options,
            &[McpToolPackArg::NotesRead, McpToolPackArg::Status],
            McpToolPackModeArg::Static,
        )
        .expect("context should build");

        assert_eq!(context.permission_profile, "readonly");
        assert!(context
            .agents_file
            .as_deref()
            .is_some_and(|contents| contents.contains("Use daily notes.")));
        assert_eq!(context.skills.len(), 1);
        assert_eq!(context.skills[0].name, "daily-review");
        let tools = context.tools.expect("tools should be included");
        assert!(tools.tools.iter().any(|tool| tool.name == "note_get"));
        assert!(!tools.tools.iter().any(|tool| tool.destructive));
    }
}
