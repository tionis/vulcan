//! Permission-checked MCP invocation of vault-defined tools.

use serde_json::{Map, Value};
use vulcan_core::VaultPaths;

use crate::mcp_protocol::McpMethodError;
use crate::tools::{run_custom_tool, show_custom_tool, CustomToolRunOptions, CustomToolRunReport};

pub fn call_custom_tool(
    paths: &VaultPaths,
    profile_name: &str,
    name: &str,
    arguments: &Map<String, Value>,
) -> Result<CustomToolRunReport, McpMethodError> {
    let unknown = || McpMethodError::invalid_params(format!("Unknown tool: {name}"));
    let report = show_custom_tool(paths, Some(profile_name), name).map_err(|_| unknown())?;
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
    fn custom_call_hides_unknown_tools_and_denies_uncallable_ones() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let arguments = Map::new();
        let missing = call_custom_tool(&paths, "unrestricted", "missing", &arguments);
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
        let descriptors = list_custom_tools(&paths, Some("unrestricted")).unwrap();
        assert_eq!(descriptors.len(), 1);
        let name = &descriptors[0].summary.name;
        let denied = call_custom_tool(&paths, "unrestricted", name, &arguments).unwrap_err();
        assert!(matches!(
            denied,
            McpMethodError::Tool { message, .. } if message.contains("permission denied")
        ));
    }
}
