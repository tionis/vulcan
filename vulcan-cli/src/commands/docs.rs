use crate::help::{
    builtin_help_topic, builtin_help_topics, help_overview, HelpSearchMatch, HelpSearchReport,
    HelpTopicKind, HelpTopicReport,
};
use crate::output::print_json;
use crate::terminal_markdown;
use crate::{
    app_config, mcp, print_markdown_output, tools, Cli, CliError, DescribeFormatArg,
    McpToolPackArg, McpToolPackModeArg, OutputFormat,
};
use clap::CommandFactory;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use vulcan_core::VaultPaths;

pub(crate) fn handle_help_command(
    output: OutputFormat,
    topic: &[String],
    search: Option<&str>,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    print_help_command(output, topic, search, stdout_is_tty, use_color)
}

pub(crate) fn handle_describe_command(
    paths: &VaultPaths,
    output: OutputFormat,
    format: DescribeFormatArg,
    tool_pack: &[McpToolPackArg],
    tool_pack_mode: McpToolPackModeArg,
    requested_profile: Option<&str>,
) -> Result<(), CliError> {
    print_describe_report(
        paths,
        output,
        format,
        tool_pack,
        tool_pack_mode,
        requested_profile,
    )
}

fn render_help_search_markdown(keyword: &str, report: &HelpSearchReport) -> String {
    if report.matches.is_empty() {
        return format!("# Help search\n\nNo help topics matched `{keyword}`.");
    }

    let items = report
        .matches
        .iter()
        .map(|item| format!("- `{}` [{}]: {}", item.name, item.kind, item.summary))
        .collect::<Vec<_>>()
        .join("\n");
    format!("# Help search\n\nTopics matching `{keyword}`:\n\n{items}")
}

fn render_help_topic_markdown(report: &HelpTopicReport) -> String {
    let mut markdown = format!("# {}\n\n{}\n", report.name, report.summary);
    if !report.body.is_empty() {
        markdown.push('\n');
        markdown.push_str(&report.body);
        markdown.push('\n');
    }
    if !report.options.is_empty() {
        markdown.push_str("\n## Options\n\n");
        for option in &report.options {
            let flag = option
                .long
                .as_deref()
                .map_or_else(|| option.id.clone(), |long| format!("--{long}"));
            let summary = option.help.as_deref().unwrap_or("undocumented");
            let _ = writeln!(markdown, "- `{flag}`: {summary}");
        }
    }
    markdown
}

pub(crate) fn print_describe_report(
    paths: &VaultPaths,
    output: OutputFormat,
    format: DescribeFormatArg,
    tool_pack: &[McpToolPackArg],
    tool_pack_mode: McpToolPackModeArg,
    requested_profile: Option<&str>,
) -> Result<(), CliError> {
    match format {
        DescribeFormatArg::JsonSchema => {
            let report = describe_cli();
            match output {
                OutputFormat::Human | OutputFormat::Markdown => {
                    print_describe_human(&report);
                    Ok(())
                }
                OutputFormat::Json => print_json(&report),
            }
        }
        DescribeFormatArg::OpenaiTools => {
            let tools =
                build_openai_tool_definitions(paths, requested_profile, tool_pack, tool_pack_mode)?;
            match output {
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&tools).map_err(CliError::operation)?
                    );
                    Ok(())
                }
                OutputFormat::Json => print_json(&tools),
            }
        }
        DescribeFormatArg::Mcp => {
            let tools = mcp::build_mcp_tool_definitions(
                paths,
                requested_profile,
                tool_pack,
                tool_pack_mode,
            )?;
            match output {
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&tools).map_err(CliError::operation)?
                    );
                    Ok(())
                }
                OutputFormat::Json => print_json(&tools),
            }
        }
    }
}

