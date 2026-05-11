mod cli_args;
mod lint;
mod skill_commands;
#[cfg(test)]
mod tests;
mod typescript;

pub use cli_args::{
    build_custom_tool_cli_input, collect_custom_tool_cli_choice_candidates,
    collect_custom_tool_cli_flag_candidates, collect_custom_tool_cli_name_candidates,
    custom_tool_cli_flag_completion_context, resolve_custom_tool_cli_name,
};
pub use lint::{
    build_tool_compat_report, lint_custom_tools, CustomToolCompatReport,
    CustomToolCompatSurfaceReport, CustomToolLintReport, CustomToolLintToolReport,
};
pub use skill_commands::{
    build_custom_tool_js_registry, list_custom_tools, require_trusted_tool_execution,
    run_custom_tool, show_custom_tool, CustomToolDescriptor, CustomToolRegistryOptions,
    CustomToolRunOptions, CustomToolRunReport, CustomToolShowReport,
};
pub(crate) use skill_commands::{
    command_matches_allowed_packs, resolve_skill_command_tool_identifier, skill_command_tool_name,
};
pub use typescript::{
    build_all_tool_types_report, build_tool_types_report, json_schema_to_typescript,
    CustomToolTypesReport, CustomToolTypesSuiteReport,
};
