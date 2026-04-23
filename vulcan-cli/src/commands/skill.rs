use crate::output::print_json;
use crate::{selected_permission_guard, Cli, CliError, OutputFormat, SkillCommand};
use serde::Serialize;
use vulcan_core::{
    list_assistant_skills, load_assistant_skill, load_vault_config, AssistantSkill,
    AssistantSkillSummary, PermissionGuard, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillListReport {
    skills: Vec<AssistantSkillSummary>,
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
        SkillCommand::Get { name } => {
            let skill = visible_skill(cli, paths, name)?;
            print_skill_report(cli.output, &skill)
        }
    }
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