pub(crate) fn print_help_command(
    output: OutputFormat,
    topic: &[String],
    search: Option<&str>,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    if let Some(keyword) = search {
        let report = search_help_topics(keyword);
        return match output {
            OutputFormat::Human => {
                let markdown = render_help_search_markdown(keyword, &report);
                println!(
                    "{}",
                    terminal_markdown::render_terminal_markdown(&markdown, use_color)
                );
                Ok(())
            }
            OutputFormat::Markdown => print_markdown_output(
                output,
                &render_help_search_markdown(keyword, &report),
                stdout_is_tty,
                use_color,
            ),
            OutputFormat::Json => print_json(&report),
        };
    }

    let report = if topic.is_empty() {
        help_overview()
    } else {
        resolve_help_topic(topic)?
    };

    match output {
        OutputFormat::Human => {
            let markdown = render_help_topic_markdown(&report);
            println!(
                "{}",
                terminal_markdown::render_terminal_markdown(&markdown, use_color)
            );
            Ok(())
        }
        OutputFormat::Markdown => print_markdown_output(
            output,
            &render_help_topic_markdown(&report),
            stdout_is_tty,
            use_color,
        ),
        OutputFormat::Json => print_json(&report),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CliDescribeReport {
    pub(crate) name: String,
    pub(crate) about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) after_help: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) global_options: Vec<CliArgDescribe>,
    pub(crate) commands: Vec<CliCommandDescribe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CliCommandDescribe {
    pub(crate) name: String,
    pub(crate) about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) after_help: Option<String>,
    pub(crate) options: Vec<CliArgDescribe>,
    pub(crate) subcommands: Vec<CliCommandDescribe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CliArgDescribe {
    pub(crate) id: String,
    pub(crate) long: Option<String>,
    pub(crate) short: Option<char>,
    pub(crate) help: Option<String>,
    pub(crate) required: bool,
    pub(crate) value_names: Vec<String>,
    pub(crate) possible_values: Vec<String>,
}

pub(crate) fn describe_cli() -> CliDescribeReport {
    let command = Cli::command().bin_name("vulcan");
    let name = command
        .get_bin_name()
        .unwrap_or(command.get_name())
        .to_string();
    CliDescribeReport {
        name,
        about: command.get_about().map(ToString::to_string),
        after_help: command.get_after_help().map(ToString::to_string),
        version: command.get_version().map(ToString::to_string),
        global_options: command
            .get_arguments()
            .filter(|argument| argument.is_global_set())
            .map(describe_argument)
            .collect(),
        commands: command
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
            .map(describe_command)
            .collect(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct OpenAiToolsReport {
    tools: Vec<OpenAiToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OpenAiToolDefinition {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) function: OpenAiFunctionDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OpenAiFunctionDefinition {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) parameters: Value,
    pub(crate) examples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct McpToolsReport {
    #[serde(rename = "protocolVersion")]
    pub(crate) protocol_version: String,
    #[serde(rename = "toolPackMode")]
    pub(crate) tool_pack_mode: String,
    #[serde(rename = "selectedToolPacks")]
    pub(crate) selected_tool_packs: Vec<String>,
    pub(crate) tools: Vec<McpToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct McpToolDefinition {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) description: String,
    #[serde(rename = "inputSchema")]
    pub(crate) input_schema: Value,
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    pub(crate) output_schema: Option<Value>,
    pub(crate) annotations: McpToolAnnotations,
    #[serde(rename = "toolPacks")]
    pub(crate) tool_packs: Vec<String>,
    pub(crate) examples: Vec<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct McpToolAnnotations {
    #[serde(rename = "readOnlyHint")]
    pub(crate) read_only_hint: bool,
    #[serde(rename = "destructiveHint")]
    pub(crate) destructive_hint: bool,
    #[serde(rename = "idempotentHint")]
    pub(crate) idempotent_hint: bool,
    #[serde(rename = "openWorldHint")]
    pub(crate) open_world_hint: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolRegistryEntry {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) input_schema: Value,
    pub(crate) output_schema: Option<Value>,
    pub(crate) annotations: McpToolAnnotations,
    pub(crate) tool_packs: Vec<String>,
    pub(crate) examples: Vec<String>,
}

impl ToolRegistryEntry {
    pub(crate) fn into_openai_definition(self) -> OpenAiToolDefinition {
        OpenAiToolDefinition {
            kind: "function".to_string(),
            function: OpenAiFunctionDefinition {
                name: self.name,
                description: self.description,
                parameters: self.input_schema,
                examples: self.examples,
            },
        }
    }

    pub(crate) fn to_mcp_definition(&self) -> McpToolDefinition {
        McpToolDefinition {
            name: self.name.clone(),
            title: self.title.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            output_schema: self.output_schema.clone(),
            annotations: self.annotations,
            tool_packs: self.tool_packs.clone(),
            examples: self.examples.clone(),
        }
    }

    pub(crate) fn to_mcp_list_item(&self) -> Value {
        let definition = self.to_mcp_definition();
        serde_json::json!({
            "name": definition.name,
            "title": definition.title,
            "description": definition.description,
            "inputSchema": definition.input_schema,
            "outputSchema": definition.output_schema,
            "annotations": definition.annotations,
            "toolPacks": definition.tool_packs,
        })
    }
}

pub(crate) fn cli_command_tree() -> clap::Command {
    Cli::command().bin_name("vulcan")
}

pub(crate) fn collect_cli_leaf_tool_names(command: &clap::Command) -> Vec<String> {
    let mut names = Vec::new();
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        collect_cli_leaf_tool_names_inner(subcommand, Vec::new(), &mut names);
    }
    names
}

fn collect_cli_leaf_tool_names_inner(
    command: &clap::Command,
    mut prefix: Vec<String>,
    names: &mut Vec<String>,
) {
    prefix.push(command.get_name().to_string());
    let subcommands = command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .collect::<Vec<_>>();
    if subcommands.is_empty() {
        names.push(tool_name_from_path(&prefix));
        return;
    }
    for subcommand in subcommands {
        collect_cli_leaf_tool_names_inner(subcommand, prefix.clone(), names);
    }
}

fn tool_name_from_path(path: &[String]) -> String {
    let path = path.iter().map(String::as_str).collect::<Vec<_>>();
    tool_name_from_str_path(&path)
}

fn tool_name_from_str_path(path: &[&str]) -> String {
    path.iter()
        .map(|segment| segment.replace('-', "_"))
        .collect::<Vec<_>>()
        .join("_")
}

fn command_input_schema(command: &clap::Command) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for argument in command
        .get_arguments()
        .filter(|argument| !argument.is_global_set())
    {
        properties.insert(
            argument.get_id().to_string(),
            argument_json_schema(argument),
        );
        if argument.is_required_set() {
            required.push(Value::String(argument.get_id().to_string()));
        }
    }
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(properties));
    schema.insert("additionalProperties".to_string(), Value::Bool(false));
    if !required.is_empty() {
        schema.insert("required".to_string(), Value::Array(required));
    }
    Value::Object(schema)
}

fn argument_json_schema(argument: &clap::Arg) -> Value {
    let schema = match argument.get_action() {
        clap::ArgAction::SetTrue | clap::ArgAction::SetFalse => serde_json::json!({
            "type": "boolean",
        }),
        clap::ArgAction::Append => serde_json::json!({
            "type": "array",
            "items": scalar_argument_schema(argument),
        }),
        clap::ArgAction::Count => serde_json::json!({
            "type": "integer",
        }),
        _ => scalar_argument_schema(argument),
    };

    let mut schema = schema;
    if let Some(description) = argument.get_help().map(ToString::to_string) {
        if let Some(object) = schema.as_object_mut() {
            object.insert("description".to_string(), Value::String(description));
        }
    }
    if let Some(default) = argument.get_default_values().first() {
        if let Some(object) = schema.as_object_mut() {
            object.insert(
                "default".to_string(),
                Value::String(default.to_string_lossy().to_string()),
            );
        }
    }
    schema
}

fn scalar_argument_schema(argument: &clap::Arg) -> Value {
    let values = argument
        .get_possible_values()
        .into_iter()
        .map(|value| Value::String(value.get_name().to_string()))
        .collect::<Vec<_>>();
    if values
        == [
            Value::String("true".to_string()),
            Value::String("false".to_string()),
        ]
    {
        serde_json::json!({ "type": "boolean" })
    } else if values.is_empty() {
        serde_json::json!({ "type": "string" })
    } else {
        serde_json::json!({
            "type": "string",
            "enum": values,
        })
    }
}

fn print_describe_human(report: &CliDescribeReport) {
    let command_count = count_described_commands(&report.commands);
    println!("Machine-readable Vulcan tool schema");
    println!();
    println!(
        "`describe` is intended for harnesses and tool integrations, not interactive browsing."
    );
    println!("For human-oriented documentation, use `vulcan help` or `vulcan help <command>`.");
    println!();
    println!(
        "Available exports: {command_count} commands, {} global options.",
        report.global_options.len()
    );
    println!();
    println!("Use one of:");
    println!("- `vulcan --output json describe` for the recursive CLI schema");
    println!("- `vulcan describe --format openai-tools` for curated OpenAI tool definitions");
    println!("- `vulcan describe --format mcp` for curated MCP tool definitions");
}

fn count_described_commands(commands: &[CliCommandDescribe]) -> usize {
    commands
        .iter()
        .map(|command| 1 + count_described_commands(&command.subcommands))
        .sum()
}

pub(crate) fn resolve_help_topic(topic: &[String]) -> Result<HelpTopicReport, CliError> {
    let key = topic.join(" ");
    if let Some(report) = builtin_help_topic(&key) {
        return Ok(report);
    }
    let root = cli_command_tree();
    let topic_refs = topic.iter().map(String::as_str).collect::<Vec<_>>();
    let Some(command) = find_command(&root, &topic_refs) else {
        return Err(CliError::operation(format!("unknown help topic `{key}`")));
    };
    Ok(help_topic_from_command(command, topic))
}

fn search_help_topics(keyword: &str) -> HelpSearchReport {
    let lowered = keyword.to_ascii_lowercase();
    let mut matches = builtin_help_topics()
        .into_iter()
        .filter(|topic| {
            topic.name.to_ascii_lowercase().contains(&lowered)
                || topic.summary.to_ascii_lowercase().contains(&lowered)
                || topic.body.to_ascii_lowercase().contains(&lowered)
        })
        .map(|topic| HelpSearchMatch {
            name: topic.name,
            kind: topic.kind,
            summary: topic.summary,
        })
        .collect::<Vec<_>>();

    matches.extend(
        collect_help_command_topics(&cli_command_tree())
            .into_iter()
            .filter(|topic| {
                topic.name.to_ascii_lowercase().contains(&lowered)
                    || topic.summary.to_ascii_lowercase().contains(&lowered)
                    || topic.body.to_ascii_lowercase().contains(&lowered)
            })
            .map(|topic| HelpSearchMatch {
                name: topic.name,
                kind: topic.kind,
                summary: topic.summary,
            }),
    );

    matches.sort_by(|left, right| left.name.cmp(&right.name));
    HelpSearchReport {
        keyword: keyword.to_string(),
        matches,
    }
}

pub(crate) fn collect_help_command_topics(command: &clap::Command) -> Vec<HelpTopicReport> {
    let mut topics = Vec::new();
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        collect_help_command_topics_inner(subcommand, Vec::new(), &mut topics);
    }
    topics
}

fn collect_help_command_topics_inner(
    command: &clap::Command,
    mut prefix: Vec<String>,
    topics: &mut Vec<HelpTopicReport>,
) {
    prefix.push(command.get_name().to_string());
    topics.push(help_topic_from_command(command, &prefix));
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        collect_help_command_topics_inner(subcommand, prefix.clone(), topics);
    }
}

fn help_topic_from_command(command: &clap::Command, path: &[String]) -> HelpTopicReport {
    let summary = command.get_about().map_or_else(
        || format!("Help for `{}`", path.join(" ")),
        ToString::to_string,
    );
    let mut sections = Vec::new();
    if let Some(after_help) = command.get_after_help() {
        let trimmed = strip_help_section(&after_help.to_string(), "Subcommands:");
        if !trimmed.is_empty() {
            sections.push(trimmed);
        }
    }
    if let Some(command_tree) = command_tree_section("Subcommands", command, true) {
        sections.push(command_tree);
    }
    if path == ["config"] {
        sections.push(render_config_reference_markdown(true));
    }
    let subcommands = command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .map(|subcommand| {
            format!(
                "{} {}",
                path.join(" "),
                subcommand.get_name().replace('-', "_").replace('_', "-")
            )
        })
        .collect::<Vec<_>>();

    HelpTopicReport {
        name: path.join(" "),
        kind: HelpTopicKind::Command,
        summary,
        body: sections.join("\n\n"),
        options: command
            .get_arguments()
            .filter(|argument| !argument.is_global_set())
            .map(describe_argument)
            .collect(),
        subcommands,
        related: Vec::new(),
    }
}

pub(crate) fn render_config_reference_markdown(include_title: bool) -> String {
    let descriptors = app_config::config_descriptor_catalog();
    let mut grouped = BTreeMap::<String, Vec<app_config::ConfigDescriptor>>::new();
    for descriptor in descriptors {
        grouped
            .entry(descriptor.section.clone())
            .or_default()
            .push(descriptor);
    }

    let mut lines = Vec::new();
    if include_title {
        lines.push("## Generated Config Reference".to_string());
        lines.push(String::new());
    }
    lines.push(
        "Derived from Vulcan's config descriptor registry. `config set`, `config unset`, `config list`, the settings TUI, and this help surface share the same supported key metadata.".to_string(),
    );
    lines.push(String::new());
    lines.push("Precedence: `.vulcan/config.local.toml` > `.vulcan/config.toml` > `.obsidian/*` imports > built-in defaults.".to_string());
    lines.push(String::new());
    lines.push("Prefer dedicated commands when available: `config alias ...`, `config permissions profile ...`, `plugin set ...`, and `export profile ...`.".to_string());
    lines.push(String::new());
    lines.push("Manual editing is still supported. Use `.vulcan/config.toml` for shared defaults you want to sync, and `.vulcan/config.local.toml` for machine-local overrides such as developer-specific paths, API env-var names, or temporary experiments.".to_string());
    lines.push(String::new());
    lines.push("Typical TOML blocks:".to_string());
    lines.push(String::new());
    lines.push("```toml".to_string());
    lines.push("[aliases]".to_string());
    lines.push("ship = \"query --where 'status = shipped'\"".to_string());
    lines.push(String::new());
    lines.push("[permissions.profiles.agent]".to_string());
    lines.push("read = \"all\"".to_string());
    lines.push("network = { allow = true, domains = [\"docs.example.com\"] }".to_string());
    lines.push(String::new());
    lines.push("[plugins.lint]".to_string());
    lines.push("enabled = true".to_string());
    lines.push("path = \".vulcan/plugins/lint.js\"".to_string());
    lines.push("events = [\"on_note_write\", \"on_pre_commit\"]".to_string());
    lines.push("sandbox = \"strict\"".to_string());
    lines.push("permission_profile = \"agent\"".to_string());
    lines.push(String::new());
    lines.push("[web.search]".to_string());
    lines.push("backend = \"brave\"".to_string());
    lines.push("api_key_env = \"BRAVE_API_KEY\"".to_string());
    lines.push("```".to_string());
    lines.push(String::new());

    let mut sections = grouped.into_iter().collect::<Vec<_>>();
    sections.sort_by(|(left, _), (right, _)| {
        config_section_order(left)
            .cmp(&config_section_order(right))
            .then_with(|| left.cmp(right))
    });

    for (_section, mut descriptors) in sections {
        descriptors.sort_by(|left, right| left.key.cmp(&right.key));
        let Some(first) = descriptors.first() else {
            continue;
        };
        lines.push(format!("### {}", first.section_title));
        lines.push(String::new());
        lines.push(first.section_description.clone());
        lines.push(String::new());
        for descriptor in descriptors {
            let mut meta = vec![
                format!("type: `{}`", config_value_kind_label(&descriptor.kind)),
                format!(
                    "target: `{}`",
                    config_target_support_label(descriptor.target_support)
                ),
            ];
            if let Some(default_display) = descriptor.default_display.as_deref() {
                meta.push(format!("default: `{default_display}`"));
            }
            if !descriptor.enum_values.is_empty() {
                meta.push(format!("values: `{}`", descriptor.enum_values.join("`, `")));
            }
            lines.push(format!("- `{}` — {}", descriptor.key, meta.join("; ")));
            lines.push(format!("  {}", descriptor.description));
            if let Some(command) = descriptor.preferred_command.as_deref() {
                lines.push(format!("  Preferred command: `{command}`"));
            }
            if let Some(example) = descriptor.examples.first() {
                lines.push(format!("  Example: `{example}`"));
            }
        }
        lines.push(String::new());
    }

    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

fn config_value_kind_label(kind: &app_config::ConfigValueKind) -> &'static str {
    match kind {
        app_config::ConfigValueKind::String => "string",
        app_config::ConfigValueKind::Integer => "integer",
        app_config::ConfigValueKind::Float => "float",
        app_config::ConfigValueKind::Boolean => "boolean",
        app_config::ConfigValueKind::Array => "array",
        app_config::ConfigValueKind::Object => "object",
        app_config::ConfigValueKind::Enum => "enum",
        app_config::ConfigValueKind::Flexible => "flexible",
    }
}

fn config_target_support_label(target_support: app_config::ConfigTargetSupport) -> &'static str {
    match target_support {
        app_config::ConfigTargetSupport::SharedOnly => "shared",
        app_config::ConfigTargetSupport::LocalOnly => "local",
        app_config::ConfigTargetSupport::SharedAndLocal => "shared|local",
    }
}

