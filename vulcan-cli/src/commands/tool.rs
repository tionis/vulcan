use crate::output::print_json;
use crate::{
    commands, custom_tool_registry_options, tools, Cli, CliError, OutputFormat, ToolCommand,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use vulcan_core::{load_vault_config, VaultPaths};

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ToolListReport {
    tools: Vec<tools::CustomToolDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ToolTestReport {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    checked: usize,
    updated: usize,
    passed: bool,
    examples: Vec<ToolTestExampleReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ToolTestSuiteReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    checked: usize,
    updated: usize,
    passed: bool,
    tools: Vec<ToolTestReport>,
}

type ToolLintReport = tools::CustomToolLintReport;
type ToolCompatReport = tools::CustomToolCompatReport;
type ToolTypesReport = tools::CustomToolTypesReport;
type ToolTypesSuiteReport = tools::CustomToolTypesSuiteReport;

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ToolCiReport {
    passed: bool,
    profile: Option<String>,
    lint: ToolLintReport,
    tests: ToolTestSuiteReport,
    compatibility: Vec<ToolCompatReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ToolTestExampleReport {
    name: String,
    input: Value,
    passed: bool,
    updated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn handle_tool_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &ToolCommand,
) -> Result<(), CliError> {
    let registry_options = custom_tool_registry_options();
    match command {
        ToolCommand::List => print_tool_list_report(
            cli.output,
            &ToolListReport {
                tools: tools::list_custom_tools(
                    paths,
                    cli.permissions.as_deref(),
                    &registry_options,
                )?,
            },
        ),
        ToolCommand::Show { name } => {
            let report = tools::show_custom_tool(
                paths,
                cli.permissions.as_deref(),
                name,
                &registry_options,
            )?;
            print_tool_show_report(cli.output, &report)
        }
        ToolCommand::Help { name } => {
            let report = tools::show_custom_tool(
                paths,
                cli.permissions.as_deref(),
                name,
                &registry_options,
            )?;
            print_tool_help_report(cli.output, &report)
        }
        ToolCommand::Init {
            name,
            description,
            command,
            template,
            dry_run,
            overwrite,
        } => {
            let report = commands::tool_init::init_skill_backed_tool(
                paths,
                &commands::tool_init::ToolInitCliOptions {
                    name,
                    description: description.as_deref(),
                    command,
                    template: *template,
                    dry_run: *dry_run,
                    overwrite: *overwrite,
                },
                &registry_options,
            )?;
            commands::tool_init::print_tool_init_report(cli.output, &report)
        }
        ToolCommand::Lint { name, strict, fix } => {
            let report = tools::lint_custom_tools(
                paths,
                cli.permissions.as_deref(),
                name.as_deref(),
                *strict,
                *fix,
                &registry_options,
            )
            .map_err(CliError::operation)?;
            let valid = report.valid;
            print_tool_lint_report(cli.output, &report)?;
            if valid {
                Ok(())
            } else {
                Err(CliError::operation("custom tool lint failed"))
            }
        }
        ToolCommand::Test {
            name,
            all,
            example,
            update_expected,
            profile,
        } => {
            let report = if *all {
                run_all_tool_examples(
                    cli,
                    paths,
                    example.as_deref(),
                    profile.as_deref(),
                    *update_expected,
                    &registry_options,
                )?
            } else {
                let name = name.as_deref().ok_or_else(|| {
                    CliError::operation("tool test requires a tool name unless --all is set")
                })?;
                ToolTestOutput::One(run_tool_examples(
                    cli,
                    paths,
                    name,
                    example.as_deref(),
                    profile.as_deref(),
                    *update_expected,
                    &registry_options,
                )?)
            };
            let passed = report.passed();
            print_tool_test_output(cli.output, &report)?;
            if passed {
                Ok(())
            } else {
                Err(CliError::operation("one or more tool examples failed"))
            }
        }
        ToolCommand::Compat { name, surface } => {
            let report = tools::build_tool_compat_report(
                paths,
                cli.permissions.as_deref(),
                name,
                surface,
                &registry_options,
            )
            .map_err(CliError::operation)?;
            print_tool_compat_report(cli.output, &report)
        }
        ToolCommand::Types { name, all } => {
            if *all {
                let report = tools::build_all_tool_types_report(
                    paths,
                    cli.permissions.as_deref(),
                    &registry_options,
                )
                .map_err(CliError::operation)?;
                print_tool_types_output(cli.output, &ToolTypesOutput::All(report))
            } else {
                let name = name.as_deref().ok_or_else(|| {
                    CliError::operation("tool types requires a tool name unless --all is set")
                })?;
                let report = tools::build_tool_types_report(
                    paths,
                    cli.permissions.as_deref(),
                    name,
                    &registry_options,
                )
                .map_err(CliError::operation)?;
                print_tool_types_output(cli.output, &ToolTypesOutput::One(report))
            }
        }
        ToolCommand::Ci { profile, surface } => {
            let report =
                build_tool_ci_report(cli, paths, profile.as_deref(), surface, &registry_options)?;
            let passed = report.passed;
            print_tool_ci_report(cli.output, &report)?;
            if passed {
                Ok(())
            } else {
                Err(CliError::operation("custom tool CI checks failed"))
            }
        }
        ToolCommand::Run {
            name,
            input_json,
            input_file,
            args,
        } => {
            if (input_json.is_some() || input_file.is_some()) && !args.is_empty() {
                return Err(CliError::operation(
                    "tool run accepts either --input-json/--input-file or custom CLI arguments, not both",
                ));
            }
            let (name, input) = if args.is_empty() {
                (
                    tools::resolve_custom_tool_cli_name(paths, name, &registry_options)?,
                    read_tool_input(input_json.as_deref(), input_file.as_deref())?,
                )
            } else {
                tools::build_custom_tool_cli_input(paths, name, args, &registry_options)?
            };
            let report = tools::run_custom_tool(
                paths,
                cli.permissions.as_deref(),
                &name,
                &input,
                &registry_options,
                &tools::CustomToolRunOptions::default(),
            )?;
            print_tool_run_report(cli.output, &report)
        }
    }
}

fn read_tool_input(input_json: Option<&str>, input_file: Option<&Path>) -> Result<Value, CliError> {
    match (input_json, input_file) {
        (None, None) => Ok(json!({})),
        (Some(input_json), None) => serde_json::from_str(input_json).map_err(CliError::operation),
        (None, Some(input_file)) => {
            let source = fs::read_to_string(input_file).map_err(CliError::operation)?;
            serde_json::from_str(&source).map_err(CliError::operation)
        }
        (Some(_), Some(_)) => Err(CliError::operation(
            "tool input accepts either --input-json or --input-file, not both",
        )),
    }
}

fn build_tool_ci_report(
    cli: &Cli,
    paths: &VaultPaths,
    profile: Option<&str>,
    requested_surfaces: &[String],
    registry_options: &tools::CustomToolRegistryOptions,
) -> Result<ToolCiReport, CliError> {
    let active_profile = profile.or(cli.permissions.as_deref());
    let lint = tools::lint_custom_tools(paths, active_profile, None, true, false, registry_options)
        .map_err(CliError::operation)?;
    let tests =
        match run_all_tool_examples(cli, paths, None, active_profile, false, registry_options)? {
            ToolTestOutput::All(report) => report,
            ToolTestOutput::One(_) => {
                return Err(CliError::operation(
                    "internal error: tool CI expected an all-tools test report",
                ))
            }
        };
    let compatibility = tools::list_custom_tools(paths, active_profile, registry_options)?
        .iter()
        .map(|tool| {
            tools::build_tool_compat_report(
                paths,
                active_profile,
                &tool.summary.name,
                requested_surfaces,
                registry_options,
            )
            .map_err(CliError::operation)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let compat_passed = compatibility
        .iter()
        .all(|report| report.surfaces.iter().all(|surface| surface.compatible));
    Ok(ToolCiReport {
        passed: lint.valid && tests.passed && compat_passed,
        profile: active_profile.map(ToOwned::to_owned),
        lint,
        tests,
        compatibility,
    })
}

fn run_tool_examples(
    cli: &Cli,
    paths: &VaultPaths,
    name: &str,
    example_filter: Option<&str>,
    profile: Option<&str>,
    update_expected: bool,
    registry_options: &tools::CustomToolRegistryOptions,
) -> Result<ToolTestReport, CliError> {
    let active_profile = profile.or(cli.permissions.as_deref());
    let show = tools::show_custom_tool(paths, active_profile, name, registry_options)?;
    let example_base_dir = tool_example_base_dir(paths, &show.tool.summary)?;
    let examples = show
        .tool
        .summary
        .examples
        .iter()
        .filter(|example| example_filter.is_none_or(|filter| example.name == filter))
        .map(|example| {
            run_tool_example(
                &ToolExampleRunContext {
                    paths,
                    requested_name: name,
                    resolved_name: &show.tool.summary.name,
                    example_base_dir: &example_base_dir,
                    active_profile,
                    update_expected,
                    registry_options,
                },
                example,
            )
        })
        .collect::<Vec<_>>();
    if examples.is_empty() {
        let message = if let Some(example) = example_filter {
            format!("tool `{name}` has no example named `{example}`")
        } else {
            format!("tool `{name}` declares no examples")
        };
        return Err(CliError::operation(message));
    }
    Ok(ToolTestReport {
        name: show.tool.summary.name,
        profile: active_profile.map(ToOwned::to_owned),
        checked: examples.len(),
        updated: examples.iter().filter(|example| example.updated).count(),
        passed: examples.iter().all(|example| example.passed),
        examples,
    })
}

enum ToolTestOutput {
    One(ToolTestReport),
    All(ToolTestSuiteReport),
}

impl ToolTestOutput {
    fn passed(&self) -> bool {
        match self {
            Self::One(report) => report.passed,
            Self::All(report) => report.passed,
        }
    }
}

fn run_all_tool_examples(
    cli: &Cli,
    paths: &VaultPaths,
    example_filter: Option<&str>,
    profile: Option<&str>,
    update_expected: bool,
    registry_options: &tools::CustomToolRegistryOptions,
) -> Result<ToolTestOutput, CliError> {
    let active_profile = profile.or(cli.permissions.as_deref());
    let tools = tools::list_custom_tools(paths, active_profile, registry_options)?;
    let reports = tools
        .iter()
        .filter(|tool| !tool.summary.examples.is_empty())
        .map(|tool| {
            run_tool_examples(
                cli,
                paths,
                &tool.summary.name,
                example_filter,
                profile,
                update_expected,
                registry_options,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    if reports.is_empty() {
        return Err(CliError::operation(
            "no exposed tools declare runnable examples",
        ));
    }
    Ok(ToolTestOutput::All(ToolTestSuiteReport {
        profile: active_profile.map(ToOwned::to_owned),
        checked: reports.iter().map(|report| report.checked).sum(),
        updated: reports.iter().map(|report| report.updated).sum(),
        passed: reports.iter().all(|report| report.passed),
        tools: reports,
    }))
}

struct ToolExampleRunContext<'a> {
    paths: &'a VaultPaths,
    requested_name: &'a str,
    resolved_name: &'a str,
    example_base_dir: &'a Path,
    active_profile: Option<&'a str>,
    update_expected: bool,
    registry_options: &'a tools::CustomToolRegistryOptions,
}

fn run_tool_example(
    context: &ToolExampleRunContext<'_>,
    example: &vulcan_core::AssistantSkillCommandExample,
) -> ToolTestExampleReport {
    let input = match tool_example_input(
        context.paths,
        context.requested_name,
        example,
        context.example_base_dir,
        context.registry_options,
    ) {
        Ok(input) => input,
        Err(error) => {
            return ToolTestExampleReport {
                name: example.name.clone(),
                input: json!({}),
                passed: false,
                updated: false,
                output: None,
                expected_output: None,
                diff: None,
                error: Some(error.to_string()),
            }
        }
    };
    let expected_output = match tool_example_expected_output(example, context.example_base_dir) {
        Ok(expected_output) => expected_output,
        Err(error) => {
            return ToolTestExampleReport {
                name: example.name.clone(),
                input,
                passed: false,
                updated: false,
                output: None,
                expected_output: None,
                diff: None,
                error: Some(error.to_string()),
            }
        }
    };
    match tools::run_custom_tool(
        context.paths,
        context.active_profile,
        context.resolved_name,
        &input,
        context.registry_options,
        &tools::CustomToolRunOptions {
            surface: "cli.tool.test".to_string(),
        },
    ) {
        Ok(report) => {
            let diff = expected_output.as_ref().and_then(|expected| {
                let diff = json_value_diff("$", expected, &report.result);
                (!diff.is_empty()).then_some(diff)
            });
            let update_result = if context.update_expected {
                Some(tool_example_update_expected_output(
                    example,
                    context.example_base_dir,
                    &report.result,
                ))
            } else {
                None
            };
            let updated = update_result
                .as_ref()
                .is_some_and(std::result::Result::is_ok);
            let update_error = if context.update_expected && !updated {
                update_result
                    .and_then(std::result::Result::err)
                    .map(|error| error.to_string())
            } else {
                None
            };
            ToolTestExampleReport {
                name: example.name.clone(),
                input,
                passed: updated || diff.is_none(),
                updated,
                output: Some(report.result),
                expected_output,
                diff: (!updated).then_some(diff).flatten(),
                error: update_error,
            }
        }
        Err(error) => ToolTestExampleReport {
            name: example.name.clone(),
            input,
            passed: false,
            updated: false,
            output: None,
            expected_output,
            diff: None,
            error: Some(error.to_string()),
        },
    }
}

fn tool_example_input(
    paths: &VaultPaths,
    requested_name: &str,
    example: &vulcan_core::AssistantSkillCommandExample,
    example_base_dir: &Path,
    registry_options: &tools::CustomToolRegistryOptions,
) -> Result<Value, CliError> {
    if let Some(input) = &example.input {
        Ok(input.clone())
    } else if let Some(input_file) = &example.input_file {
        read_skill_example_json_file(example_base_dir, input_file)
    } else {
        tools::build_custom_tool_cli_input(
            paths,
            requested_name,
            &example.cli_args,
            registry_options,
        )
        .map(|(_, input)| input)
    }
}

fn tool_example_expected_output(
    example: &vulcan_core::AssistantSkillCommandExample,
    example_base_dir: &Path,
) -> Result<Option<Value>, CliError> {
    if let Some(expected_output) = &example.expected_output {
        Ok(Some(expected_output.clone()))
    } else if let Some(expected_output_file) = &example.expected_output_file {
        read_skill_example_json_file(example_base_dir, expected_output_file).map(Some)
    } else {
        Ok(None)
    }
}

fn tool_example_update_expected_output(
    example: &vulcan_core::AssistantSkillCommandExample,
    example_base_dir: &Path,
    output: &Value,
) -> Result<(), CliError> {
    let Some(expected_output_file) = &example.expected_output_file else {
        return Err(CliError::operation(format!(
            "example `{}` cannot be updated because it does not use expected_output_file",
            example.name
        )));
    };
    let relative_path = safe_relative_example_path(expected_output_file)?;
    let path = example_base_dir.join(relative_path);
    let formatted = serde_json::to_string_pretty(output).map_err(CliError::operation)?;
    fs::write(&path, format!("{formatted}\n")).map_err(|error| {
        CliError::operation(format!("failed to write {}: {error}", path.display()))
    })
}

fn tool_example_base_dir(
    paths: &VaultPaths,
    tool: &vulcan_core::AssistantToolSummary,
) -> Result<PathBuf, CliError> {
    let manifest_path = paths.vault_root().join(&tool.path);
    if manifest_path.exists() {
        return manifest_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| CliError::operation("tool manifest has no parent directory"));
    }
    let config = load_vault_config(paths).config;
    let manifest_path = paths
        .vault_root()
        .join(config.assistant.skills_folder)
        .join(&tool.path);
    manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| CliError::operation("tool manifest has no parent directory"))
}

fn read_skill_example_json_file(base_dir: &Path, relative_path: &str) -> Result<Value, CliError> {
    let relative_path = safe_relative_example_path(relative_path)?;
    let path = base_dir.join(relative_path);
    let source = fs::read_to_string(&path).map_err(|error| {
        CliError::operation(format!("failed to read {}: {error}", path.display()))
    })?;
    serde_json::from_str(&source).map_err(|error| {
        CliError::operation(format!(
            "failed to parse {} as JSON: {error}",
            path.display()
        ))
    })
}

fn safe_relative_example_path(value: &str) -> Result<PathBuf, CliError> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(CliError::operation(format!(
            "example file path `{value}` must be relative"
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(CliError::operation(format!(
            "example file path `{value}` must stay inside the skill directory"
        )));
    }
    Ok(path.to_path_buf())
}

fn json_value_diff(path: &str, expected: &Value, actual: &Value) -> Vec<String> {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            let keys = expected
                .keys()
                .chain(actual.keys())
                .collect::<BTreeSet<_>>();
            keys.into_iter()
                .flat_map(|key| {
                    let child_path = format!("{path}.{key}");
                    match (expected.get(key), actual.get(key)) {
                        (Some(expected), Some(actual)) => {
                            json_value_diff(&child_path, expected, actual)
                        }
                        (Some(expected), None) => vec![format!(
                            "{child_path}: missing actual value, expected {}",
                            compact_json(expected)
                        )],
                        (None, Some(actual)) => vec![format!(
                            "{child_path}: unexpected actual value {}",
                            compact_json(actual)
                        )],
                        (None, None) => Vec::new(),
                    }
                })
                .collect()
        }
        (Value::Array(expected), Value::Array(actual)) => {
            let max_len = expected.len().max(actual.len());
            (0..max_len)
                .flat_map(|index| {
                    let child_path = format!("{path}[{index}]");
                    match (expected.get(index), actual.get(index)) {
                        (Some(expected), Some(actual)) => {
                            json_value_diff(&child_path, expected, actual)
                        }
                        (Some(expected), None) => vec![format!(
                            "{child_path}: missing actual value, expected {}",
                            compact_json(expected)
                        )],
                        (None, Some(actual)) => vec![format!(
                            "{child_path}: unexpected actual value {}",
                            compact_json(actual)
                        )],
                        (None, None) => Vec::new(),
                    }
                })
                .collect()
        }
        _ if expected == actual => Vec::new(),
        _ => vec![format!(
            "{path}: expected {}, got {}",
            compact_json(expected),
            compact_json(actual)
        )],
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_string())
}

fn print_tool_list_report(output: OutputFormat, report: &ToolListReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.tools.is_empty() {
                println!("No exposed skill command tools.");
                return Ok(());
            }
            for tool in &report.tools {
                let callable = if tool.callable {
                    "callable"
                } else {
                    "not callable"
                };
                println!(
                    "- {} [{}; sandbox={}; packs={}] {}",
                    tool.summary.name,
                    callable,
                    tools::tool_sandbox(tool.summary.sandbox),
                    if tool.summary.packs.is_empty() {
                        "custom".to_string()
                    } else {
                        tool.summary.packs.join(", ")
                    },
                    tool.summary.path
                );
                println!("  {}", tool.summary.description);
            }
            Ok(())
        }
    }
}

fn print_tool_lint_report(output: OutputFormat, report: &ToolLintReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Custom tool lint: {} ({} checked, {} fixed)",
                if report.valid { "passed" } else { "failed" },
                report.checked,
                report.fixed
            );
            for tool in &report.tools {
                for fix in &tool.fixes {
                    println!("fix: {}: {fix}", tool.name);
                }
            }
            for warning in &report.warnings {
                println!("warning: {warning}");
            }
            for error in &report.errors {
                println!("error: {error}");
            }
            Ok(())
        }
    }
}

