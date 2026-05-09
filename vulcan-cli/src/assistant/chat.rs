use crate::assistant::engine;
use crate::assistant::renderer::{AssistantRenderReport, AssistantRenderer, RenderOptions};
use crate::assistant::{export_session_after_run, AssistantHostContext, AssistantHostOptions};
use crate::{CliError, OutputFormat};
use serde_json::{Map, Value};
#[cfg(test)]
use std::borrow::Cow;
use std::io::{self, IsTerminal, Read, Write};
use vulcan_core::VaultPaths;

#[cfg(test)]
use crate::cli::Cli;
#[cfg(test)]
use clap::CommandFactory;
#[cfg(test)]
use rustyline::completion::{Completer, Pair};
#[cfg(test)]
use rustyline::highlight::Highlighter;
#[cfg(test)]
use rustyline::hint::Hinter;
#[cfg(test)]
use rustyline::validate::Validator;
#[cfg(test)]
use rustyline::{Context, Helper};
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::Path;

#[cfg(test)]
const CHAT_SLASH_COMMANDS: &[&str] = &[
    "/compact",
    "/exit",
    "/follow-up",
    "/followup",
    "/help",
    "/model",
    "/models",
    "/new",
    "/quit",
    "/set-model",
    "/state",
    "/stats",
    "/steer",
    "/thinking",
    "/vulcan",
];

pub(crate) fn run_chat(
    paths: &VaultPaths,
    host: &AssistantHostOptions,
    _context: &AssistantHostContext,
    initial_prompt: &[String],
    show_thinking: bool,
    output: OutputFormat,
) -> Result<(), CliError> {
    if io::stdin().is_terminal() {
        engine::run_pi_interactive(host, paths.vault_root(), initial_prompt)?;
        export_session_after_run(paths, host)?;
        return Ok(());
    }

    let mut process = engine::spawn_pi_rpc(host, paths.vault_root())?;
    process.ensure_running()?;

    if !initial_prompt.is_empty() {
        send_and_render(
            &mut process.client,
            &initial_prompt.join(" "),
            show_thinking,
            output,
        )?;
    }

    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(CliError::operation)?;
    for line in input.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.starts_with('/') {
            if handle_slash_command(&mut process.client, line, output)?.should_quit {
                break;
            }
        } else {
            send_and_render(&mut process.client, line, show_thinking, output)?;
        }
    }
    process.shutdown()?;
    export_session_after_run(paths, host)
}