fn config_section_order(section: &str) -> usize {
    match section {
        "general" => 0,
        "links" => 1,
        "properties" => 2,
        "templates" => 3,
        "periodic" => 4,
        "tasks" => 5,
        "tasknotes" => 6,
        "kanban" => 7,
        "dataview" => 8,
        "js_runtime" => 9,
        "web" => 10,
        "plugins" => 11,
        "permissions" => 12,
        "aliases" => 13,
        "export" => 14,
        _ => 50,
    }
}

fn strip_help_section(after_help: &str, heading: &str) -> String {
    let mut result = Vec::new();
    let mut lines = after_help.lines().peekable();

    while let Some(line) = lines.next() {
        if line.trim() == heading {
            while let Some(next_line) = lines.peek() {
                if next_line.trim().is_empty() {
                    lines.next();
                    break;
                }
                lines.next();
            }
            continue;
        }
        result.push(line);
    }

    while result.first().is_some_and(|line| line.trim().is_empty()) {
        result.remove(0);
    }
    while result.last().is_some_and(|line| line.trim().is_empty()) {
        result.pop();
    }

    let mut normalized = Vec::new();
    let mut previous_blank = false;
    for line in result {
        let blank = line.trim().is_empty();
        if blank && previous_blank {
            continue;
        }
        normalized.push(line);
        previous_blank = blank;
    }

    normalized.join("\n")
}