fn print_tool_compat_report(
    output: OutputFormat,
    report: &ToolCompatReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Custom tool compatibility: {}", report.name);
            for surface in &report.surfaces {
                println!(
                    "{}: {}",
                    surface.surface,
                    if surface.compatible {
                        "compatible"
                    } else {
                        "not compatible"
                    }
                );
                for warning in &surface.warnings {
                    println!("  warning: {warning}");
                }
                for error in &surface.errors {
                    println!("  error: {error}");
                }
            }
            Ok(())
        }
    }
}

enum ToolTypesOutput {
    One(ToolTypesReport),
    All(ToolTypesSuiteReport),
}

fn print_tool_types_output(output: OutputFormat, report: &ToolTypesOutput) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            match report {
                ToolTypesOutput::One(report) => print!("{}", report.source),
                ToolTypesOutput::All(report) => print!("{}", report.source),
            }
            Ok(())
        }
    }
}

impl Serialize for ToolTypesOutput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::One(report) => report.serialize(serializer),
            Self::All(report) => report.serialize(serializer),
        }
    }
}

fn print_tool_ci_report(output: OutputFormat, report: &ToolCiReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Custom tool CI: {}",
                if report.passed { "passed" } else { "failed" }
            );
            println!(
                "lint: {} ({} checked, {} warnings, {} errors)",
                if report.lint.valid {
                    "passed"
                } else {
                    "failed"
                },
                report.lint.checked,
                report.lint.warnings.len(),
                report.lint.errors.len()
            );
            println!(
                "examples: {} ({} checked across {} tools)",
                if report.tests.passed {
                    "passed"
                } else {
                    "failed"
                },
                report.tests.checked,
                report.tests.tools.len()
            );
            let compatible_surfaces = report
                .compatibility
                .iter()
                .flat_map(|tool| &tool.surfaces)
                .filter(|surface| surface.compatible)
                .count();
            let total_surfaces: usize = report
                .compatibility
                .iter()
                .map(|tool| tool.surfaces.len())
                .sum();
            println!("compatibility: {compatible_surfaces}/{total_surfaces} surfaces compatible");
            for warning in &report.lint.warnings {
                println!("warning: {warning}");
            }
            for error in &report.lint.errors {
                println!("error: {error}");
            }
            for tool in &report.tests.tools {
                for example in &tool.examples {
                    if !example.passed {
                        println!("error: {} example `{}` failed", tool.name, example.name);
                    }
                }
            }
            for tool in &report.compatibility {
                for surface in &tool.surfaces {
                    for warning in &surface.warnings {
                        println!("warning: {} {}: {warning}", tool.name, surface.surface);
                    }
                    for error in &surface.errors {
                        println!("error: {} {}: {error}", tool.name, surface.surface);
                    }
                }
            }
            Ok(())
        }
    }
}

