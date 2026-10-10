use crate::commands::note::path_buf_to_slash_string;
use crate::output::print_json;
use crate::terminal_markdown;
use crate::{CliError, OutputFormat, RenderMode};
use serde::Serialize;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use vulcan_core::{
    render_evaluated_markdown, render_vault_html, DataviewJsRenderPolicy, EvaluatedMarkdownOptions,
    HtmlRenderOptions, PermissionGuard, RenderDiagnostic, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RenderReport {
    path: Option<String>,
    source: String,
    /// The source with dynamic content evaluated; equal to `source` when
    /// evaluation is off or the input is outside the vault.
    markdown: String,
    rendered: String,
    mode: String,
    evaluated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diagnostics: Vec<RenderDiagnostic>,
    #[serde(skip)]
    strict_line_breaks: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct RenderOptions<'a> {
    pub(crate) mode: RenderMode,
    /// Evaluate dynamic content of files inside the vault.
    pub(crate) evaluate: bool,
    pub(crate) dataview_js: DataviewJsRenderPolicy,
    pub(crate) guard: Option<&'a dyn PermissionGuard>,
}

pub(crate) fn run_render_command(
    paths: &VaultPaths,
    path: Option<&PathBuf>,
    options: RenderOptions<'_>,
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

    let permission_profile = options.guard.map(PermissionGuard::profile_name);
    let strict_line_breaks = vulcan_core::load_vault_config(paths)
        .config
        .strict_line_breaks;
    // Dynamic content only has meaning for notes inside the vault. HTML
    // output always evaluates, like `note get --mode html`.
    let evaluate = source_path.is_some() && (options.evaluate || options.mode == RenderMode::Html);
    let (markdown, rendered, diagnostics) = match options.mode {
        RenderMode::Terminal if evaluate => {
            let evaluated = render_evaluated_markdown(
                paths,
                &source,
                &EvaluatedMarkdownOptions {
                    source_path: source_path.as_deref(),
                    dataview_js: options.dataview_js,
                    guard: options.guard,
                    permission_profile,
                },
            );
            let rendered =
                terminal_markdown::render_note_markdown(&evaluated.markdown, strict_line_breaks);
            (evaluated.markdown, rendered, evaluated.diagnostics)
        }
        RenderMode::Terminal => {
            let rendered = terminal_markdown::render_note_markdown(&source, strict_line_breaks);
            (source.clone(), rendered, Vec::new())
        }
        RenderMode::Html => {
            let rendered = render_vault_html(
                paths,
                &source,
                &HtmlRenderOptions {
                    source_path: source_path.as_deref(),
                    guard: options.guard,
                    permission_profile,
                    dataview_js_policy: options.dataview_js,
                    ..HtmlRenderOptions::default()
                },
            );
            (source.clone(), rendered.html, rendered.diagnostics)
        }
    };
    Ok(RenderReport {
        path,
        source,
        markdown,
        rendered,
        mode: match options.mode {
            RenderMode::Terminal => "terminal",
            RenderMode::Html => "html",
        }
        .to_string(),
        evaluated: evaluate,
        diagnostics,
        strict_line_breaks,
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
                terminal_markdown::render_note_markdown_for_stdout(
                    &report.markdown,
                    use_color,
                    report.strict_line_breaks,
                )
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
                report.markdown.as_str()
            };
            print!("{rendered}");
            if !rendered.ends_with('\n') {
                println!();
            }
            Ok(())
        }
    }
}