fn command_tree_section(
    title: &str,
    command: &clap::Command,
    include_examples: bool,
) -> Option<String> {
    let mut lines = Vec::new();
    append_command_tree_lines(command, 0, include_examples, &mut lines);
    if lines.is_empty() {
        None
    } else {
        let code_block = lines
            .into_iter()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        Some(format!("## {title}\n\n{code_block}"))
    }
}

fn append_command_tree_lines(
    command: &clap::Command,
    depth: usize,
    include_examples: bool,
    lines: &mut Vec<String>,
) {
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        let indent = "  ".repeat(depth);
        let name = subcommand.get_name();
        let summary = subcommand
            .get_about()
            .map_or_else(|| "undocumented".to_string(), ToString::to_string);
        lines.push(format!("{indent}{name:<16} {summary}"));
        if include_examples {
            if let Some(example) = extract_examples(subcommand).first() {
                lines.push(format!("{indent}  e.g. {example}"));
            }
        }
        append_command_tree_lines(subcommand, depth + 1, include_examples, lines);
    }
}

fn find_command<'a>(command: &'a clap::Command, path: &[&str]) -> Option<&'a clap::Command> {
    let mut current = command;
    for segment in path {
        current = current
            .get_subcommands()
            .find(|candidate| candidate.get_name().eq_ignore_ascii_case(segment))?;
    }
    Some(current)
}

