//! Permission- and pack-checked MCP invocation of vault-defined tools.

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use vulcan_core::VaultPaths;

use crate::mcp_assistant::custom_tool_matches_selected_packs;
use crate::mcp_catalog::{pack_name_list, McpToolPack};
use crate::mcp_protocol::McpMethodError;
use crate::tools::{
    run_custom_tool, show_custom_tool, CustomToolRegistryOptions, CustomToolRunOptions,
    CustomToolRunReport,
};

pub fn call_custom_tool(
    paths: &VaultPaths,
    profile_name: &str,
    selected_packs: &BTreeSet<McpToolPack>,
    registry_options: &CustomToolRegistryOptions,
    name: &str,
    arguments: &Map<String, Value>,
) -> Result<CustomToolRunReport, McpMethodError> {
    let unknown = || McpMethodError::invalid_params(format!("Unknown tool: {name}"));
    if !selected_packs.contains(&McpToolPack::Custom) {
        return Err(unknown());
    }
    let report = show_custom_tool(paths, Some(profile_name), name, registry_options)
        .map_err(|_| unknown())?;
    let selected_pack_names = pack_name_list(selected_packs)
        .into_iter()
        .collect::<BTreeSet<_>>();
    if !custom_tool_matches_selected_packs(&report.tool.summary.packs, &selected_pack_names) {
        return Err(unknown());
    }
    if !report.callable {
        return Err(McpMethodError::tool(format!(
            "permission denied: tool `{name}` is not available under profile `{profile_name}`"
        )));
    }
    run_custom_tool(
        paths,
        Some(profile_name),
        name,
        &Value::Object(arguments.clone()),
        registry_options,
        &CustomToolRunOptions {
            surface: "mcp".to_string(),
        },
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::list_custom_tools;
    use std::fs;

    #[test]
    fn custom_call_hides_unselected_and_unknown_tools() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let options = CustomToolRegistryOptions::default();
        let arguments = Map::new();
        let no_packs = BTreeSet::new();
        let denied = call_custom_tool(
            &paths,
            "unrestricted",
            &no_packs,
            &options,
            "missing",
            &arguments,
        );
        assert!(matches!(
            denied,
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));

        let selected = BTreeSet::from([McpToolPack::Custom]);
        let missing = call_custom_tool(
            &paths,
            "unrestricted",
            &selected,
            &options,
            "missing",
            &arguments,
        );
        assert!(matches!(
            missing,
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));

        let skill = temporary.path().join(".agents/skills/sample");
        fs::create_dir_all(skill.join("scripts")).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: sample\ndescription: Sample tool\nmetadata:\n  vulcan:\n    commands:\n      - id: run\n        description: Run sample\n        script: scripts/main.js\n        expose: true\n        input_schema:\n          type: object\n---\n# Sample\n",
        )
        .unwrap();
        fs::write(
            skill.join("scripts/main.js"),
            "function main() { return {}; }\n",
        )
        .unwrap();
        let descriptors = list_custom_tools(&paths, Some("unrestricted"), &options).unwrap();
        assert_eq!(descriptors.len(), 1);
        let name = &descriptors[0].summary.name;
        let denied = call_custom_tool(
            &paths,
            "unrestricted",
            &selected,
            &options,
            name,
            &arguments,
        )
        .unwrap_err();
        assert!(matches!(
            denied,
            McpMethodError::Tool { message, .. } if message.contains("permission denied")
        ));
    }
}