fn print_tool_show_report(
    output: OutputFormat,
    report: &tools::CustomToolShowReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{}",
                report
                    .tool
                    .summary
                    .title
                    .as_deref()
                    .unwrap_or(&report.tool.summary.name)
            );
            println!("name: {}", report.tool.summary.name);
            println!("description: {}", report.tool.summary.description);
            println!("manifest: {}", report.tool.summary.path);
            println!("entrypoint: {}", report.tool.summary.entrypoint_path);
            println!(
                "sandbox: {}",
                tools::tool_sandbox(report.tool.summary.sandbox)
            );
            println!(
                "callable: {}",
                if report.callable {
                    "yes"
                } else {
                    "no (vault not trusted)"
                }
            );
            if let Some(permission_profile) = &report.tool.summary.permission_profile {
                println!("permission profile: {permission_profile}");
            }
            if !report.tool.summary.packs.is_empty() {
                println!("packs: {}", report.tool.summary.packs.join(", "));
            }
            if let Some(cli) = &report.tool.summary.cli {
                if !cli.aliases.is_empty() {
                    println!("aliases: {}", cli.aliases.join(", "));
                }
                if !cli.args.is_empty() {
                    println!("cli flags:");
                    for arg in &cli.args {
                        let flag = format!("--{}", arg.flag.trim_start_matches('-'));
                        let value_hint = tool_cli_arg_value_hint(arg);
                        let usage = if value_hint.is_empty() {
                            flag
                        } else {
                            format!("{flag} <{value_hint}>")
                        };
                        if let Some(description) = &arg.description {
                            println!("  {usage} - {description}");
                        } else {
                            println!("  {usage}");
                        }
                    }
                }
            }
            if !report.tool.summary.secrets.is_empty() {
                println!(
                    "secrets: {}",
                    report
                        .tool
                        .summary
                        .secrets
                        .iter()
                        .map(|secret| format!("{}={}", secret.name, secret.env))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !report.tool.body.is_empty() {
                println!("\n{}", report.tool.body);
            }
            Ok(())
        }
    }
}