fn build_openai_tool_definitions(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack: &[McpToolPackArg],
    tool_pack_mode: McpToolPackModeArg,
) -> Result<OpenAiToolsReport, CliError> {
    let tools = mcp::build_openai_tool_registry_entries(
        paths,
        requested_profile,
        tool_pack,
        tool_pack_mode,
    )?
    .into_iter()
    .chain(openai_cli_helper_tool_entries())
    .map(ToolRegistryEntry::into_openai_definition)
    .collect::<Vec<_>>();
    Ok(OpenAiToolsReport { tools })
}

const OPENAI_CLI_HELPER_COMMANDS: &[&[&str]] = &[
    &["skill", "list"],
    &["skill", "get"],
    &["agent", "print-config"],
];

fn openai_cli_helper_tool_entries() -> Vec<ToolRegistryEntry> {
    let command_tree = cli_command_tree();
    OPENAI_CLI_HELPER_COMMANDS
        .iter()
        .filter_map(|path| command_to_openai_helper_entry(&command_tree, path))
        .collect()
}

fn command_to_openai_helper_entry(
    command_tree: &clap::Command,
    path: &[&str],
) -> Option<ToolRegistryEntry> {
    let command = find_command(command_tree, path)?;
    Some(ToolRegistryEntry {
        name: tool_name_from_str_path(path),
        title: path.join(" "),
        description: command
            .get_about()
            .map_or_else(|| path.join(" "), ToString::to_string),
        input_schema: command_input_schema(command),
        output_schema: None,
        annotations: McpToolAnnotations::default(),
        tool_packs: vec!["assistant".to_string()],
        examples: extract_examples(command),
    })
}

