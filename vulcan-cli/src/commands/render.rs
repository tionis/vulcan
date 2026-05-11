use crate::commands::note::path_buf_to_slash_string;
use crate::output::print_json;
use crate::terminal_markdown;
use crate::{CliError, OutputFormat, RenderMode};
use serde::Serialize;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use vulcan_core::{render_note_html, render_vault_html, HtmlRenderOptions, VaultPaths};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RenderReport {
    path: Option<String>,
    source: String,
    rendered: String,
    mode: String,
}

pub(crate) fn run_render_command(
    paths: &VaultPaths,
    path: Option<&PathBuf>,
    mode: RenderMode,
) -> Result<RenderReport, CliError> {
    let (path, source, source_path) = match path {
        Some(path) if path.as_os_str() == "-" => {
            let mut source = String::new();
            io::stdin()
                .read_to_string(&mut source)
                .map_err(CliError::operation)?;
            (None, source, None)
        }
        Some(path) => {
            let contents = fs::read_to_string(path).map_err(CliError::operation)?;
            let absolute = fs::canonicalize(path).map_err(CliError::operation)?;
            let source_path = paths
                .relative_to_vault(&absolute)
                .map(|relative| path_buf_to_slash_string(&relative));
            (Some(path.display().to_string()), contents, source_path)
        }
        None => {
            if io::stdin().is_terminal() {
                return Err(CliError::operation(
                    "`vulcan render` requires a file path or piped stdin",
                ));
            }
            let mut source = String::new();
            io::stdin()
                .read_to_string(&mut source)
                .map_err(CliError::operation)?;
            (None, source, None)
        }
    };

    let rendered = match mode {
        RenderMode::Terminal => terminal_markdown::render_terminal_markdown(&source, false),
        RenderMode::Html => source_path.as_deref().map_or_else(
            || {
                render_vault_html(
                    paths,
                    &source,
                    &HtmlRenderOptions {
                        full_document: true,
                        ..HtmlRenderOptions::default()
                    },
                )
                .html
            },
            |relative_path| render_note_html(paths, relative_path, &source).html,
        ),
    };
    Ok(RenderReport {
        path,
        source,
        rendered,
        mode: match mode {
            RenderMode::Terminal => "terminal",
            RenderMode::Html => "html",
        }
        .to_string(),
    })
}

pub(crate) fn print_render_report(
    output: OutputFormat,
    report: &RenderReport,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human => {
            let rendered = if report.mode == "terminal" && stdout_is_tty {
                terminal_markdown::render_terminal_markdown(&report.source, use_color)
            } else {
                report.rendered.clone()
            };
            print!("{rendered}");
            if !rendered.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        OutputFormat::Markdown => {
            let rendered = if report.mode == "html" {
                report.rendered.as_str()
            } else {
                report.source.as_str()
            };
            print!("{rendered}");
            if !rendered.ends_with('\n') {
                println!();
            }
            Ok(())
        }
    }
}