fn print_tool_help_report(
    output: OutputFormat,
    report: &tools::CustomToolShowReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let name = report
                .tool
                .summary
                .cli
                .as_ref()
                .and_then(|cli| cli.aliases.first())
                .unwrap_or(&report.tool.summary.name);
            println!("{}", report.tool.summary.description);
            println!();
            println!("Usage:");
            if let Some(cli) = &report.tool.summary.cli {
                if cli.args.is_empty() {
                    println!("  vulcan tool run {name}");
                } else {
                    let flags = cli
                        .args
                        .iter()
                        .map(|arg| {
                            let flag = format!("--{}", arg.flag.trim_start_matches('-'));
                            let value_hint = tool_cli_arg_value_hint(arg);
                            if value_hint.is_empty() {
                                flag
                            } else {
                                format!("{flag} <{value_hint}>")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    println!("  vulcan tool run {name} {flags}");
                }
                if !cli.aliases.is_empty() {
                    println!();
                    println!("Aliases: {}", cli.aliases.join(", "));
                }
                if !cli.args.is_empty() {
                    println!();
                    println!("Flags:");
                    for arg in &cli.args {
                        let flag = format!("--{}", arg.flag.trim_start_matches('-'));
                        let value_hint = tool_cli_arg_value_hint(arg);
                        let usage = if value_hint.is_empty() {
                            flag
                        } else {
                            format!("{flag} <{value_hint}>")
                        };
                        let mut details = Vec::new();
                        if !arg.choices.is_empty() {
                            details.push(format!("choices: {}", arg.choices.join(", ")));
                        }
                        if let Some(completion) = &arg.completion {
                            details.push(format!("completion: {completion}"));
                        }
                        let suffix = if details.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", details.join("; "))
                        };
                        if let Some(description) = &arg.description {
                            println!("  {usage} - {description}{suffix}");
                        } else {
                            println!("  {usage}{suffix}");
                        }
                    }
                }
                if !report.tool.summary.examples.is_empty() {
                    println!();
                    println!("Examples:");
                    for example in &report.tool.summary.examples {
                        if !example.cli_args.is_empty() {
                            println!(
                                "  {}: vulcan tool run {name} {}",
                                example.name,
                                example.cli_args.join(" ")
                            );
                        } else if let Some(input) = &example.input {
                            println!(
                                "  {}: vulcan tool run {} --input-json '{}'",
                                example.name,
                                report.tool.summary.name,
                                serde_json::to_string(input).map_err(CliError::operation)?
                            );
                        }
                        if let Some(description) = &example.description {
                            println!("    {description}");
                        }
                    }
                }
            } else {
                println!(
                    "  vulcan tool run {} --input-json '<json>'",
                    report.tool.summary.name
                );
            }
            Ok(())
        }
    }
}

