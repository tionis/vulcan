use std::collections::BTreeSet;

use crate::{McpToolPackArg, McpToolPackModeArg, ToolRegistryEntry};
use vulcan_app::mcp_catalog::resolve_selected_tool_packs as resolve_shared_tool_packs;
pub(super) use vulcan_app::mcp_catalog::{
    default_openai_tool_packs, pack_name_list, tool_by_name, tool_visible, visible_tool_catalog,
    McpToolCatalogEntry, McpToolId, McpToolPack, McpToolPackMode, McpVisibilityRequirement,
};

impl From<McpToolPackModeArg> for McpToolPackMode {
    fn from(value: McpToolPackModeArg) -> Self {
        match value {
            McpToolPackModeArg::Static => Self::Static,
            McpToolPackModeArg::Adaptive => Self::Adaptive,
        }
    }
}

fn pack_from_arg(value: McpToolPackArg) -> McpToolPack {
    match value {
        McpToolPackArg::NotesRead => McpToolPack::NotesRead,
        McpToolPackArg::Search => McpToolPack::Search,
        McpToolPackArg::Status => McpToolPack::Status,
        McpToolPackArg::Graph => McpToolPack::Graph,
        McpToolPackArg::Custom => McpToolPack::Custom,
        McpToolPackArg::Daily => McpToolPack::Daily,
        McpToolPackArg::Tasks => McpToolPack::Tasks,
        McpToolPackArg::NotesWrite => McpToolPack::NotesWrite,
        McpToolPackArg::NotesManage => McpToolPack::NotesManage,
        McpToolPackArg::Web => McpToolPack::Web,
        McpToolPackArg::Config => McpToolPack::Config,
        McpToolPackArg::Index => McpToolPack::Index,
        McpToolPackArg::Sync => McpToolPack::Sync,
    }
}

pub(super) fn resolve_selected_tool_packs(
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode: McpToolPackMode,
) -> BTreeSet<McpToolPack> {
    let requested: Vec<McpToolPack> = tool_pack_args.iter().copied().map(pack_from_arg).collect();
    resolve_shared_tool_packs(&requested, tool_pack_mode)
}

pub(super) fn is_default_tool_pack_args(tool_pack_args: &[McpToolPackArg]) -> bool {
    tool_pack_args
        == [
            McpToolPackArg::NotesRead,
            McpToolPackArg::Search,
            McpToolPackArg::Status,
        ]
}

pub(super) fn mcp_tool_registry_entry(tool: &McpToolCatalogEntry) -> ToolRegistryEntry {
    ToolRegistryEntry {
        name: tool.name.to_string(),
        title: tool.title.to_string(),
        description: tool.description.to_string(),
        input_schema: (tool.input_schema)(),
        output_schema: tool.output_schema.map(|schema| schema()),
        annotations: tool.annotations,
        tool_packs: tool
            .packs
            .iter()
            .map(|pack| pack.as_str().to_string())
            .collect(),
        examples: tool
            .examples
            .iter()
            .map(|item| (*item).to_string())
            .collect(),
    }
}