fn send_and_render<R: std::io::BufRead, W: std::io::Write>(
    client: &mut crate::assistant::rpc::ManagedRpcClient<R, W>,
    prompt: &str,
    show_thinking: bool,
    output: OutputFormat,
) -> Result<AssistantRenderReport, CliError> {
    let result = client.prompt(prompt)?;
    ensure_success(&result.response)?;
    let json_events = output == OutputFormat::Json;
    let writer: Box<dyn Write> = if json_events {
        Box::new(Vec::<u8>::new())
    } else {
        Box::new(io::stdout())
    };
    let mut renderer = AssistantRenderer::new(
        writer,
        RenderOptions {
            show_thinking,
            json_events,
        },
    );
    for event in &result.events {
        renderer.render_event(event)?;
    }
    renderer.finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SlashCommandResult {
    should_quit: bool,
}

fn handle_slash_command<R: std::io::BufRead, W: std::io::Write>(
    client: &mut crate::assistant::rpc::ManagedRpcClient<R, W>,
    line: &str,
    output: OutputFormat,
) -> Result<SlashCommandResult, CliError> {
    let mut parts = line.split_whitespace();
    let command = parts.next().unwrap_or_default();
    match command {
        "/quit" | "/exit" => Ok(SlashCommandResult { should_quit: true }),
        "/help" => {
            print_chat_help();
            Ok(SlashCommandResult { should_quit: false })
        }
        "/model" => {
            let result = client.command("cycle_model", Map::new())?;
            print_rpc_data(output, &result.response.data)?;
            Ok(SlashCommandResult { should_quit: false })
        }
        "/models" => {
            let models = client.get_available_models()?;
            print_rpc_data(
                output,
                &serde_json::to_value(models).map_err(CliError::operation)?,
            )?;
            Ok(SlashCommandResult { should_quit: false })
        }
        "/thinking" => {
            let result = client.command("cycle_thinking_level", Map::new())?;
            print_rpc_data(output, &result.response.data)?;
            Ok(SlashCommandResult { should_quit: false })
        }
        "/compact" => {
            let compact = client.compact()?;
            print_rpc_data(
                output,
                &serde_json::to_value(compact).map_err(CliError::operation)?,
            )?;
            Ok(SlashCommandResult { should_quit: false })
        }
        "/new" => {
            let cancelled = client.new_session()?;
            print_rpc_data(output, &serde_json::json!({ "cancelled": cancelled }))?;
            Ok(SlashCommandResult { should_quit: false })
        }
        "/stats" => {
            let result = client.command("get_session_stats", Map::new())?;
            print_rpc_data(output, &result.response.data)?;
            Ok(SlashCommandResult { should_quit: false })
        }
        "/state" => {
            let state = client.get_state()?;
            print_rpc_data(
                output,
                &serde_json::to_value(state).map_err(CliError::operation)?,
            )?;
            Ok(SlashCommandResult { should_quit: false })
        }
        "/steer" => {
            let message = parts.collect::<Vec<_>>().join(" ");
            let result = client.steer(&message)?;
            print_rpc_data(output, &result.response.data)?;
            Ok(SlashCommandResult { should_quit: false })
        }
        "/follow-up" | "/followup" => {
            let message = parts.collect::<Vec<_>>().join(" ");
            let result = client.follow_up(&message)?;
            print_rpc_data(output, &result.response.data)?;
            Ok(SlashCommandResult { should_quit: false })
        }
        "/set-model" => {
            let provider = parts
                .next()
                .ok_or_else(|| CliError::operation("usage: /set-model <provider> <model-id>"))?;
            let model = parts
                .next()
                .ok_or_else(|| CliError::operation("usage: /set-model <provider> <model-id>"))?;
            let result = client.set_model(provider, model)?;
            print_rpc_data(output, &result.response.data)?;
            Ok(SlashCommandResult { should_quit: false })
        }
        _ => Err(CliError::operation(format!(
            "unknown assistant command `{command}`; use /help"
        ))),
    }
}

fn print_chat_help() {
    println!(
        "Commands: /model /models /thinking /compact /new /stats /state /steer <text> /follow-up <text> /set-model <provider> <model> /vulcan <command> /quit"
    );
}

fn print_rpc_data(output: OutputFormat, data: &Value) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string(data).map_err(CliError::operation)?
            );
        }
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{}",
                serde_json::to_string_pretty(data).map_err(CliError::operation)?
            );
        }
    }
    Ok(())
}

fn ensure_success(response: &crate::assistant::rpc::RpcResponse) -> Result<(), CliError> {
    if response.success {
        Ok(())
    } else {
        Err(CliError::operation(response.error.clone().unwrap_or_else(
            || "managed assistant engine returned an error".to_string(),
        )))
    }
}

#[cfg(test)]
#[derive(Clone)]
struct AssistantChatHelper {
    slash_commands: Vec<String>,
    vulcan_commands: Vec<String>,
    vault_paths: Vec<String>,
}

#[cfg(test)]
impl AssistantChatHelper {
    fn new(vault_root: &Path) -> Self {
        Self {
            slash_commands: CHAT_SLASH_COMMANDS
                .iter()
                .map(|command| (*command).to_string())
                .collect(),
            vulcan_commands: collect_vulcan_command_paths(),
            vault_paths: collect_chat_vault_paths(vault_root),
        }
    }

    fn complete_line(&self, line: &str, pos: usize) -> (usize, Vec<Pair>) {
        let prefix = &line[..pos.min(line.len())];
        if let Some((start, needle)) = at_path_completion(prefix) {
            return (
                start,
                completion_pairs(
                    self.vault_paths
                        .iter()
                        .filter(|path| path.starts_with(needle)),
                ),
            );
        }
        if let Some(needle) = prefix.strip_prefix("/vulcan ") {
            let needle = needle.trim_start();
            let start = pos.saturating_sub(needle.len());
            return (
                start,
                completion_pairs(
                    self.vulcan_commands
                        .iter()
                        .filter(|command| command.starts_with(needle)),
                ),
            );
        }
        if prefix.starts_with('/') && !prefix.contains(char::is_whitespace) {
            return (
                0,
                completion_pairs(
                    self.slash_commands
                        .iter()
                        .filter(|command| command.starts_with(prefix)),
                ),
            );
        }
        (pos, Vec::new())
    }
}

