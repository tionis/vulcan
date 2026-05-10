use crate::commit::AutoCommitPolicy;
use crate::editor::open_in_editor;
use crate::output::print_json;
use crate::resolve::resolve_note_argument;
use crate::{
    print_markdown_output, run_incremental_scan, warn_auto_commit_if_needed, CliError,
    OutputFormat, TemplateEngineArg,
};
use serde::Serialize;
use std::io::{self, IsTerminal};
use vulcan_app::templates::{
    apply_template_create, apply_template_insert, build_template_list_report,
    build_template_preview_report, build_template_show_report, parse_template_var_bindings,
    TemplateCreateRequest, TemplateEngineKind, TemplateInsertMode, TemplateInsertRequest,
    TemplatePreviewRequest,
};
use vulcan_core::VaultPaths;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TemplateListReport {
    pub(crate) templates: Vec<TemplateSummary>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TemplateCreateReport {
    pub(crate) template: String,
    pub(crate) template_source: String,
    pub(crate) path: String,
    pub(crate) engine: String,
    pub(crate) opened_editor: bool,
    pub(crate) warnings: Vec<String>,
    pub(crate) diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TemplateInsertReport {
    pub(crate) template: String,
    pub(crate) template_source: String,
    pub(crate) note: String,
    pub(crate) mode: String,
    pub(crate) engine: String,
    pub(crate) warnings: Vec<String>,
    pub(crate) diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TemplatePreviewReport {
    pub(crate) template: String,
    pub(crate) template_source: String,
    pub(crate) path: String,
    pub(crate) engine: String,
    pub(crate) content: String,
    pub(crate) warnings: Vec<String>,
    pub(crate) diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TemplateSummary {
    pub(crate) name: String,
    pub(crate) source: String,
    pub(crate) path: String,
}

pub(crate) enum TemplateCommandResult {
    List(TemplateListReport),
    Create(TemplateCreateReport),
    Insert(TemplateInsertReport),
    Preview(TemplatePreviewReport),
}

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub(crate) fn run_template_command(
    paths: &VaultPaths,
    name: Option<&str>,
    list: bool,
    output_path: Option<&str>,
    engine: TemplateEngineArg,
    vars: &[String],
    no_commit: bool,
    quiet: bool,
    stdout_is_tty: bool,
) -> Result<TemplateCommandResult, CliError> {
    if list {
        let report = build_template_list_report(paths)?;
        return Ok(TemplateCommandResult::List(TemplateListReport {
            templates: report
                .templates
                .into_iter()
                .map(|template| TemplateSummary {
                    name: template.name,
                    source: template.source,
                    path: template.path,
                })
                .collect(),
            warnings: report.warnings,
        }));
    }

    let template_name = name.ok_or_else(|| {
        CliError::operation("`template` requires a template name unless --list is used")
    })?;
    let created = apply_template_create(
        paths,
        &TemplateCreateRequest {
            template: template_name.to_string(),
            output_path: output_path.map(ToOwned::to_owned),
            engine: template_engine_kind(engine),
            vars: parse_template_var_bindings(vars)?,
        },
    )?;

    let mut opened_editor = false;
    if stdout_is_tty && io::stdin().is_terminal() {
        open_in_editor(&created.absolute_path).map_err(CliError::operation)?;
        opened_editor = true;
    }

    run_incremental_scan(paths, OutputFormat::Human, false, false)?;
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, quiet);
    auto_commit
        .commit(paths, "template", &created.changed_paths, None, quiet)
        .map_err(CliError::operation)?;

    Ok(TemplateCommandResult::Create(TemplateCreateReport {
        template: created.template,
        template_source: created.template_source,
        path: created.path,
        engine: created.engine,
        opened_editor,
        warnings: created.warnings,
        diagnostics: created.diagnostics,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_template_insert_command(
    paths: &VaultPaths,
    template_name: &str,
    note: Option<&str>,
    mode: TemplateInsertMode,
    engine: TemplateEngineArg,
    vars: &[String],
    no_commit: bool,
    quiet: bool,
    interactive_note_selection: bool,
) -> Result<TemplateInsertReport, CliError> {
    let target_identifier = resolve_note_argument(
        paths,
        note,
        interactive_note_selection,
        "template insert target note",
    )?;
    let report = apply_template_insert(
        paths,
        &TemplateInsertRequest {
            template: template_name.to_string(),
            note: target_identifier,
            mode,
            engine: template_engine_kind(engine),
            vars: parse_template_var_bindings(vars)?,
        },
    )?;

    run_incremental_scan(paths, OutputFormat::Human, false, false)?;
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, quiet);
    auto_commit
        .commit(paths, "template insert", &report.changed_paths, None, quiet)
        .map_err(CliError::operation)?;

    Ok(TemplateInsertReport {
        template: report.template,
        template_source: report.template_source,
        note: report.note,
        mode: report.mode,
        engine: report.engine,
        warnings: report.warnings,
        diagnostics: report.diagnostics,
    })
}

pub(crate) fn run_template_preview_command(
    paths: &VaultPaths,
    template_name: &str,
    output_path: Option<&str>,
    engine: TemplateEngineArg,
    vars: &[String],
) -> Result<TemplatePreviewReport, CliError> {
    let report = build_template_preview_report(
        paths,
        &TemplatePreviewRequest {
            template: template_name.to_string(),
            output_path: output_path.map(ToOwned::to_owned),
            engine: template_engine_kind(engine),
            vars: parse_template_var_bindings(vars)?,
        },
    )?;
    Ok(TemplatePreviewReport {
        template: report.template,
        template_source: report.template_source,
        path: report.path,
        engine: report.engine,
        content: report.content,
        warnings: report.warnings,
        diagnostics: report.diagnostics,
    })
}

fn template_engine_kind(engine: TemplateEngineArg) -> TemplateEngineKind {
    match engine {
        TemplateEngineArg::Native => TemplateEngineKind::Native,
        TemplateEngineArg::Templater => TemplateEngineKind::Templater,
        TemplateEngineArg::Auto => TemplateEngineKind::Auto,
    }
}

pub(crate) fn print_template_list_report(
    output: OutputFormat,
    report: &TemplateListReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.templates.is_empty() {
                println!("No templates found.");
            } else {
                for template in &report.templates {
                    println!("{} [{}: {}]", template.name, template.source, template.path);
                }
            }
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_template_create_report(
    output: OutputFormat,
    report: &TemplateCreateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Created {} from {} ({}, {})",
                report.path, report.template, report.template_source, report.engine
            );
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            for diagnostic in &report.diagnostics {
                eprintln!("Diagnostic: {diagnostic}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_template_insert_report(
    output: OutputFormat,
    report: &TemplateInsertReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Inserted {} into {} ({}, {}, {})",
                report.template, report.note, report.mode, report.template_source, report.engine
            );
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            for diagnostic in &report.diagnostics {
                eprintln!("Diagnostic: {diagnostic}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_template_preview_report(
    output: OutputFormat,
    report: &TemplatePreviewReport,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            print_markdown_output(output, &report.content, stdout_is_tty, use_color)?;
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            for diagnostic in &report.diagnostics {
                eprintln!("Diagnostic: {diagnostic}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn run_template_show_command(
    paths: &VaultPaths,
    name: &str,
    output: OutputFormat,
) -> Result<(), CliError> {
    let report = build_template_show_report(paths, name)?;
    match output {
        OutputFormat::Json => print_json(&report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Name:   {}", report.name);
            println!("Source: {}", report.source);
            println!("Path:   {}", report.path);
            println!();
            print!("{}", report.content);
            Ok(())
        }
    }
}