pub(crate) fn custom_tool_registry_entry(tool: &tools::CustomToolDescriptor) -> ToolRegistryEntry {
    ToolRegistryEntry {
        name: tool.summary.name.clone(),
        title: tool
            .summary
            .title
            .clone()
            .unwrap_or_else(|| tool.summary.name.clone()),
        description: tool.summary.description.clone(),
        input_schema: tool.summary.input_schema.clone(),
        output_schema: tool.summary.output_schema.clone(),
        annotations: McpToolAnnotations {
            read_only_hint: tool.summary.read_only,
            destructive_hint: tool.summary.destructive,
            idempotent_hint: tool.summary.read_only && !tool.summary.destructive,
            open_world_hint: matches!(tool.summary.sandbox, vulcan_core::JsRuntimeSandbox::Net),
        },
        tool_packs: tool.summary.packs.clone(),
        examples: Vec::new(),
    }
}

fn extract_examples(command: &clap::Command) -> Vec<String> {
    let Some(after_help) = command.get_after_help() else {
        return Vec::new();
    };
    let mut capture = false;
    let mut examples = Vec::new();
    for line in after_help.to_string().lines() {
        let trimmed = line.trim();
        if trimmed == "Examples:" {
            capture = true;
            continue;
        }
        if !capture {
            continue;
        }
        if trimmed.is_empty() {
            break;
        }
        examples.push(trimmed.to_string());
    }
    examples
}

fn describe_command(command: &clap::Command) -> CliCommandDescribe {
    CliCommandDescribe {
        name: command.get_name().to_string(),
        about: command.get_about().map(ToString::to_string),
        after_help: command.get_after_help().map(ToString::to_string),
        options: command
            .get_arguments()
            .filter(|argument| !argument.is_global_set())
            .map(describe_argument)
            .collect(),
        subcommands: command
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
            .map(describe_command)
            .collect(),
    }
}

fn describe_argument(argument: &clap::Arg) -> CliArgDescribe {
    CliArgDescribe {
        id: argument.get_id().to_string(),
        long: argument.get_long().map(ToString::to_string),
        short: argument.get_short(),
        help: argument.get_help().map(ToString::to_string),
        required: argument.is_required_set(),
        value_names: argument.get_value_names().map_or_else(Vec::new, |values| {
            values.iter().map(ToString::to_string).collect()
        }),
        possible_values: argument
            .get_possible_values()
            .into_iter()
            .map(|value| value.get_name().to_string())
            .collect(),
    }
}