#[cfg(test)]
impl Helper for AssistantChatHelper {}

#[cfg(test)]
impl Hinter for AssistantChatHelper {
    type Hint = String;
}

#[cfg(test)]
impl Highlighter for AssistantChatHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Borrowed(hint)
    }
}

#[cfg(test)]
impl Validator for AssistantChatHelper {}

#[cfg(test)]
impl Completer for AssistantChatHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _context: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        Ok(self.complete_line(line, pos))
    }
}

#[cfg(test)]
fn at_path_completion(prefix: &str) -> Option<(usize, &str)> {
    let at = prefix.rfind('@')?;
    let needle = &prefix[at + 1..];
    if needle.contains(char::is_whitespace) {
        return None;
    }
    Some((at + 1, needle))
}

#[cfg(test)]
fn completion_pairs<'a>(values: impl Iterator<Item = &'a String>) -> Vec<Pair> {
    values
        .take(50)
        .map(|value| Pair {
            display: value.clone(),
            replacement: value.clone(),
        })
        .collect()
}

#[cfg(test)]
fn collect_vulcan_command_paths() -> Vec<String> {
    fn visit(command: &clap::Command, prefix: &mut Vec<String>, output: &mut Vec<String>) {
        for subcommand in command
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
        {
            prefix.push(subcommand.get_name().to_string());
            output.push(prefix.join(" "));
            visit(subcommand, prefix, output);
            prefix.pop();
        }
    }

    let root = Cli::command().bin_name("vulcan");
    let mut output = Vec::new();
    visit(&root, &mut Vec::new(), &mut output);
    output.sort();
    output.dedup();
    output
}

#[cfg(test)]
fn collect_chat_vault_paths(vault_root: &Path) -> Vec<String> {
    fn visit(root: &Path, dir: &Path, output: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            if relative.starts_with(".git/")
                || relative == ".git"
                || relative.starts_with(".vulcan/")
                || relative == ".vulcan"
            {
                continue;
            }
            if path.is_dir() {
                output.push(format!("{relative}/"));
                visit(root, &path, output);
            } else if path.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("canvas")
            }) {
                output.push(relative);
            }
        }
    }

    let mut output = Vec::new();
    visit(vault_root, vault_root, &mut output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};
    use tempfile::TempDir;

    #[test]
    fn slash_help_and_quit_are_local() {
        let reader = BufReader::new(Cursor::new(Vec::<u8>::new()));
        let writer = Vec::new();
        let mut client = crate::assistant::rpc::ManagedRpcClient::new(reader, writer);

        assert!(
            !handle_slash_command(&mut client, "/help", OutputFormat::Human)
                .expect("help should work")
                .should_quit
        );
        assert!(
            handle_slash_command(&mut client, "/quit", OutputFormat::Human)
                .expect("quit should work")
                .should_quit
        );
    }

    #[test]
    fn chat_completion_suggests_slash_vulcan_and_vault_paths() {
        let temp = TempDir::new().expect("temp dir should exist");
        fs::create_dir_all(temp.path().join("Projects")).expect("folder should write");
        fs::create_dir_all(temp.path().join(".vulcan")).expect("internal folder should write");
        fs::write(temp.path().join("Projects/Alpha.md"), "# Alpha").expect("note should write");
        fs::write(temp.path().join(".vulcan/internal.md"), "# Hidden")
            .expect("hidden should write");
        let helper = AssistantChatHelper::new(temp.path());

        let (_, slash) = helper.complete_line("/th", 3);
        assert!(slash.iter().any(|pair| pair.replacement == "/thinking"));

        let (_, commands) = helper.complete_line("/vulcan note g", 14);
        assert!(commands.iter().any(|pair| pair.replacement == "note get"));

        let (start, paths) = helper.complete_line("read @Projects/A", 16);
        assert_eq!(start, "read @".len());
        assert!(paths
            .iter()
            .any(|pair| pair.replacement == "Projects/Alpha.md"));
        assert!(!paths
            .iter()
            .any(|pair| pair.replacement.contains(".vulcan")));
    }
}