fn print_tool_test_output(output: OutputFormat, report: &ToolTestOutput) -> Result<(), CliError> {
    match report {
        ToolTestOutput::One(report) => print_tool_test_report(output, report),
        ToolTestOutput::All(report) => print_tool_test_suite_report(output, report),
    }
}

fn print_tool_test_suite_report(
    output: OutputFormat,
    report: &ToolTestSuiteReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Tool examples: {} ({} checked across {} tools, {} updated)",
                if report.passed { "passed" } else { "failed" },
                report.checked,
                report.tools.len(),
                report.updated
            );
            if let Some(profile) = &report.profile {
                println!("profile: {profile}");
            }
            for tool in &report.tools {
                println!(
                    "- {}: {} ({} checked, {} updated)",
                    tool.name,
                    if tool.passed { "passed" } else { "failed" },
                    tool.checked,
                    tool.updated
                );
                for example in &tool.examples {
                    print_tool_test_example_report(example);
                }
            }
            Ok(())
        }
    }
}

fn print_tool_test_report(output: OutputFormat, report: &ToolTestReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Tool examples: {} ({} checked, {} updated)",
                if report.passed { "passed" } else { "failed" },
                report.checked,
                report.updated
            );
            if let Some(profile) = &report.profile {
                println!("profile: {profile}");
            }
            for example in &report.examples {
                print_tool_test_example_report(example);
            }
            Ok(())
        }
    }
}

