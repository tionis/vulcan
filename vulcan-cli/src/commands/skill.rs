use crate::output::print_json;
use crate::{selected_permission_guard, Cli, CliError, OutputFormat, SkillCommand};
use serde::Serialize;
use vulcan_core::{
    list_assistant_skills, load_assistant_skill, load_vault_config, AssistantSkill,
    AssistantSkillCommandSummary, AssistantSkillSummary, PermissionGuard, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillListReport {
    skills: Vec<AssistantSkillSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillCommandsReport {
    commands: Vec<SkillCommandRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillCommandRow {
    skill: String,
    skill_path: String,
    #[serde(flatten)]
    command: AssistantSkillCommandSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillValidateReport {
    valid: bool,
    skills: usize,
    commands: usize,
}

pub(crate) fn handle_skill_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SkillCommand,
) -> Result<(), CliError> {
    match command {
        SkillCommand::List => {
            let report = SkillListReport {
                skills: visible_skills(cli, paths)?,
            };
            print_skill_list_report(cli.output, &report)
        }
        SkillCommand::Get { name } | SkillCommand::Show { name } => {
            let skill = visible_skill(cli, paths, name)?;
            print_skill_report(cli.output, &skill)
        }
        SkillCommand::Commands { name } => {
            let rows = if let Some(name) = name {
                let skill = visible_skill(cli, paths, name)?;
                skill_command_rows(&skill)
            } else {
                visible_skills(cli, paths)?
                    .into_iter()
                    .flat_map(|summary| {
                        let skill = summary.name.clone();
                        let skill_path = summary.path.clone();
                        summary
                            .commands
                            .into_iter()
                            .map(move |command| SkillCommandRow {
                                skill: skill.clone(),
                                skill_path: skill_path.clone(),
                                command,
                            })
                    })
                    .collect()
            };
            print_skill_commands_report(cli.output, &SkillCommandsReport { commands: rows })
        }
        SkillCommand::Validate => {
            let skills = visible_skills(cli, paths)?;
            let commands = skills.iter().map(|skill| skill.commands.len()).sum();
            print_skill_validate_report(
                cli.output,
                &SkillValidateReport {
                    valid: true,
                    skills: skills.len(),
                    commands,
                },
            )
        }
    }
}

fn skill_command_rows(skill: &AssistantSkill) -> Vec<SkillCommandRow> {
    skill
        .summary
        .commands
        .iter()
        .cloned()
        .map(|command| SkillCommandRow {
            skill: skill.summary.name.clone(),
            skill_path: skill.summary.path.clone(),
            command,
        })
        .collect()
}

fn visible_skills(cli: &Cli, paths: &VaultPaths) -> Result<Vec<AssistantSkillSummary>, CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    let skills = list_assistant_skills(paths).map_err(CliError::operation)?;
    Ok(skills
        .into_iter()
        .filter(|skill| {
            guard
                .check_read_path(&skill_relative_path(paths, skill))
                .is_ok()
        })
        .collect())
}

fn visible_skill(cli: &Cli, paths: &VaultPaths, name: &str) -> Result<AssistantSkill, CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    let skill = load_assistant_skill(paths, name).map_err(CliError::operation)?;
    let relative_path = skill_relative_path(paths, &skill.summary);
    guard
        .check_read_path(&relative_path)
        .map_err(CliError::operation)?;
    Ok(skill)
}

fn skill_relative_path(paths: &VaultPaths, skill: &AssistantSkillSummary) -> String {
    load_vault_config(paths)
        .config
        .assistant
        .skills_folder
        .join(&skill.path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn print_skill_list_report(output: OutputFormat, report: &SkillListReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.skills.is_empty() {
                println!("No visible skills.");
                return Ok(());
            }
            for skill in &report.skills {
                let title = skill.title.as_deref().unwrap_or(&skill.name);
                match skill.description.as_deref() {
                    Some(description) => {
                        println!("- {} [{}] — {}", skill.name, skill.path, description);
                    }
                    None => println!("- {} [{}]", title, skill.path),
                }
            }
            Ok(())
        }
    }
}

fn print_skill_commands_report(
    output: OutputFormat,
    report: &SkillCommandsReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.commands.is_empty() {
                println!("No skill commands.");
                return Ok(());
            }
            for row in &report.commands {
                println!(
                    "- {}:{} -> {}",
                    row.skill, row.command.id, row.command.script
                );
            }
            Ok(())
        }
    }
}

fn print_skill_validate_report(
    output: OutputFormat,
    report: &SkillValidateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Valid skills: {} ({} skills, {} commands)",
                report.valid, report.skills, report.commands
            );
            Ok(())
        }
    }
}

fn print_skill_report(output: OutputFormat, skill: &AssistantSkill) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(skill),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{}",
                skill
                    .summary
                    .title
                    .as_deref()
                    .unwrap_or(&skill.summary.name)
            );
            println!("Path: {}", skill.summary.path);
            if let Some(description) = skill.summary.description.as_deref() {
                println!("Description: {description}");
            }
            if !skill.summary.tools.is_empty() {
                println!("Tools: {}", skill.summary.tools.join(", "));
            }
            if let Some(output_file) = skill.summary.output_file.as_deref() {
                println!("Output file: {output_file}");
            }
            if !skill.summary.tags.is_empty() {
                println!("Tags: {}", skill.summary.tags.join(", "));
            }
            if !skill.body.is_empty() {
                println!();
                println!("{}", skill.body);
            }
            Ok(())
        }
    }
}
