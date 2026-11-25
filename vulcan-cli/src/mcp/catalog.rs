use crate::ToolRegistryEntry;
use vulcan_app::mcp_catalog::ALWAYS_LOADED_TOOLS;
pub(super) use vulcan_app::mcp_catalog::{visible_tool_catalog, McpToolCatalogEntry};

pub(super) fn mcp_tool_registry_entry(tool: &McpToolCatalogEntry) -> ToolRegistryEntry {
    ToolRegistryEntry {
        name: tool.name.to_string(),
        title: tool.title.to_string(),
        description: tool.description.to_string(),
        input_schema: (tool.input_schema)(),
        output_schema: tool.output_schema.map(|schema| schema()),
        annotations: tool.annotations,
        always_load: ALWAYS_LOADED_TOOLS.contains(&tool.name),
        examples: tool
            .examples
            .iter()
            .map(|item| (*item).to_string())
            .collect(),
    }
}