fn print_tool_test_example_report(example: &ToolTestExampleReport) {
    let status = if example.updated {
        "updated"
    } else if example.passed {
        "passed"
    } else {
        "failed"
    };
    println!("- {}: {status}", example.name);
    if let Some(error) = &example.error {
        println!("  error: {error}");
    }
    if let Some(diff) = &example.diff {
        for line in diff {
            println!("  diff: {line}");
        }
    }
}

fn tool_cli_arg_value_hint(arg: &vulcan_core::AssistantSkillCommandCliArg) -> &'static str {
    match arg.action {
        vulcan_core::AssistantSkillCommandCliArgAction::Boolean => "",
        vulcan_core::AssistantSkillCommandCliArgAction::Integer => "integer",
        vulcan_core::AssistantSkillCommandCliArgAction::Number => "number",
        vulcan_core::AssistantSkillCommandCliArgAction::Json
        | vulcan_core::AssistantSkillCommandCliArgAction::JsonArray => "json",
        vulcan_core::AssistantSkillCommandCliArgAction::StringFile
        | vulcan_core::AssistantSkillCommandCliArgAction::JsonFile => "path|-",
        vulcan_core::AssistantSkillCommandCliArgAction::Choice => "choice",
        vulcan_core::AssistantSkillCommandCliArgAction::String
        | vulcan_core::AssistantSkillCommandCliArgAction::StringArray
        | vulcan_core::AssistantSkillCommandCliArgAction::AppendMessage => "text",
    }
}

fn print_tool_run_report(
    output: OutputFormat,
    report: &tools::CustomToolRunReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if let Some(text) = &report.text {
                println!("{text}");
            } else {
                println!("Ran tool {}", report.name);
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&report.result).map_err(CliError::operation)?
            );
            Ok(())
        }
    }
}
