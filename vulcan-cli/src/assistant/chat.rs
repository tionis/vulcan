use crate::assistant::engine;
use crate::assistant::renderer::{AssistantRenderReport, AssistantRenderer, RenderOptions};
use crate::assistant::{export_session_after_run, AssistantHostContext, AssistantHostOptions};
use crate::{CliError, OutputFormat};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use serde_json::{Map, Value};
use std::io::{self, IsTerminal, Read, Write};
use vulcan_core::VaultPaths;

pub(crate) fn run_chat(
    paths: &VaultPaths,
    host: &AssistantHostOptions,
    context: &AssistantHostContext,
    initial_prompt: &[String],
    show_thinking: bool,
    output: OutputFormat,
) -> Result<(), CliError> {
    let mut process = engine::spawn_pi_rpc(host, paths.vault_root())?;
    process.ensure_running()?;
    configure_engine(&mut process.client, context)?;

    if !initial_prompt.is_empty() {
        send_and_render(
            &mut process.client,
            &initial_prompt.join(" "),
            show_thinking,
            output,
        )?;
    }

    if !io::stdin().is_terminal() {
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
        export_session_after_run(paths, host)?;
        return Ok(());
    }

    let mut editor = DefaultEditor::new().map_err(CliError::operation)?;
    loop {
        match editor.readline("vulcan> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = editor.add_history_entry(line);
                if line.starts_with('/') {
                    if handle_slash_command(&mut process.client, line, output)?.should_quit {
                        break;
                    }
                    continue;
                }
                send_and_render(&mut process.client, line, show_thinking, output)?;
            }
            Err(ReadlineError::Interrupted) => {
                let _ = process.client.abort();
                println!("aborted");
            }
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(CliError::operation(error)),
        }
    }
    process.shutdown()?;
    export_session_after_run(paths, host)
}

fn configure_engine<R: std::io::BufRead, W: std::io::Write>(
    client: &mut crate::assistant::rpc::ManagedRpcClient<R, W>,
    context: &AssistantHostContext,
) -> Result<(), CliError> {
    let mut configure = Map::new();
    configure.insert(
        "context".to_string(),
        serde_json::to_value(context).map_err(CliError::operation)?,
    );
    let result = client.command("configure", configure)?;
    ensure_success(&result.response)
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
        "Commands: /model /models /thinking /compact /new /stats /state /steer <text> /follow-up <text> /set-model <provider> <model> /quit"
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
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

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
}
