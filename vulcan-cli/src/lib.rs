//! CLI and terminal-facing adapters for Vulcan.
//!
//! Reusable workflow orchestration should live in `vulcan-app` or
//! `vulcan-core`; this crate owns argument parsing, TUI state, interactive I/O,
//! terminal rendering, and other CLI-specific behavior.

mod bases_tui;
mod browse_tui;
mod bundle_server;
mod cli;
mod commands;
mod commit;
mod config_tui;
mod editor;
mod help;
mod js_repl;
mod mcp;
mod note_picker;
mod output;
mod resolve;
mod serve;
mod site_server;
mod terminal_markdown;

pub(crate) use commands::edit::{
    print_diff_report, print_edit_report, run_diff_command, run_edit_command, EditReport,
};
pub(crate) use commands::inbox::{print_inbox_report, run_inbox_command};
pub(crate) use commands::note::{
    normalize_note_path, path_buf_to_slash_string, resolve_existing_markdown_target,
    resolve_existing_note_path, run_note_append_command, run_note_create_with_body,
    run_note_delete_command, run_note_get_command, run_note_info_command, run_note_outline_command,
    run_note_patch_command, run_note_set_with_content, NoteAppendOptions, NoteGetOptions,
    NotePatchOptions,
};
#[cfg(test)]
pub(crate) use commands::template::TemplateSummary;
pub(crate) use commands::template::{
    print_template_create_report, print_template_insert_report, print_template_list_report,
    print_template_preview_report, run_template_command, run_template_insert_command,
    run_template_preview_command, run_template_show_command, TemplateCommandResult,
};
pub(crate) use vulcan_app::notes::NoteAppendMode;

mod plugins {
    use crate::CliError;
    use serde_json::Value;
    pub(crate) use vulcan_app::plugins::{list_plugins, PluginDescriptor};
    use vulcan_core::{DataviewJsResult, PluginEvent, VaultPaths};

    pub(crate) fn run_plugin(
        paths: &VaultPaths,
        active_permission_profile: Option<&str>,
        name: &str,
    ) -> Result<DataviewJsResult, CliError> {
        vulcan_app::plugins::run_plugin(paths, active_permission_profile, name)
            .map_err(CliError::operation)
    }

    pub(crate) fn dispatch_plugin_event(
        paths: &VaultPaths,
        active_permission_profile: Option<&str>,
        event: PluginEvent,
        payload: &Value,
        quiet: bool,
    ) -> Result<(), CliError> {
        vulcan_app::plugins::dispatch_plugin_event(
            paths,
            active_permission_profile,
            event,
            payload,
            quiet,
        )
        .map_err(CliError::operation)
    }
}

mod tools {
    use crate::CliError;
    use serde_json::Value;
    use std::sync::Arc;
    pub(crate) use vulcan_app::tools::{
        build_all_tool_types_report, build_custom_tool_js_registry, build_tool_compat_report,
        build_tool_types_report, lint_custom_tools, CustomToolCompatReport, CustomToolDescriptor,
        CustomToolLintReport, CustomToolRegistryOptions, CustomToolRunOptions, CustomToolRunReport,
        CustomToolShowReport, CustomToolTypesReport, CustomToolTypesSuiteReport,
    };
    use vulcan_core::{DataviewJsToolRegistry, JsRuntimeSandbox, VaultPaths};

    pub(crate) fn list_custom_tools(
        paths: &VaultPaths,
        active_permission_profile: Option<&str>,
        options: &CustomToolRegistryOptions,
    ) -> Result<Vec<CustomToolDescriptor>, CliError> {
        vulcan_app::tools::list_custom_tools(paths, active_permission_profile, options)
            .map_err(CliError::operation)
    }

    pub(crate) fn show_custom_tool(
        paths: &VaultPaths,
        active_permission_profile: Option<&str>,
        name: &str,
        options: &CustomToolRegistryOptions,
    ) -> Result<CustomToolShowReport, CliError> {
        vulcan_app::tools::show_custom_tool(paths, active_permission_profile, name, options)
            .map_err(CliError::operation)
    }

    pub(crate) fn run_custom_tool(
        paths: &VaultPaths,
        active_permission_profile: Option<&str>,
        name: &str,
        input: &Value,
        registry_options: &CustomToolRegistryOptions,
        run_options: &CustomToolRunOptions,
    ) -> Result<CustomToolRunReport, CliError> {
        vulcan_app::tools::run_custom_tool(
            paths,
            active_permission_profile,
            name,
            input,
            registry_options,
            run_options,
        )
        .map_err(CliError::operation)
    }

    pub(crate) fn resolve_custom_tool_cli_name(
        paths: &VaultPaths,
        name: &str,
        registry_options: &CustomToolRegistryOptions,
    ) -> Result<String, CliError> {
        vulcan_app::tools::resolve_custom_tool_cli_name(paths, name, registry_options)
            .map_err(CliError::operation)
    }

    pub(crate) fn build_custom_tool_cli_input(
        paths: &VaultPaths,
        name: &str,
        args: &[String],
        registry_options: &CustomToolRegistryOptions,
    ) -> Result<(String, Value), CliError> {
        vulcan_app::tools::build_custom_tool_cli_input(paths, name, args, registry_options)
            .map_err(CliError::operation)
    }

    pub(crate) fn runtime_tool_registry(
        paths: &VaultPaths,
        active_permission_profile: Option<&str>,
        surface: &str,
    ) -> Arc<dyn DataviewJsToolRegistry> {
        build_custom_tool_js_registry(
            paths,
            active_permission_profile,
            surface,
            &crate::custom_tool_registry_options(),
        )
    }

    pub(crate) fn require_trusted_tool_execution(
        paths: &VaultPaths,
        name: Option<&str>,
    ) -> Result<(), CliError> {
        vulcan_app::tools::require_trusted_tool_execution(paths, name).map_err(CliError::operation)
    }

    pub(crate) fn tool_sandbox(value: JsRuntimeSandbox) -> &'static str {
        match value {
            JsRuntimeSandbox::Strict => "strict",
            JsRuntimeSandbox::Fs => "fs",
            JsRuntimeSandbox::Net => "net",
            JsRuntimeSandbox::None => "none",
        }
    }
}

pub(crate) fn custom_tool_registry_options() -> tools::CustomToolRegistryOptions {
    let mut reserved_names = default_assistant_tool_reserved_names()
        .into_iter()
        .collect::<BTreeSet<_>>();
    reserved_names.extend(collect_cli_leaf_tool_names(&cli_command_tree()));
    tools::CustomToolRegistryOptions {
        reserved_names: reserved_names.into_iter().collect(),
        ..tools::CustomToolRegistryOptions::default()
    }
}

mod trust {
    use crate::CliError;
    use std::path::{Path, PathBuf};

    pub(crate) fn is_trusted(vault_root: &Path) -> bool {
        vulcan_app::trust::is_trusted(vault_root)
    }

    pub(crate) fn add_trust(vault_root: &Path) -> Result<bool, CliError> {
        vulcan_app::trust::add_trust(vault_root).map_err(CliError::operation)
    }

    pub(crate) fn revoke_trust(vault_root: &Path) -> Result<bool, CliError> {
        vulcan_app::trust::revoke_trust(vault_root).map_err(CliError::operation)
    }

    pub(crate) fn list_trusted() -> Result<Vec<PathBuf>, CliError> {
        vulcan_app::trust::list_trusted().map_err(CliError::operation)
    }
}

mod app_config {
    use crate::CliError;
    use std::path::Path;
    use toml::Value as TomlValue;
    use vulcan_core::VaultPaths;

    pub(crate) use vulcan_app::config::{
        ConfigBatchReport, ConfigDescriptor, ConfigDocumentSaveReport, ConfigGetReport,
        ConfigListEntry, ConfigListReport, ConfigMutationOperation, ConfigSetReport,
        ConfigShowReport, ConfigTarget, ConfigTargetSupport, ConfigUnsetReport, ConfigValueKind,
        ConfigValueSource,
    };

    pub(crate) fn build_config_show_report(
        paths: &VaultPaths,
        section: Option<&str>,
        selected_permission_profile: Option<&str>,
    ) -> Result<ConfigShowReport, CliError> {
        vulcan_app::config::build_config_show_report(paths, section, selected_permission_profile)
            .map_err(CliError::operation)
    }

    pub(crate) fn build_config_show_report_from_overrides(
        paths: &VaultPaths,
        shared_toml: &TomlValue,
        local_toml: &TomlValue,
        section: Option<&str>,
        selected_permission_profile: Option<&str>,
    ) -> Result<ConfigShowReport, CliError> {
        vulcan_app::config::build_config_show_report_from_overrides(
            paths,
            shared_toml,
            local_toml,
            section,
            selected_permission_profile,
        )
        .map_err(CliError::operation)
    }

    pub(crate) fn build_config_get_report(
        paths: &VaultPaths,
        key: &str,
    ) -> Result<ConfigGetReport, CliError> {
        vulcan_app::config::build_config_get_report(paths, key).map_err(CliError::operation)
    }

    pub(crate) fn build_config_list_report(
        paths: &VaultPaths,
        section: Option<&str>,
    ) -> Result<ConfigListReport, CliError> {
        vulcan_app::config::build_config_list_report(paths, section).map_err(CliError::operation)
    }

    pub(crate) fn build_config_list_report_from_overrides(
        paths: &VaultPaths,
        shared_toml: &TomlValue,
        local_toml: &TomlValue,
        section: Option<&str>,
    ) -> Result<ConfigListReport, CliError> {
        vulcan_app::config::build_config_list_report_from_overrides(
            paths,
            shared_toml,
            local_toml,
            section,
        )
        .map_err(CliError::operation)
    }

    pub(crate) fn config_descriptor_catalog() -> Vec<ConfigDescriptor> {
        vulcan_app::config::config_descriptor_catalog()
    }

    pub(crate) fn plan_config_set_report(
        paths: &VaultPaths,
        key: &str,
        raw_value: &str,
        dry_run: bool,
    ) -> Result<ConfigSetReport, CliError> {
        vulcan_app::config::plan_config_set_report(paths, key, raw_value, dry_run)
            .map_err(CliError::operation)
    }

    pub(crate) fn plan_config_set_report_for_target(
        paths: &VaultPaths,
        key: &str,
        raw_value: &str,
        target: ConfigTarget,
        dry_run: bool,
    ) -> Result<ConfigSetReport, CliError> {
        vulcan_app::config::plan_config_set_report_for_target(
            paths, key, raw_value, target, dry_run,
        )
        .map_err(CliError::operation)
    }

    pub(crate) fn plan_config_set_report_to(
        paths: &VaultPaths,
        key: &str,
        value: &TomlValue,
        target: ConfigTarget,
        dry_run: bool,
    ) -> Result<ConfigSetReport, CliError> {
        vulcan_app::config::plan_config_set_report_to(paths, key, value, target, dry_run)
            .map_err(CliError::operation)
    }

    pub(crate) fn apply_config_set_report(
        paths: &VaultPaths,
        report: ConfigSetReport,
    ) -> Result<ConfigSetReport, CliError> {
        vulcan_app::config::apply_config_set_report(paths, report).map_err(CliError::operation)
    }

    pub(crate) fn plan_config_unset_report(
        paths: &VaultPaths,
        key: &str,
        target: ConfigTarget,
        dry_run: bool,
    ) -> Result<ConfigUnsetReport, CliError> {
        vulcan_app::config::plan_config_unset_report(paths, key, target, dry_run)
            .map_err(CliError::operation)
    }

    pub(crate) fn apply_config_unset_report(
        paths: &VaultPaths,
        report: ConfigUnsetReport,
    ) -> Result<ConfigUnsetReport, CliError> {
        vulcan_app::config::apply_config_unset_report(paths, report).map_err(CliError::operation)
    }

    pub(crate) fn plan_config_batch_report(
        paths: &VaultPaths,
        operations: &[ConfigMutationOperation],
        target: ConfigTarget,
        dry_run: bool,
    ) -> Result<ConfigBatchReport, CliError> {
        vulcan_app::config::plan_config_batch_report(paths, operations, target, dry_run)
            .map_err(CliError::operation)
    }

    pub(crate) fn apply_config_batch_report(
        paths: &VaultPaths,
        report: ConfigBatchReport,
    ) -> Result<ConfigBatchReport, CliError> {
        vulcan_app::config::apply_config_batch_report(paths, report).map_err(CliError::operation)
    }

    pub(crate) fn plan_config_document_save_for_target(
        paths: &VaultPaths,
        rendered_contents: &str,
        target: ConfigTarget,
    ) -> Result<ConfigDocumentSaveReport, CliError> {
        vulcan_app::config::plan_config_document_save_for_target(paths, rendered_contents, target)
            .map_err(CliError::operation)
    }

    pub(crate) fn apply_config_document_save(
        paths: &VaultPaths,
        report: ConfigDocumentSaveReport,
    ) -> Result<ConfigDocumentSaveReport, CliError> {
        vulcan_app::config::apply_config_document_save(paths, report).map_err(CliError::operation)
    }

    pub(crate) fn load_config_file_toml(path: &Path) -> Result<TomlValue, CliError> {
        vulcan_app::config::load_config_file_toml(path).map_err(CliError::operation)
    }

    pub(crate) fn set_config_toml_value(
        config: &mut TomlValue,
        path: &[&str],
        value: TomlValue,
    ) -> Result<(), CliError> {
        vulcan_app::config::set_config_toml_value(config, path, value).map_err(CliError::operation)
    }

    pub(crate) fn remove_config_toml_value(
        config: &mut TomlValue,
        path: &[&str],
    ) -> Result<bool, CliError> {
        vulcan_app::config::remove_config_toml_value(config, path).map_err(CliError::operation)
    }
}

pub use cli::{
    AgentCommand, AgentImportArgs, AgentInstallArgs, AgentPrintConfigArgs, AgentRuntimeArg,
    AutomationCommand, BasesCommand, CacheCommand, CheckpointCommand, Cli, ColorMode, Command,
    ConfigAliasCommand, ConfigCommand, ConfigImportArgs, ConfigImportCommand,
    ConfigImportSelection, ConfigPermissionsCommand, ConfigPermissionsProfileCommand,
    ConfigTargetArg, DailyCommand, DataviewCommand, DescribeFormatArg, EpubTocStyle, ExportArgs,
    ExportCommand, ExportFormat, ExportProfileCommand, ExportProfileFormatArg,
    ExportProfileRuleCommand, ExportQueryArgs, ExportTransformArgs, GitCommand, GraphCommand,
    GraphExportFormat, IndexCommand, InitArgs, KanbanCommand, McpToolPackArg, McpToolPackModeArg,
    McpTransportArg, NoteAppendPeriodicArg, NoteCheckboxState, NoteCommand, NoteGetMode,
    OutputFormat, PeriodicOpenArgs, PeriodicSubcommand, PluginCommand, PluginEventArg,
    PluginSandboxArg, PropertySortArg, QueryEngineArg, QueryFormatArg, RefactorCommand,
    RefreshMode, RenderArgs, RenderMode, RepairCommand, SavedCommand, SavedCreateCommand,
    SearchBackendArg, SearchMode, SearchSortArg, SiteCommand, SkillCommand, SuggestCommand,
    SuggestLinkStatusArg, TagSortArg, TasksCommand, TasksListSourceArg, TasksPomodoroCommand,
    TasksTrackCommand, TasksTrackSummaryPeriodArg, TasksViewCommand, TemplateEngineArg,
    TemplateRenderArgs, TemplateSubcommand, ToolCommand, ToolInitTemplateArg, TrustCommand,
    VectorQueueCommand, VectorsCommand, WebCommand, WebFetchMode,
};

use crate::commands::config::{
    discover_config_importers, normalize_config_import_report, normalize_import_discovery_item,
    ConfigImportBatchReport, ConfigImportDiscoveryItem,
};
use crate::commit::AutoCommitPolicy;
use crate::help::{
    builtin_help_topic, builtin_help_topics, help_overview, HelpSearchMatch, HelpSearchReport,
    HelpTopicKind, HelpTopicReport,
};
use crate::output::{
    markdown_table_column_count, markdown_table_header_lines, markdown_table_row, paginated_items,
    print_json, print_json_lines, print_selected_human_fields, render_dataview_inline_value,
    render_human_value, select_fields, ListOutputControls,
};
use crate::resolve::{interactive_note_selection_allowed, resolve_note_argument};
use bundle_server::{serve_frontend_bundle_profile, FrontendBundleServeOptions};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use clap_complete::generate;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use serve::{serve_forever, ServeOptions};
use site_server::{build_site_with_policy_and_progress, spawn_site_server, SiteServeOptions};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fmt::{Display, Formatter, Write as FmtWrite};
use std::fs;
use std::io;
use std::io::{IsTerminal, Read};
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};
use toml::Value as TomlValue;
use vulcan_app::browse::{
    build_vault_status_report as app_build_vault_status_report,
    collect_complete_candidates as app_collect_complete_candidates,
    DataviewBlockResult as AppDataviewBlockResult, DataviewEvalReport as AppDataviewEvalReport,
    VaultStatusReport as AppVaultStatusReport,
};
use vulcan_app::export::{
    apply_export_profile_create, apply_export_profile_delete, apply_export_profile_rule_add,
    apply_export_profile_rule_delete, apply_export_profile_rule_move,
    apply_export_profile_rule_update, apply_export_profile_set, build_content_transform_rules,
    build_export_profile_list, build_export_profile_rule_list, build_export_profile_show_report,
    execute_export_query, export_profile_format_label, export_profile_query_args,
    load_export_links, load_exported_notes, prepare_export_data, render_csv_export_payload,
    render_json_export_payload, render_markdown_export_payload, require_export_profile_format,
    require_export_profile_path, validate_export_profile_config,
    write_epub_export as app_write_epub_export, write_sqlite_export, write_zip_export,
    BoolConfigUpdate, ConfigValueUpdate, CsvExportSummary,
    EpubExportOptions as AppEpubExportOptions, EpubRenderCallbacks as AppEpubRenderCallbacks,
    ExportProfileCreateRequest, ExportProfileDeleteReport, ExportProfileListEntry,
    ExportProfileRuleListEntry, ExportProfileRuleMoveRequest, ExportProfileRuleRequest,
    ExportProfileRuleWriteAction, ExportProfileRuleWriteReport, ExportProfileSetRequest,
    ExportProfileShowReport, ExportProfileWriteAction, ExportProfileWriteReport, JsonExportSummary,
    MarkdownExportSummary,
};
use vulcan_app::notes::json_properties_to_frontmatter;
use vulcan_app::scan::refresh_cache_incrementally_with_progress;
use vulcan_app::site::{
    build_frontend_bundle as app_build_frontend_bundle,
    build_site_doctor_report as app_build_site_doctor_report,
    build_site_profiles_report as app_build_site_profiles_report, FrontendBundleRequest,
    SiteBuildPhase, SiteBuildProgress, SiteBuildReport, SiteBuildRequest, SiteDoctorReport,
    SiteProfileListEntry,
};
#[cfg(test)]
use vulcan_app::templates::{
    list_templates_in_directory, prepare_template_insertion, render_template_variable,
    resolve_template_file, template_variables_for_path, TemplateCandidate, TemplateTimestamp,
};
use vulcan_app::templates::{
    load_named_template, merge_template_frontmatter, parse_frontmatter_document,
    render_loaded_template, render_note_from_parts, LoadedTemplateRenderRequest,
    TemplateEngineKind, TemplateInsertMode, TemplateRunMode, TemplateVariables,
};
#[cfg(test)]
use vulcan_core::config::TemplatesConfig;
use vulcan_core::config::{
    ContentTransformRuleConfig, ExportEpubTocStyleConfig, ExportGraphFormatConfig,
    ExportProfileConfig, ExportProfileFormat,
};
use vulcan_core::expression::functions::date_components;
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::{
    all_importers, annotate_import_conflicts, bulk_replace, cache_vacuum, create_checkpoint,
    default_assistant_tool_reserved_names, delete_saved_report, doctor_fix, doctor_vault,
    evaluate_base_file, evaluate_dql_with_filter, export_static_search_index, initialize_vault,
    inspect_cache, link_mentions, list_checkpoints, list_saved_reports, load_saved_report,
    load_vault_config, merge_tags, move_note, plan_base_note_create, query_change_report,
    query_notes, rebuild_vault_with_progress, rename_alias, rename_block_ref, rename_heading,
    rename_property, render_note_html, render_vault_html, repair_fts, resolve_note_reference,
    resolve_permission_profile, save_saved_report, scan_vault_with_progress, search_vault,
    verify_cache, watch_vault, AutoScanMode, BacklinkRecord, BacklinksReport, BasesCreateContext,
    BasesEvalReport, BasesViewEditReport, BulkMutationReport, CacheDatabase, CacheInspectReport,
    CacheVacuumQuery, CacheVacuumReport, CacheVerifyReport, ChangeAnchor, ChangeItem, ChangeKind,
    ChangeReport, CheckpointRecord, ClusterReport, DataviewJsOutput, DataviewJsResult,
    DoctorDiagnosticIssue, DoctorFixReport, DoctorLinkIssue, DoctorReport, DqlQueryResult,
    DuplicateSuggestionsReport, GraphAnalyticsReport, GraphCommunitiesReport,
    GraphComponentsReport, GraphConfidenceBreakdown, GraphDeadEndsReport, GraphHubsReport,
    GraphMocCandidate, GraphMocReport, GraphPathReport, GraphTrendsReport, HtmlRenderOptions,
    ImportTarget, InitSummary, LinkSuggestion, LinkSuggestionsReport, MentionSuggestion,
    MentionSuggestionsReport, MergeCandidate, MoveSummary, NamedCount, NoteQuery, NoteRecord,
    NotesReport, OutgoingLinkRecord, OutgoingLinksReport, PermissionFilter, PermissionGuard,
    PluginEvent, ProfilePermissionGuard, QueryReport, RebuildQuery, RebuildReport, RefactorChange,
    RefactorReport, RelatedNoteHit, RelatedNotesReport, RepairFtsQuery, RepairFtsReport,
    ResolvedPermissionProfile, SavedExport, SavedExportFormat, SavedReportDefinition,
    SavedReportKind, SavedReportQuery, SavedReportSummary, ScanMode, ScanPhase, ScanProgress,
    ScanSummary, SearchHit, SearchQuery, SearchReport, SearchSort, StoredModelInfo, VaultPaths,
    VectorDuplicatePair, VectorDuplicatesReport, VectorIndexPhase, VectorIndexProgress,
    VectorIndexReport, VectorNeighborHit, VectorNeighborsReport, VectorQueueReport,
    VectorRepairReport, WatchOptions, WatchReport,
};
#[derive(Debug)]
pub struct CliError {
    exit_code: u8,
    code: &'static str,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BulkNoteSelection {
    Filters(Vec<String>),
    Paths(Vec<String>),
}

impl CliError {
    pub(crate) fn io(error: &io::Error) -> Self {
        Self {
            exit_code: 1,
            code: "io_error",
            message: format!("failed to read current working directory: {error}"),
        }
    }

    pub(crate) fn operation(error: impl Display) -> Self {
        Self {
            exit_code: 1,
            code: "operation_failed",
            message: error.to_string(),
        }
    }

    pub(crate) fn issues(message: impl Into<String>) -> Self {
        Self {
            exit_code: 2,
            code: "issues_detected",
            message: message.into(),
        }
    }

    /// Exit code 2, no message — used by `--exit-code` when query/search returns zero results.
    pub(crate) fn no_results() -> Self {
        Self {
            exit_code: 2,
            code: "no_results",
            message: String::new(),
        }
    }

    pub(crate) fn clap(error: &clap::Error) -> Self {
        Self {
            exit_code: 2,
            code: "invalid_arguments",
            message: error.to_string(),
        }
    }

    #[must_use]
    pub fn exit_code(&self) -> u8 {
        self.exit_code
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

impl From<vulcan_app::AppError> for CliError {
    fn from(error: vulcan_app::AppError) -> Self {
        Self::operation(error)
    }
}

fn permission_error_to_cli(error: impl Display) -> CliError {
    CliError::operation(error)
}

pub(crate) fn selected_permission_profile(
    cli: &Cli,
    paths: &VaultPaths,
) -> Result<ResolvedPermissionProfile, CliError> {
    resolve_permission_profile(paths, cli.permissions.as_deref()).map_err(permission_error_to_cli)
}

pub(crate) fn selected_permission_guard(
    cli: &Cli,
    paths: &VaultPaths,
) -> Result<ProfilePermissionGuard, CliError> {
    selected_permission_profile(cli, paths)
        .map(|selection| ProfilePermissionGuard::new(paths, selection))
}

pub(crate) fn selected_read_permission_filter(
    cli: &Cli,
    paths: &VaultPaths,
) -> Result<Option<PermissionFilter>, CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    let filter = guard.read_filter();
    Ok((!filter.path_permission().is_unrestricted()).then_some(filter))
}

const SCAN_PROGRESS_STEP: usize = 250;

struct BundledTextFile {
    kind: &'static str,
    relative_path: &'static str,
    contents: &'static str,
    target: BundledFileTarget,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BundledFileTarget {
    VaultRoot,
    SkillsFolder,
    PromptsFolder,
}

const BUNDLED_AGENT_TEMPLATE: BundledTextFile = BundledTextFile {
    kind: "agents_template",
    relative_path: "AGENTS.md",
    contents: include_str!("../../docs/assistant/AGENTS.template.md"),
    target: BundledFileTarget::VaultRoot,
};

const BUNDLED_SKILL_FILES: &[BundledTextFile] = &[
    BundledTextFile {
        kind: "skill",
        relative_path: "note-operations/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/note-operations.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "vault-query/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/vault-query.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "js-api-guide/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/js-api-guide.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "skill-creator/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/skill-creator.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "graph-exploration/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/graph-exploration.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "link-curation/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/link-curation.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "daily-notes/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/daily-notes.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "properties-and-tags/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/properties-and-tags.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "refactoring/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/refactoring.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "web-research/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/web-research.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "git-workflow/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/git-workflow.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "task-management/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/task-management.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "configuration-and-permissions/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/configuration-and-permissions.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "mcp-setup/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/mcp-setup.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "index-maintenance/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/index-maintenance.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "dataview-and-bases/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/dataview-and-bases.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "templates-and-capture/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/templates-and-capture.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "publishing-and-export/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/publishing-and-export.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "plugin-authoring/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/plugin-authoring.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "diagnostics-and-repair/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/diagnostics-and-repair.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "conversation-export/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/conversation-export.md"),
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "conversation-export/scripts/export-conversation.js",
        contents: include_str!(
            "../../docs/assistant/skills/conversation-export/export-conversation.js"
        ),
        target: BundledFileTarget::SkillsFolder,
    },
];

const BUNDLED_PROMPT_FILES: &[BundledTextFile] = &[
    BundledTextFile {
        kind: "prompt",
        relative_path: "summarize-note.md",
        contents: include_str!("../../docs/assistant/prompts/summarize-note.md"),
        target: BundledFileTarget::PromptsFolder,
    },
    BundledTextFile {
        kind: "prompt",
        relative_path: "daily-review.md",
        contents: include_str!("../../docs/assistant/prompts/daily-review.md"),
        target: BundledFileTarget::PromptsFolder,
    },
];

const BUNDLED_TOOL_FILES: &[BundledTextFile] = &[
    BundledTextFile {
        kind: "skill",
        relative_path: "summarize-note/SKILL.md",
        contents: r"---
name: summarize-note
description: Summarize one vault note.
license: UNLICENSED
compatibility:
  - vulcan
allowed-tools:
  - note_get
metadata:
  vulcan:
    commands:
      - id: summarize
        script: scripts/summarize.js
        sandbox: fs
        packs: [custom]
        expose: true
        input_schema:
          type: object
          required: [note]
          properties:
            note:
              type: string
        output_schema:
          type: object
          required: [note, summary]
          properties:
            note:
              type: string
            summary:
              type: string
---

# Summarize Note

Use this skill command as a minimal Agent Skills-compatible executable example.
",
        target: BundledFileTarget::SkillsFolder,
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "summarize-note/scripts/summarize.js",
        contents: "#!/usr/bin/env -S vulcan skill exec\nfunction main(input) {\n  return {\n    note: input.note,\n    summary: `TODO: summarize ${input.note}`,\n  };\n}\n",
        target: BundledFileTarget::SkillsFolder,
    },
];

pub(crate) fn bundled_support_relative_paths(paths: &VaultPaths) -> Vec<String> {
    std::iter::once(&BUNDLED_AGENT_TEMPLATE)
        .chain(BUNDLED_SKILL_FILES.iter())
        .chain(BUNDLED_PROMPT_FILES.iter())
        .map(|file| bundled_text_file_display_path(paths, file))
        .collect()
}

#[derive(Clone, Copy)]
enum RefreshTarget {
    Command,
    Browse,
}

#[derive(Clone, Copy)]
pub(crate) struct AnsiPalette {
    enabled: bool,
}

impl AnsiPalette {
    pub(crate) fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    pub(crate) fn bold(self, text: &str) -> String {
        self.wrap("1", text)
    }

    pub(crate) fn cyan(self, text: &str) -> String {
        self.wrap("36", text)
    }

    fn green(self, text: &str) -> String {
        self.wrap("32", text)
    }

    fn yellow(self, text: &str) -> String {
        self.wrap("33", text)
    }

    fn red(self, text: &str) -> String {
        self.wrap("31", text)
    }

    fn dim(self, text: &str) -> String {
        self.wrap("2", text)
    }

    fn wrap(self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

struct ScanProgressReporter {
    palette: AnsiPalette,
    started_at: Instant,
    last_phase: Option<ScanPhase>,
    next_checkpoint: usize,
}

impl ScanProgressReporter {
    fn new(use_color: bool) -> Self {
        Self {
            palette: AnsiPalette::new(use_color),
            started_at: Instant::now(),
            last_phase: None,
            next_checkpoint: SCAN_PROGRESS_STEP,
        }
    }

    fn record(&mut self, progress: &ScanProgress) {
        match progress.phase {
            ScanPhase::PreparingFiles => {
                if self.last_phase != Some(progress.phase) {
                    eprintln!(
                        "{} {} files for a {} scan...",
                        self.palette.cyan("Preparing"),
                        progress.discovered,
                        match progress.mode {
                            ScanMode::Full => "full",
                            ScanMode::Incremental => "incremental",
                        }
                    );
                    self.last_phase = Some(progress.phase);
                }
            }
            ScanPhase::ScanningFiles => {
                if progress.processed == 0 {
                    eprintln!(
                        "{} {} files; running {} scan...",
                        self.palette.cyan("Discovered"),
                        progress.discovered,
                        self.palette.bold(match progress.mode {
                            ScanMode::Full => "full",
                            ScanMode::Incremental => "incremental",
                        })
                    );
                    self.last_phase = Some(progress.phase);
                    self.next_checkpoint = SCAN_PROGRESS_STEP.min(progress.discovered.max(1));
                    return;
                }

                if progress.processed >= self.next_checkpoint
                    || progress.processed == progress.discovered
                {
                    let elapsed = self.started_at.elapsed();
                    let rate =
                        count_as_f64(progress.processed) / elapsed.as_secs_f64().max(f64::EPSILON);
                    let remaining = progress.discovered.saturating_sub(progress.processed);
                    eprintln!(
                        "{} {}/{} files: {} added, {} updated, {} unchanged, {} deleted | {} | {}",
                        self.palette.cyan("Scanned"),
                        self.palette.bold(&progress.processed.to_string()),
                        progress.discovered,
                        self.palette.green(&progress.added.to_string()),
                        self.palette.yellow(&progress.updated.to_string()),
                        progress.unchanged,
                        self.palette.red(&progress.deleted.to_string()),
                        self.palette.dim(&format!("{rate:.0} files/s")),
                        self.palette
                            .dim(&format!("ETA {}", format_eta(remaining, rate)))
                    );
                    while self.next_checkpoint <= progress.processed {
                        self.next_checkpoint += SCAN_PROGRESS_STEP;
                    }
                }
            }
            ScanPhase::RefreshingPropertyCatalog | ScanPhase::ResolvingLinks => {
                if self.last_phase != Some(progress.phase) {
                    eprintln!(
                        "{}...",
                        self.palette.cyan(match progress.phase {
                            ScanPhase::RefreshingPropertyCatalog => "Refreshing property catalog",
                            ScanPhase::ResolvingLinks => "Resolving links",
                            ScanPhase::PreparingFiles
                            | ScanPhase::ScanningFiles
                            | ScanPhase::Completed => unreachable!(),
                        })
                    );
                    self.last_phase = Some(progress.phase);
                }
            }
            ScanPhase::Completed => {}
        }
    }
}

struct SiteBuildProgressReporter {
    palette: AnsiPalette,
    started_at: Instant,
    last_phase: Option<SiteBuildPhase>,
    next_checkpoint: usize,
}

impl SiteBuildProgressReporter {
    fn new(use_color: bool) -> Self {
        Self {
            palette: AnsiPalette::new(use_color),
            started_at: Instant::now(),
            last_phase: None,
            next_checkpoint: 25,
        }
    }

    fn record(&mut self, progress: &SiteBuildProgress) {
        match progress.phase {
            SiteBuildPhase::Planning => {
                if self.last_phase != Some(progress.phase) {
                    eprintln!("{}", self.palette.cyan("Planning site build..."));
                    self.last_phase = Some(progress.phase);
                }
            }
            SiteBuildPhase::RenderingNotes => {
                if progress.processed == 0 {
                    eprintln!(
                        "{} {} note(s)...",
                        self.palette.cyan("Rendering"),
                        self.palette.bold(&progress.total.to_string()),
                    );
                    self.last_phase = Some(progress.phase);
                    self.next_checkpoint = 25.min(progress.total.max(1));
                    return;
                }

                if progress.processed >= self.next_checkpoint
                    || progress.processed == progress.total
                    || progress.total <= 10
                {
                    let elapsed = self.started_at.elapsed();
                    let rate =
                        count_as_f64(progress.processed) / elapsed.as_secs_f64().max(f64::EPSILON);
                    let current = progress.current_path.as_deref().unwrap_or_default();
                    eprintln!(
                        "{} {}/{} note(s) | {}{}",
                        self.palette.cyan("Rendered"),
                        self.palette.bold(&progress.processed.to_string()),
                        progress.total,
                        self.palette.dim(&format!("{rate:.1} notes/s")),
                        if current.is_empty() {
                            String::new()
                        } else {
                            format!(" | {}", self.palette.dim(current))
                        }
                    );
                    while self.next_checkpoint <= progress.processed {
                        self.next_checkpoint += 25;
                    }
                }
            }
            SiteBuildPhase::CopyingAssets => {
                self.report_stage_once(
                    "Writing note pages and copying static assets...",
                    progress.phase,
                );
            }
            SiteBuildPhase::WritingSearchIndex => {
                self.report_stage_once("Building search index...", progress.phase);
            }
            SiteBuildPhase::WritingGraph => {
                self.report_stage_once("Building graph export...", progress.phase);
            }
            SiteBuildPhase::WritingPages => {
                self.report_stage_once("Writing pages and manifests...", progress.phase);
            }
            SiteBuildPhase::Finalizing => {
                self.report_stage_once("Finalizing site output...", progress.phase);
            }
        }
    }

    fn report_stage_once(&mut self, message: &str, phase: SiteBuildPhase) {
        if self.last_phase != Some(phase) {
            eprintln!("{}", self.palette.cyan(message));
            self.last_phase = Some(phase);
        }
    }
}

struct VectorIndexProgressReporter {
    palette: AnsiPalette,
    started_at: Instant,
    last_batches_completed: usize,
    prepared: bool,
    verbose: bool,
}

impl VectorIndexProgressReporter {
    fn new(use_color: bool, verbose: bool) -> Self {
        Self {
            palette: AnsiPalette::new(use_color),
            started_at: Instant::now(),
            last_batches_completed: 0,
            prepared: false,
            verbose,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn record(&mut self, progress: &VectorIndexProgress) {
        match progress.phase {
            VectorIndexPhase::Preparing => {
                if self.prepared {
                    return;
                }
                self.prepared = true;
                if self.verbose {
                    eprintln!(
                        "{} {}:{}",
                        self.palette.dim("Provider:"),
                        progress.provider_name,
                        progress.model_name
                    );
                    eprintln!(
                        "{} {}",
                        self.palette.dim("Endpoint:"),
                        progress.endpoint_url
                    );
                    let key_status = match (&progress.api_key_env, progress.api_key_set) {
                        (Some(env_var), true) => format!("set (from ${env_var})"),
                        (Some(env_var), false) => format!("NOT SET (expected ${env_var})"),
                        (None, _) => "none configured".to_string(),
                    };
                    eprintln!(
                        "{} {}",
                        self.palette.dim("API key: "),
                        if progress.api_key_set {
                            key_status
                        } else {
                            self.palette.red(&key_status).clone()
                        }
                    );
                }
                if progress.pending == 0 {
                    eprintln!(
                        "{} for {}:{} {}",
                        self.palette.cyan("Vector index is up to date"),
                        progress.provider_name,
                        progress.model_name,
                        self.palette.dim(&format!(
                            "(batch size {}, concurrency {}, {} skipped)",
                            progress.batch_size, progress.max_concurrency, progress.skipped
                        ))
                    );
                } else {
                    eprintln!(
                        "{} {} vector chunks with {}:{} {}",
                        self.palette.cyan("Indexing"),
                        self.palette.bold(&progress.pending.to_string()),
                        progress.provider_name,
                        progress.model_name,
                        self.palette.dim(&format!(
                            "(batch size {}, concurrency {}, {} batches)",
                            progress.batch_size, progress.max_concurrency, progress.total_batches
                        ))
                    );
                }
            }
            VectorIndexPhase::Embedding => {
                if progress.batches_completed > self.last_batches_completed {
                    let elapsed = self.started_at.elapsed();
                    let rate =
                        count_as_f64(progress.processed) / elapsed.as_secs_f64().max(f64::EPSILON);
                    let remaining = progress.pending.saturating_sub(progress.processed);
                    eprintln!(
                        "{} {}/{}: {} indexed, {} failed, {} remaining | {} | {}",
                        self.palette.cyan("Completed batch"),
                        self.palette.bold(&progress.batches_completed.to_string()),
                        progress.total_batches.max(1),
                        self.palette.green(&progress.indexed.to_string()),
                        self.palette.red(&progress.failed.to_string()),
                        remaining,
                        self.palette.dim(&format!("{rate:.1} chunks/s")),
                        self.palette
                            .dim(&format!("ETA {}", format_eta(remaining, rate)))
                    );
                    self.last_batches_completed = progress.batches_completed;
                }
                if !progress.batch_failures.is_empty() {
                    let deduped = dedup_failure_messages(&progress.batch_failures);
                    if self.verbose {
                        for (message, count) in &deduped {
                            eprintln!(
                                "  {} {} {}",
                                self.palette.red("FAIL"),
                                message,
                                self.palette.dim(&format!("({count} chunks)"))
                            );
                        }
                    } else if progress.batches_completed == 1 {
                        let (message, count) = &deduped[0];
                        eprintln!(
                            "  {} {} {}",
                            self.palette.red("FAIL"),
                            message,
                            self.palette.dim(&format!(
                                "({count} chunks, use --verbose to see all failures)"
                            ))
                        );
                    }
                }
            }
            VectorIndexPhase::Completed => {
                if progress.dry_run {
                    eprintln!(
                        "{} {} vector chunks across {} batches",
                        self.palette.cyan("Dry run planned"),
                        self.palette.bold(&progress.pending.to_string()),
                        progress.total_batches
                    );
                } else if !self.prepared {
                    eprintln!(
                        "{} {} indexed, {} failed, {} skipped {}",
                        self.palette.cyan("Vector index complete"),
                        self.palette.green(&progress.indexed.to_string()),
                        self.palette.red(&progress.failed.to_string()),
                        progress.skipped,
                        self.palette.dim(&format!(
                            "in {:.3}s",
                            self.started_at.elapsed().as_secs_f64()
                        ))
                    );
                }
            }
        }
    }
}

fn dedup_failure_messages(failures: &[(String, String, String)]) -> Vec<(&str, usize)> {
    let mut seen = Vec::new();
    for (_, _, message) in failures {
        if let Some((_, count)) = seen
            .iter_mut()
            .find(|(m, _): &&mut (&str, usize)| *m == message)
        {
            *count += 1;
        } else {
            seen.push((message.as_str(), 1));
        }
    }
    seen
}

fn color_enabled_for_terminal(is_tty: bool) -> bool {
    is_tty
        && std::env::var_os("NO_COLOR").is_none()
        && !std::env::var("TERM").is_ok_and(|value| value == "dumb")
}

fn resolve_use_color(mode: ColorMode, is_tty: bool) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => color_enabled_for_terminal(is_tty),
    }
}

fn format_eta(remaining_units: usize, rate_per_second: f64) -> String {
    if remaining_units == 0 || rate_per_second <= f64::EPSILON {
        return "0s".to_string();
    }

    format_duration(Duration::from_secs_f64(
        count_as_f64(remaining_units) / rate_per_second,
    ))
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 1.0 {
        "<1s".to_string()
    } else if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else if seconds < 3_600.0 {
        let minutes = (seconds / 60.0).floor();
        let remaining = seconds - (minutes * 60.0);
        format!("{minutes:.0}m {remaining:.0}s")
    } else {
        let hours = (seconds / 3_600.0).floor();
        let minutes = ((seconds - (hours * 3_600.0)) / 60.0).floor();
        format!("{hours:.0}h {minutes:.0}m")
    }
}

fn count_as_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedExport {
    format: SavedExportFormat,
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BatchRunItemReport {
    name: String,
    kind: Option<SavedReportKind>,
    ok: bool,
    row_count: Option<usize>,
    export_format: Option<SavedExportFormat>,
    export_path: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BatchRunReport {
    total: usize,
    succeeded: usize,
    failed: usize,
    items: Vec<BatchRunItemReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AutomationRunReport {
    actions: Vec<String>,
    reports: Option<BatchRunReport>,
    scan: Option<ScanSummary>,
    doctor_issues: Option<vulcan_core::DoctorSummary>,
    doctor_fix: Option<DoctorFixReport>,
    cache_verify: Option<CacheVerifyReport>,
    repair_fts: Option<RepairFtsReport>,
    issues_detected: bool,
}

type DataviewEvalReport = AppDataviewEvalReport;
type DataviewBlockResult = AppDataviewBlockResult;
type VaultStatusReport = AppVaultStatusReport;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RenderReport {
    path: Option<String>,
    source: String,
    rendered: String,
    mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NoteEntryInsertion {
    updated: String,
    line_number: i64,
    change: RefactorChange,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct BasesCreateReport {
    pub(crate) file: String,
    pub(crate) view_name: Option<String>,
    pub(crate) view_index: usize,
    pub(crate) dry_run: bool,
    pub(crate) path: String,
    pub(crate) folder: Option<String>,
    pub(crate) template: Option<String>,
    pub(crate) properties: BTreeMap<String, Value>,
    pub(crate) filters: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct OpenReport {
    path: String,
    uri: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SupportFileStatus {
    Created,
    Updated,
    Kept,
}

impl SupportFileStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Kept => "kept",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SupportFileReport {
    path: String,
    kind: String,
    status: SupportFileStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct InitReport {
    #[serde(flatten)]
    summary: InitSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    importable_sources: Vec<ConfigImportDiscoveryItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    support_files: Vec<SupportFileReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    imported: Option<ConfigImportBatchReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentInstallReport {
    support_files: Vec<SupportFileReport>,
}

#[allow(clippy::large_enum_variant)]
enum SavedExecution {
    Search(SearchReport),
    Notes(NotesReport),
    Bases(BasesEvalReport),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SavedReportDeleteReport {
    name: String,
    path: PathBuf,
    deleted: bool,
}

impl SavedExecution {
    fn kind(&self) -> SavedReportKind {
        match self {
            Self::Search(_) => SavedReportKind::Search,
            Self::Notes(_) => SavedReportKind::Notes,
            Self::Bases(_) => SavedReportKind::Bases,
        }
    }
}

fn stored_export_from_args(export: &ExportArgs) -> Result<Option<SavedExport>, CliError> {
    match (export.export, export.export_path.as_ref()) {
        (Some(format), Some(path)) => Ok(Some(SavedExport {
            format: saved_export_format(format),
            path: path_to_string(path)?,
        })),
        (None, None) => Ok(None),
        _ => Err(CliError::operation(
            "export flags require both --export and --export-path",
        )),
    }
}

fn saved_report_definition_from_create(
    cli: &Cli,
    command: &SavedCreateCommand,
) -> Result<SavedReportDefinition, CliError> {
    Ok(match command {
        SavedCreateCommand::Search {
            name,
            query,
            filters,
            mode,
            tag,
            path_prefix,
            has_property,
            sort,
            match_case,
            context_size,
            raw_query,
            fuzzy,
            description,
            export,
        } => SavedReportDefinition {
            name: name.clone(),
            description: description.clone(),
            fields: cli.fields.clone(),
            limit: cli.limit,
            export: stored_export_from_args(export)?,
            query: SavedReportQuery::Search {
                query: query.clone(),
                mode: cli_search_mode(*mode),
                tag: tag.clone(),
                path_prefix: path_prefix.clone(),
                has_property: has_property.clone(),
                filters: filters.clone(),
                context_size: *context_size,
                sort: sort.map(cli_search_sort),
                match_case: match_case.then_some(true),
                raw_query: *raw_query,
                fuzzy: *fuzzy,
            },
        },
        SavedCreateCommand::Notes {
            name,
            filters,
            sort,
            desc,
            description,
            export,
        } => SavedReportDefinition {
            name: name.clone(),
            description: description.clone(),
            fields: cli.fields.clone(),
            limit: cli.limit,
            export: stored_export_from_args(export)?,
            query: SavedReportQuery::Notes {
                filters: filters.clone(),
                sort_by: sort.clone(),
                sort_descending: *desc,
            },
        },
        SavedCreateCommand::Bases {
            name,
            file,
            description,
            export,
        } => SavedReportDefinition {
            name: name.clone(),
            description: description.clone(),
            fields: cli.fields.clone(),
            limit: cli.limit,
            export: stored_export_from_args(export)?,
            query: SavedReportQuery::Bases { file: file.clone() },
        },
    })
}

fn resolve_cli_export(export: &ExportArgs) -> Result<Option<ResolvedExport>, CliError> {
    match (export.export, export.export_path.as_ref()) {
        (Some(format), Some(path)) => Ok(Some(ResolvedExport {
            format: saved_export_format(format),
            path: resolve_relative_output_path(
                path,
                &std::env::current_dir().map_err(|error| CliError::io(&error))?,
            ),
        })),
        (None, None) => Ok(None),
        _ => Err(CliError::operation(
            "export flags require both --export and --export-path",
        )),
    }
}

fn resolve_saved_export(paths: &VaultPaths, export: &SavedExport) -> ResolvedExport {
    ResolvedExport {
        format: export.format,
        path: resolve_relative_output_path(Path::new(&export.path), paths.vault_root()),
    }
}

fn resolve_runtime_export(
    paths: &VaultPaths,
    definition: &SavedReportDefinition,
    override_export: &ExportArgs,
) -> Result<Option<ResolvedExport>, CliError> {
    if let Some(export) = resolve_cli_export(override_export)? {
        return Ok(Some(export));
    }

    definition
        .export
        .as_ref()
        .map(|export| Ok(resolve_saved_export(paths, export)))
        .transpose()
}

fn resolve_relative_output_path(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn path_to_string(path: &Path) -> Result<String, CliError> {
    path.to_str()
        .map(ToString::to_string)
        .ok_or_else(|| CliError::operation("export paths must be valid UTF-8"))
}

fn run_incremental_scan(
    paths: &VaultPaths,
    output: OutputFormat,
    use_stderr_color: bool,
    quiet: bool,
) -> Result<ScanSummary, CliError> {
    let mut progress = (output == OutputFormat::Human && !quiet)
        .then(|| ScanProgressReporter::new(use_stderr_color));
    refresh_cache_incrementally_with_progress(paths, |event| {
        if let Some(progress) = progress.as_mut() {
            progress.record(&event);
        }
    })
    .map_err(CliError::operation)
}

fn refresh_mode_for_target(paths: &VaultPaths, cli: &Cli, target: RefreshTarget) -> AutoScanMode {
    if let Some(mode) = cli.refresh {
        return match mode {
            RefreshMode::Off => AutoScanMode::Off,
            RefreshMode::Blocking => AutoScanMode::Blocking,
            RefreshMode::Background => AutoScanMode::Background,
        };
    }

    let scan_config = load_vault_config(paths).config.scan;
    match target {
        RefreshTarget::Command => scan_config.default_mode,
        RefreshTarget::Browse => scan_config.browse_mode,
    }
}

fn command_uses_auto_refresh(command: &Command) -> bool {
    match command {
        Command::Backlinks { .. }
        | Command::Graph { .. }
        | Command::Open { .. }
        | Command::Doctor { .. }
        | Command::Move { .. }
        | Command::RenameProperty { .. }
        | Command::MergeTags { .. }
        | Command::RenameAlias { .. }
        | Command::RenameHeading { .. }
        | Command::RenameBlockRef { .. }
        | Command::Links { .. }
        | Command::Ls { .. }
        | Command::Tags { .. }
        | Command::Properties { .. }
        | Command::Query { .. }
        | Command::Dataview { .. }
        | Command::Tasks { .. }
        | Command::Kanban { .. }
        | Command::Update { .. }
        | Command::Unset { .. }
        | Command::Search { .. }
        | Command::Changes { .. }
        | Command::Diff { .. }
        | Command::LinkMentions { .. }
        | Command::Rewrite { .. }
        | Command::Suggest { .. }
        | Command::Refactor { .. }
        | Command::Checkpoint { .. }
        | Command::Export { .. }
        | Command::Site { .. } => true,
        Command::Daily { command } => matches!(
            command,
            DailyCommand::Show { .. } | DailyCommand::List { .. } | DailyCommand::ExportIcs { .. }
        ),
        Command::Periodic { command, .. } => {
            matches!(command, Some(PeriodicSubcommand::List { .. }))
        }
        Command::Edit { new, .. } => !new,
        Command::Bases { command } => matches!(
            command,
            BasesCommand::Eval { .. } | BasesCommand::Tui { .. }
        ),
        Command::Saved { command } => matches!(command, SavedCommand::Run { .. }),
        Command::Vectors { command } => matches!(
            command,
            VectorsCommand::Cluster { .. }
                | VectorsCommand::Related { .. }
                | VectorsCommand::Neighbors { .. }
                | VectorsCommand::Duplicates { .. }
        ),
        Command::Template { command, .. } => {
            matches!(command, Some(TemplateSubcommand::Insert { .. }))
        }
        Command::Note { command } => matches!(
            command,
            NoteCommand::Links { .. }
                | NoteCommand::Backlinks { .. }
                | NoteCommand::Update { .. }
                | NoteCommand::Unset { .. }
                | NoteCommand::Delete { .. }
                | NoteCommand::Info { .. }
                | NoteCommand::Doctor { .. }
                | NoteCommand::Diff { .. }
        ),
        _ => false,
    }
}

fn maybe_auto_refresh_command_cache(
    paths: &VaultPaths,
    cli: &Cli,
    use_stderr_color: bool,
) -> Result<(), CliError> {
    if !command_uses_auto_refresh(&cli.command) {
        return Ok(());
    }

    match refresh_mode_for_target(paths, cli, RefreshTarget::Command) {
        AutoScanMode::Off => Ok(()),
        AutoScanMode::Blocking | AutoScanMode::Background => {
            run_incremental_scan(paths, cli.output, use_stderr_color, cli.quiet)?;
            Ok(())
        }
    }
}

fn warn_auto_commit_if_needed(policy: &AutoCommitPolicy, quiet: bool) {
    if !quiet {
        if let Some(message) = policy.warning() {
            eprintln!("warning: {message}");
        }
    }
}

fn refactor_changed_files(report: &RefactorReport) -> Vec<String> {
    report.files.iter().map(|file| file.path.clone()).collect()
}

fn bulk_mutation_changed_files(report: &BulkMutationReport) -> Vec<String> {
    report.files.iter().map(|file| file.path.clone()).collect()
}

fn move_changed_files(summary: &MoveSummary) -> Vec<String> {
    std::iter::once(summary.source_path.clone())
        .chain(std::iter::once(summary.destination_path.clone()))
        .chain(summary.rewritten_files.iter().map(|file| file.path.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn create_note_from_bases_view(
    paths: &VaultPaths,
    file: &str,
    view_index: usize,
    title: Option<&str>,
    dry_run: bool,
) -> Result<BasesCreateReport, CliError> {
    let context = plan_base_note_create(paths, file, view_index).map_err(CliError::operation)?;
    let path = allocate_bases_note_path(paths, &context, title)?;
    let contents = render_bases_note_contents(paths, &context, &path)?;

    if !dry_run {
        let absolute = paths.vault_root().join(&path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        fs::write(&absolute, contents).map_err(CliError::operation)?;
    }

    Ok(BasesCreateReport {
        file: context.file,
        view_name: context.view_name,
        view_index: context.view_index,
        dry_run,
        path,
        folder: context.folder,
        template: context.template,
        properties: context.properties,
        filters: context.filters,
    })
}

fn allocate_bases_note_path(
    paths: &VaultPaths,
    context: &BasesCreateContext,
    title: Option<&str>,
) -> Result<String, CliError> {
    let stem = sanitize_new_note_title(title.unwrap_or("Untitled"));
    let folder_prefix = context
        .folder
        .as_deref()
        .filter(|folder| !folder.is_empty())
        .map_or_else(String::new, |folder| format!("{folder}/"));

    for index in 0.. {
        let suffix = if index == 0 {
            String::new()
        } else {
            format!(" {}", index + 1)
        };
        let candidate = format!("{folder_prefix}{stem}{suffix}.md");
        let normalized = normalize_relative_input_path(
            &candidate,
            RelativePathOptions {
                expected_extension: Some("md"),
                append_extension_if_missing: false,
            },
        )
        .map_err(CliError::operation)?;
        if !paths.vault_root().join(&normalized).exists() {
            return Ok(normalized);
        }
    }

    Err(CliError::operation("failed to allocate a note path"))
}

fn sanitize_new_note_title(title: &str) -> String {
    let trimmed = title.trim();
    let trimmed = trimmed.strip_suffix(".md").unwrap_or(trimmed);
    let sanitized = trimmed
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ if character.is_control() => '-',
            _ => character,
        })
        .collect::<String>();
    let sanitized = sanitized.trim().trim_matches('.').to_string();
    if sanitized.is_empty() {
        "Untitled".to_string()
    } else {
        sanitized
    }
}

fn render_bases_note_contents(
    paths: &VaultPaths,
    context: &BasesCreateContext,
    relative_path: &str,
) -> Result<String, CliError> {
    let config = load_vault_config(paths).config;
    let rendered_template = if let Some(template_name) = context.template.as_deref() {
        let loaded = load_named_template(paths, &config, template_name)?;
        render_loaded_template(
            paths,
            &config,
            &loaded,
            &LoadedTemplateRenderRequest {
                target_path: relative_path,
                target_contents: None,
                engine: TemplateEngineKind::Auto,
                vars: &HashMap::new(),
                allow_mutations: true,
                run_mode: TemplateRunMode::Create,
            },
        )?
        .content
    } else {
        String::new()
    };
    let (template_frontmatter, template_body) =
        parse_frontmatter_document(&rendered_template, true).map_err(CliError::operation)?;
    let derived_frontmatter = build_bases_create_frontmatter(&context.properties)?;
    let merged_frontmatter = merge_template_frontmatter(derived_frontmatter, template_frontmatter);

    render_note_from_parts(merged_frontmatter.as_ref(), &template_body).map_err(CliError::operation)
}

fn build_bases_create_frontmatter(
    properties: &BTreeMap<String, Value>,
) -> Result<Option<vulcan_app::templates::YamlMapping>, CliError> {
    json_properties_to_frontmatter(properties).map_err(CliError::operation)
}

pub(crate) fn inbox_input_text(
    text: Option<&str>,
    file: Option<&PathBuf>,
) -> Result<String, CliError> {
    if let Some(file) = file {
        return fs::read_to_string(file).map_err(CliError::operation);
    }

    match text {
        Some("-") => {
            let mut buffer = String::new();
            io::stdin()
                .read_to_string(&mut buffer)
                .map_err(CliError::operation)?;
            Ok(buffer)
        }
        Some(text) => Ok(text.to_string()),
        None => Err(CliError::operation(
            "`inbox` requires text, `-`, or --file <path>",
        )),
    }
}

pub(crate) fn render_inbox_entry(
    format: &str,
    text: &str,
    variables: &TemplateVariables,
) -> String {
    format
        .replace("{text}", text.trim_end())
        .replace("{date}", &variables.date)
        .replace("{time}", &variables.time)
        .replace("{datetime}", &variables.datetime)
}

#[cfg(test)]
fn render_template_contents(
    template: &str,
    variables: &TemplateVariables,
    config: &TemplatesConfig,
) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let rest = &remaining[start + 2..];
        let Some(end) = rest.find("}}") else {
            rendered.push_str(&remaining[start..]);
            return rendered;
        };

        let expression = rest[..end].trim();
        let replacement =
            render_template_variable(expression, variables, config).unwrap_or_else(|| {
                let mut original = String::with_capacity(expression.len() + 4);
                original.push_str("{{");
                original.push_str(expression);
                original.push_str("}}");
                original
            });
        rendered.push_str(&replacement);
        remaining = &rest[end + 2..];
    }

    rendered.push_str(remaining);
    rendered
}

pub(crate) fn append_at_end(contents: &str, entry: &str) -> String {
    append_entry_at_end(contents, entry).updated
}

pub(crate) fn append_under_heading(contents: &str, heading: &str, entry: &str) -> String {
    append_entry_under_heading(contents, heading, entry).updated
}

fn append_entry_at_end(contents: &str, entry: &str) -> NoteEntryInsertion {
    let mut prefix = contents.trim_end_matches('\n').to_string();
    if !prefix.is_empty() {
        prefix.push_str("\n\n");
    }
    let line_number = i64::try_from(prefix.lines().count().saturating_add(1))
        .expect("line count should fit in i64");
    let mut updated = prefix;
    updated.push_str(entry.trim_end());
    updated.push('\n');

    NoteEntryInsertion {
        updated,
        line_number,
        change: RefactorChange {
            before: String::new(),
            after: entry.trim_end().to_string(),
        },
    }
}

fn append_entry_under_heading(contents: &str, heading: &str, entry: &str) -> NoteEntryInsertion {
    let heading = heading.trim();
    if heading.is_empty() {
        return append_entry_at_end(contents, entry);
    }

    let heading_level = markdown_heading_level(heading);
    let mut offset = 0usize;
    let mut insert_at = None;
    for line in contents.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if insert_at.is_none() && trimmed == heading {
            insert_at = Some(offset + line.len());
        } else if insert_at.is_some()
            && markdown_heading_level(trimmed).is_some_and(|level| Some(level) <= heading_level)
        {
            insert_at = Some(offset);
            break;
        }
        offset += line.len();
    }

    if let Some(insert_at) = insert_at {
        let mut prefix = String::new();
        prefix.push_str(&contents[..insert_at]);
        if !prefix.ends_with('\n') {
            prefix.push('\n');
        }
        if !prefix.ends_with("\n\n") {
            prefix.push('\n');
        }
        let line_number = i64::try_from(prefix.lines().count().saturating_add(1))
            .expect("line count should fit in i64");
        let mut updated = prefix;
        updated.push_str(entry.trim_end());
        updated.push('\n');
        if insert_at < contents.len() && !contents[insert_at..].starts_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&contents[insert_at..]);
        NoteEntryInsertion {
            updated,
            line_number,
            change: RefactorChange {
                before: String::new(),
                after: entry.trim_end().to_string(),
            },
        }
    } else {
        let mut prefix = contents.trim_end_matches('\n').to_string();
        if !prefix.is_empty() {
            prefix.push_str("\n\n");
        }
        prefix.push_str(heading);
        prefix.push_str("\n\n");
        let line_number = i64::try_from(prefix.lines().count().saturating_add(1))
            .expect("line count should fit in i64");
        let mut updated = prefix;
        updated.push_str(entry.trim_end());
        updated.push('\n');
        NoteEntryInsertion {
            updated,
            line_number,
            change: RefactorChange {
                before: String::new(),
                after: entry.trim_end().to_string(),
            },
        }
    }
}

pub(crate) fn markdown_heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|ch| *ch == '#').count();
    (hashes > 0 && hashes <= 6 && line.chars().nth(hashes).is_some_and(char::is_whitespace))
        .then_some(hashes)
}

fn link_confidence_for_note(
    paths: &VaultPaths,
    note_path: &str,
) -> Result<GraphConfidenceBreakdown, CliError> {
    let database = CacheDatabase::open(paths).map_err(CliError::operation)?;
    let mut statement = database
        .connection()
        .prepare(
            "
            SELECT links.confidence, COUNT(*)
            FROM links
            JOIN documents AS source ON source.id = links.source_document_id
            LEFT JOIN documents AS target ON target.id = links.resolved_target_id
            WHERE source.path = ?1 OR target.path = ?1
            GROUP BY links.confidence
            ",
        )
        .map_err(CliError::operation)?;
    let rows = statement
        .query_map([note_path], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
        })
        .map_err(CliError::operation)?;
    let mut confidence = GraphConfidenceBreakdown::default();
    for row in rows {
        let (label, count) = row.map_err(CliError::operation)?;
        match label.as_str() {
            "EXTRACTED" => confidence.extracted += count,
            "INFERRED" => confidence.inferred += count,
            "AMBIGUOUS" => confidence.ambiguous += count,
            _ => {}
        }
    }
    Ok(confidence)
}

pub(crate) fn resolve_bulk_note_selection(
    filters: &[String],
    stdin: bool,
) -> Result<BulkNoteSelection, CliError> {
    if stdin {
        return Ok(BulkNoteSelection::Paths(read_note_paths_from_stdin()?));
    }
    Ok(BulkNoteSelection::Filters(filters.to_vec()))
}

pub(crate) fn read_note_paths_from_stdin() -> Result<Vec<String>, CliError> {
    if io::stdin().is_terminal() {
        return Err(CliError::operation(
            "`--stdin` requires newline-delimited note paths on stdin",
        ));
    }

    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .map_err(CliError::operation)?;

    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for line in buffer.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.strip_prefix("./").unwrap_or(trimmed).to_string();
        if seen.insert(normalized.clone()) {
            paths.push(normalized);
        }
    }
    Ok(paths)
}

fn run_open_command(
    paths: &VaultPaths,
    note: Option<&str>,
    interactive_note_selection: bool,
) -> Result<OpenReport, CliError> {
    let note = resolve_note_argument(paths, note, interactive_note_selection, "note")?;
    let resolved = resolve_note_reference(paths, &note).map_err(CliError::operation)?;
    let uri = build_obsidian_uri(paths, &resolved.path);
    launch_uri(&uri)?;

    Ok(OpenReport {
        path: resolved.path,
        uri,
    })
}

fn build_obsidian_uri(paths: &VaultPaths, path: &str) -> String {
    let vault_name = paths
        .vault_root()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("vault");
    format!(
        "obsidian://open?vault={}&file={}",
        percent_encode(vault_name),
        percent_encode(path)
    )
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn launch_uri(uri: &str) -> Result<(), CliError> {
    let mut command = ProcessCommand::new(open_uri_program());
    for arg in open_uri_args(uri) {
        command.arg(arg);
    }
    let status = command.status().map_err(CliError::operation)?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::operation(format!(
            "launcher exited with status {status} for {uri}"
        )))
    }
}

#[cfg(target_os = "linux")]
fn open_uri_program() -> &'static str {
    "xdg-open"
}

#[cfg(target_os = "macos")]
fn open_uri_program() -> &'static str {
    "open"
}

#[cfg(target_os = "windows")]
fn open_uri_program() -> &'static str {
    "cmd"
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_uri_program() -> &'static str {
    "xdg-open"
}

#[cfg(target_os = "windows")]
fn open_uri_args(uri: &str) -> Vec<String> {
    vec![
        "/C".to_string(),
        "start".to_string(),
        String::new(),
        uri.to_string(),
    ]
}

#[cfg(not(target_os = "windows"))]
fn open_uri_args(uri: &str) -> Vec<String> {
    vec![uri.to_string()]
}

fn saved_export_format(format: ExportFormat) -> SavedExportFormat {
    match format {
        ExportFormat::Csv => SavedExportFormat::Csv,
        ExportFormat::Jsonl => SavedExportFormat::Jsonl,
    }
}

fn export_rows(
    rows: &[Value],
    fields: Option<&[String]>,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let Some(export) = export else {
        return Ok(());
    };

    if let Some(parent) = export.path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }

    match export.format {
        SavedExportFormat::Jsonl => {
            let rendered = rows
                .iter()
                .map(|row| {
                    serde_json::to_string(&select_fields(row.clone(), fields))
                        .map_err(CliError::operation)
                })
                .collect::<Result<Vec<_>, _>>()?
                .join("\n");
            let mut payload = rendered;
            if !payload.is_empty() {
                payload.push('\n');
            }
            fs::write(&export.path, payload).map_err(CliError::operation)?;
        }
        SavedExportFormat::Csv => {
            let mut writer = csv::Writer::from_path(&export.path).map_err(CliError::operation)?;
            let headers = csv_headers(rows, fields);
            writer
                .write_record(headers.iter().map(String::as_str))
                .map_err(CliError::operation)?;
            for row in rows {
                let selected = select_fields(row.clone(), fields);
                let record = headers
                    .iter()
                    .map(|header| csv_cell_for_value(selected.get(header)))
                    .collect::<Vec<_>>();
                writer.write_record(record).map_err(CliError::operation)?;
            }
            writer.flush().map_err(CliError::operation)?;
        }
    }

    Ok(())
}

fn csv_headers(rows: &[Value], fields: Option<&[String]>) -> Vec<String> {
    if let Some(fields) = fields {
        return fields.to_vec();
    }

    let mut headers = rows
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|object| object.keys().cloned())
        .collect::<Vec<_>>();
    headers.sort();
    headers.dedup();
    if headers.is_empty() {
        headers.push("value".to_string());
    }
    headers
}

fn csv_cell_for_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn execute_saved_report(
    paths: &VaultPaths,
    definition: &SavedReportDefinition,
    provider: Option<String>,
    controls: &ListOutputControls,
) -> Result<SavedExecution, CliError> {
    match &definition.query {
        SavedReportQuery::Search {
            query,
            mode,
            tag,
            path_prefix,
            has_property,
            filters,
            context_size,
            sort,
            match_case,
            raw_query,
            fuzzy,
        } => Ok(SavedExecution::Search(
            search_vault(
                paths,
                &SearchQuery {
                    text: query.clone(),
                    tag: tag.clone(),
                    path_prefix: path_prefix.clone(),
                    has_property: has_property.clone(),
                    filters: filters.clone(),
                    provider,
                    mode: *mode,
                    sort: *sort,
                    match_case: *match_case,
                    limit: controls.requested_result_limit(),
                    context_size: *context_size,
                    raw_query: *raw_query,
                    fuzzy: *fuzzy,
                    explain: false,
                },
            )
            .map_err(CliError::operation)?,
        )),
        SavedReportQuery::Notes {
            filters,
            sort_by,
            sort_descending,
        } => Ok(SavedExecution::Notes(
            query_notes(
                paths,
                &NoteQuery {
                    filters: filters.clone(),
                    sort_by: sort_by.clone(),
                    sort_descending: *sort_descending,
                },
            )
            .map_err(CliError::operation)?,
        )),
        SavedReportQuery::Bases { file } => Ok(SavedExecution::Bases(
            evaluate_base_file(paths, file).map_err(CliError::operation)?,
        )),
    }
}

fn saved_execution_rows(execution: &SavedExecution, controls: &ListOutputControls) -> Vec<Value> {
    match execution {
        SavedExecution::Search(report) => {
            search_hit_rows(report, paginated_items(&report.hits, controls))
        }
        SavedExecution::Notes(report) => {
            note_rows(report, paginated_items(&report.notes, controls))
        }
        SavedExecution::Bases(report) => {
            let rows = bases_rows(report);
            let start = controls.offset.min(rows.len());
            let end = controls.limit.map_or(rows.len(), |limit| {
                start.saturating_add(limit).min(rows.len())
            });
            rows[start..end].to_vec()
        }
    }
}

fn run_saved_reports_batch(
    paths: &VaultPaths,
    provider: Option<&String>,
    controls: &ListOutputControls,
    names: &[String],
    all: bool,
) -> Result<BatchRunReport, CliError> {
    if all && !names.is_empty() {
        return Err(CliError::operation(
            "batch accepts either explicit report names or --all, not both",
        ));
    }

    let selected_names = if all {
        list_saved_reports(paths)
            .map_err(CliError::operation)?
            .into_iter()
            .map(|report| report.name)
            .collect::<Vec<_>>()
    } else {
        names.to_vec()
    };

    if selected_names.is_empty() {
        return Err(CliError::operation(
            "no saved reports selected; pass names or use --all",
        ));
    }

    let mut items = Vec::new();
    let mut succeeded = 0_usize;
    for name in selected_names {
        match load_saved_report(paths, &name).map_err(CliError::operation) {
            Ok(definition) => {
                let effective_controls =
                    controls.with_saved_defaults(definition.fields.clone(), definition.limit);
                let result = match definition.export.as_ref() {
                    Some(saved_export) => {
                        let resolved_export = resolve_saved_export(paths, saved_export);
                        execute_saved_report(
                            paths,
                            &definition,
                            provider.cloned(),
                            &effective_controls,
                        )
                        .and_then(|execution| {
                            let rows = saved_execution_rows(&execution, &effective_controls);
                            export_rows(
                                &rows,
                                effective_controls.fields.as_deref(),
                                Some(&resolved_export),
                            )?;
                            Ok(BatchRunItemReport {
                                name: definition.name.clone(),
                                kind: Some(execution.kind()),
                                ok: true,
                                row_count: Some(rows.len()),
                                export_format: Some(resolved_export.format),
                                export_path: Some(resolved_export.path.display().to_string()),
                                error: None,
                            })
                        })
                    }
                    None => Err(CliError::operation(
                        "batch mode requires each saved report to define an export target",
                    )),
                };

                match result {
                    Ok(item) => {
                        succeeded += 1;
                        items.push(item);
                    }
                    Err(error) => {
                        items.push(BatchRunItemReport {
                            name: definition.name,
                            kind: Some(definition.query.kind()),
                            ok: false,
                            row_count: None,
                            export_format: None,
                            export_path: None,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Err(error) => {
                items.push(BatchRunItemReport {
                    name,
                    kind: None,
                    ok: false,
                    row_count: None,
                    export_format: None,
                    export_path: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(BatchRunReport {
        total: items.len(),
        succeeded,
        failed: items.len().saturating_sub(succeeded),
        items,
    })
}

fn doctor_summary_has_issues(summary: &vulcan_core::DoctorSummary) -> bool {
    summary.unresolved_links > 0
        || summary.ambiguous_links > 0
        || summary.broken_embeds > 0
        || summary.parse_failures > 0
        || summary.type_mismatches > 0
        || summary.unsupported_syntax > 0
        || summary.stale_index_rows > 0
        || summary.missing_index_rows > 0
        || summary.orphan_notes > 0
        || summary.orphan_assets > 0
        || summary.html_links > 0
}

fn cli_search_mode(mode: SearchMode) -> vulcan_core::search::SearchMode {
    match mode {
        SearchMode::Keyword => vulcan_core::search::SearchMode::Keyword,
        SearchMode::Hybrid => vulcan_core::search::SearchMode::Hybrid,
    }
}

fn cli_search_sort(sort: SearchSortArg) -> SearchSort {
    match sort {
        SearchSortArg::Relevance => SearchSort::Relevance,
        SearchSortArg::PathAsc => SearchSort::PathAsc,
        SearchSortArg::PathDesc => SearchSort::PathDesc,
        SearchSortArg::ModifiedNewest => SearchSort::ModifiedNewest,
        SearchSortArg::ModifiedOldest => SearchSort::ModifiedOldest,
        SearchSortArg::CreatedNewest => SearchSort::CreatedNewest,
        SearchSortArg::CreatedOldest => SearchSort::CreatedOldest,
    }
}

fn display_search_sort(sort: SearchSort) -> &'static str {
    match sort {
        SearchSort::Relevance => "relevance",
        SearchSort::PathAsc => "path-asc",
        SearchSort::PathDesc => "path-desc",
        SearchSort::ModifiedNewest => "modified-newest",
        SearchSort::ModifiedOldest => "modified-oldest",
        SearchSort::CreatedNewest => "created-newest",
        SearchSort::CreatedOldest => "created-oldest",
    }
}

fn execute_automation_run(
    paths: &VaultPaths,
    provider: Option<&String>,
    output: OutputFormat,
    use_stderr_color: bool,
    controls: &ListOutputControls,
    command: &AutomationCommand,
) -> Result<AutomationRunReport, CliError> {
    let AutomationCommand::Run {
        reports,
        all_reports,
        scan,
        doctor,
        doctor_fix: doctor_fix_requested,
        verify_cache: verify_cache_requested,
        repair_fts: repair_fts_requested,
        fail_on_issues: _,
    } = command
    else {
        return Err(CliError::operation(
            "automation list does not execute scans or report runs",
        ));
    };

    if !*scan
        && !*doctor
        && !*doctor_fix_requested
        && !*verify_cache_requested
        && !*repair_fts_requested
        && !*all_reports
        && reports.is_empty()
    {
        return Err(CliError::operation(
            "automation run requires at least one action",
        ));
    }

    let mut actions = Vec::new();
    let mut scan_report = None;
    if *scan {
        actions.push("scan".to_string());
        let mut progress =
            (output == OutputFormat::Human).then(|| ScanProgressReporter::new(use_stderr_color));
        scan_report = Some(
            scan_vault_with_progress(paths, ScanMode::Incremental, |event| {
                if let Some(progress) = progress.as_mut() {
                    progress.record(&event);
                }
            })
            .map_err(CliError::operation)?,
        );
    }

    let mut doctor_issues = None;
    let mut doctor_fix_report = None;
    if *doctor {
        actions.push("doctor".to_string());
        doctor_issues = Some(doctor_vault(paths).map_err(CliError::operation)?.summary);
    } else if *doctor_fix_requested {
        actions.push("doctor_fix".to_string());
        doctor_fix_report = Some(doctor_fix(paths, false).map_err(CliError::operation)?);
    }

    let mut cache_verify_report = None;
    if *verify_cache_requested {
        actions.push("cache_verify".to_string());
        cache_verify_report = Some(verify_cache(paths).map_err(CliError::operation)?);
    }

    let mut repair_fts_report = None;
    if *repair_fts_requested {
        actions.push("repair_fts".to_string());
        repair_fts_report = Some(
            repair_fts(paths, &RepairFtsQuery { dry_run: false }).map_err(CliError::operation)?,
        );
    }

    let batch_report = if *all_reports || !reports.is_empty() {
        actions.push(if *all_reports {
            "saved_reports_all".to_string()
        } else {
            "saved_reports".to_string()
        });
        Some(run_saved_reports_batch(
            paths,
            provider,
            controls,
            reports,
            *all_reports,
        )?)
    } else {
        None
    };

    let issues_detected = doctor_issues
        .as_ref()
        .is_some_and(doctor_summary_has_issues)
        || doctor_fix_report
            .as_ref()
            .and_then(|report| report.issues_after.as_ref())
            .is_some_and(doctor_summary_has_issues)
        || cache_verify_report
            .as_ref()
            .is_some_and(|report| !report.healthy);

    Ok(AutomationRunReport {
        actions,
        reports: batch_report,
        scan: scan_report,
        doctor_issues,
        doctor_fix: doctor_fix_report,
        cache_verify: cache_verify_report,
        repair_fts: repair_fts_report,
        issues_detected,
    })
}

pub fn run() -> Result<(), CliError> {
    run_from(std::env::args_os())
}

const NOTE_SUBCOMMAND_HINTS: &[&str] = &[
    "get", "set", "create", "append", "update", "unset", "patch", "delete", "rename", "info",
    "history",
];

/// Return a human-readable hint when the user has likely mixed up `note` and `notes`.
fn detect_command_confusion(args: &[OsString]) -> Option<String> {
    let strs: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
    // Skip the binary name at index 0; look at index 1 for the subcommand.
    let subcommand = strs.get(1).copied().unwrap_or("");
    let rest = strs.get(2..).unwrap_or(&[]);
    if subcommand == "notes" {
        if let Some(&sub) = rest.first() {
            if NOTE_SUBCOMMAND_HINTS.contains(&sub) {
                return Some(format!(
                    "`vulcan notes {sub}` is not valid — did you mean `vulcan note {sub}`?\n\
                     `vulcan query --where ...` handles note-set queries; `vulcan note` operates on a single note."
                ));
            }
        }
    }

    // `vulcan note --where …` → should be `vulcan query --where …`
    if subcommand == "note" && rest.contains(&"--where") {
        return Some(
            "`vulcan note --where` is not valid — did you mean `vulcan query --where ...`?\n\
             `vulcan query --where ...` queries matching notes; `vulcan note` operates on a single note."
                .to_string(),
        );
    }

    None
}

fn command_index_for_alias_expansion(args: &[OsString]) -> Option<usize> {
    let mut index = 1;
    while index < args.len() {
        let rendered = args[index].to_string_lossy();
        let takes_value = matches!(
            rendered.as_ref(),
            "--vault"
                | "--output"
                | "--refresh"
                | "--fields"
                | "--provider"
                | "--permissions"
                | "--limit"
                | "--offset"
                | "--color"
        );
        let is_global_flag = takes_value
            || matches!(
                rendered.as_ref(),
                "--verbose" | "--quiet" | "-q" | "--no-header"
            );

        if takes_value {
            index += 2;
            continue;
        }
        if rendered.starts_with("--vault=")
            || rendered.starts_with("--output=")
            || rendered.starts_with("--refresh=")
            || rendered.starts_with("--fields=")
            || rendered.starts_with("--provider=")
            || rendered.starts_with("--permissions=")
            || rendered.starts_with("--limit=")
            || rendered.starts_with("--offset=")
            || rendered.starts_with("--color=")
        {
            index += 1;
            continue;
        }
        if is_global_flag {
            index += 1;
            continue;
        }
        return Some(index);
    }
    None
}

fn extract_vault_root_from_args(args: &[OsString]) -> PathBuf {
    let mut index = 1;
    while index < args.len() {
        let rendered = args[index].to_string_lossy();
        if rendered == "--vault" {
            if let Some(path) = args.get(index + 1) {
                return PathBuf::from(path);
            }
            break;
        }
        if let Some(path) = rendered.strip_prefix("--vault=") {
            return PathBuf::from(path);
        }
        index += 1;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn split_alias_words(source: &str) -> Option<Vec<OsString>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = source.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            _ if ch.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    words.push(OsString::from(current.clone()));
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if in_single || in_double {
        return None;
    }
    if !current.is_empty() {
        words.push(OsString::from(current));
    }
    Some(words)
}

fn should_skip_alias_expansion(args: &[OsString], command_index: usize) -> bool {
    args.get(command_index)
        .and_then(|value| value.to_str())
        .is_some_and(|command| {
            command == "notes"
                && args
                    .get(command_index + 1)
                    .and_then(|value| value.to_str())
                    .is_some_and(|next| NOTE_SUBCOMMAND_HINTS.contains(&next))
        })
}

fn expand_cli_aliases(args: &[OsString]) -> Vec<OsString> {
    let aliases = load_vault_config(&VaultPaths::new(extract_vault_root_from_args(args)))
        .config
        .aliases;
    if aliases.is_empty() {
        return args.to_vec();
    }

    let mut expanded = args.to_vec();
    for _ in 0..8 {
        let Some(command_index) = command_index_for_alias_expansion(&expanded) else {
            break;
        };
        if should_skip_alias_expansion(&expanded, command_index) {
            break;
        }
        let Some(command_name) = expanded[command_index].to_str() else {
            break;
        };
        let Some(alias) = aliases.get(command_name) else {
            break;
        };
        let Some(replacement) = split_alias_words(alias) else {
            break;
        };
        if replacement.is_empty() {
            break;
        }
        let mut rewritten = expanded[..command_index].to_vec();
        rewritten.extend(replacement);
        rewritten.extend_from_slice(&expanded[command_index + 1..]);
        expanded = rewritten;
    }
    expanded
}

pub fn run_from<I, T>(args: I) -> Result<(), CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<OsString>>();
    let expanded_args = expand_cli_aliases(&args);
    let cli = match Cli::try_parse_from(&expanded_args) {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                error.print().map_err(CliError::operation)?;
                return Ok(());
            }
            _ => {
                if let Some(hint) = detect_command_confusion(&args) {
                    eprintln!("hint: {hint}");
                }
                return Err(CliError::clap(&error));
            }
        },
    };
    dispatch(&cli)
}

#[allow(clippy::too_many_lines)]
fn dispatch(cli: &Cli) -> Result<(), CliError> {
    // Handle `complete` before vault resolution: vault-independent contexts (e.g.
    // daily-date) must work when invoked from outside a vault by shell completion
    // hooks.  Vault-dependent contexts silently return empty output rather than
    // erroring so the shell gets a clean exit.
    if let Command::Complete {
        ref context,
        ref prefix,
    } = cli.command
    {
        if context == "daily-date" {
            // Emit vault-free candidates (keywords + last 14 days) first, then
            // supplement with existing note dates from the vault if available.
            let mut seen = std::collections::HashSet::new();
            for candidate in collect_complete_candidates_no_vault(context, prefix.as_deref()) {
                if seen.insert(candidate.clone()) {
                    println!("{candidate}");
                }
            }
            if let Ok(paths) = resolve_vault_root(&cli.vault).map(VaultPaths::new) {
                for candidate in collect_complete_candidates(&paths, context, prefix.as_deref()) {
                    if seen.insert(candidate.clone()) {
                        println!("{candidate}");
                    }
                }
            }
            return Ok(());
        }
        // All other contexts need a vault; if we can't find one, return empty.
        let Ok(paths) = resolve_vault_root(&cli.vault).map(VaultPaths::new) else {
            return Ok(());
        };
        run_complete_command(&paths, context, prefix.as_deref());
        return Ok(());
    }

    if let Command::Render(RenderArgs { ref file, mode }) = cli.command {
        let stdout_is_tty = io::stdout().is_terminal();
        let use_stdout_color = resolve_use_color(cli.color, stdout_is_tty);
        let render_paths = VaultPaths::new(resolve_vault_root(&cli.vault)?);
        let report = run_render_command(&render_paths, file.as_ref(), mode)?;
        return print_render_report(cli.output, &report, stdout_is_tty, use_stdout_color);
    }

    let paths = VaultPaths::new(resolve_vault_root(&cli.vault)?);
    let list_controls = ListOutputControls::from_cli(cli);
    let stdout_is_tty = io::stdout().is_terminal();
    let stderr_is_tty = io::stderr().is_terminal();
    let use_stdout_color = resolve_use_color(cli.color, stdout_is_tty);
    let use_stderr_color = resolve_use_color(cli.color, stderr_is_tty);
    maybe_auto_refresh_command_cache(&paths, cli, use_stderr_color)?;
    let interactive_note_selection = interactive_note_selection_allowed(cli, stdout_is_tty);

    match cli.command {
        Command::Render(_) => unreachable!("render handled before vault resolution"),
        Command::Index { ref command } => commands::index::handle_index_command(
            cli,
            &paths,
            command,
            stdout_is_tty,
            use_stderr_color,
            use_stdout_color,
        ),
        Command::Backlinks {
            ref note,
            ref export,
        } => commands::query::handle_backlinks_command(
            cli,
            &paths,
            note.as_deref(),
            export,
            interactive_note_selection,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Graph { ref command } => commands::graph::handle_graph_command(
            cli,
            &paths,
            command,
            interactive_note_selection,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Edit {
            ref note,
            new,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_edit_command(
                &paths,
                cli,
                stdout_is_tty,
                use_stderr_color,
                note.as_deref(),
                new,
            )?;
            auto_commit
                .commit(
                    &paths,
                    "edit",
                    std::slice::from_ref(&report.path),
                    cli.permissions.as_deref(),
                    cli.quiet,
                )
                .map_err(CliError::operation)?;
            print_edit_report(cli.output, &report);
            Ok(())
        }
        Command::Open { ref note } => {
            let report = run_open_command(&paths, note.as_deref(), interactive_note_selection)?;
            print_open_report(cli.output, &report)
        }
        Command::Browse { no_commit } => {
            commands::browse::handle_browse_command(cli, &paths, stdout_is_tty, no_commit)
        }
        Command::Note { ref command } => commands::note::handle_note_command(
            cli,
            &paths,
            command,
            interactive_note_selection,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
            use_stderr_color,
        ),
        Command::Completions { shell } => {
            let mut buf = Vec::new();
            let mut command = Cli::command();
            generate(shell, &mut command, "vulcan", &mut buf);
            let static_script = String::from_utf8_lossy(&buf).into_owned();
            let dynamic = generate_dynamic_completions(shell);
            // clap_complete doesn't add "not __fish_seen_subcommand_from" guards
            // for nested subcommand candidates — patch the script so subcommand
            // names stop cycling once one has been selected.
            let patched = if matches!(shell, clap_complete::Shell::Fish) {
                fix_fish_nested_subcommand_guards(&static_script)
            } else {
                static_script
            };
            if matches!(shell, clap_complete::Shell::Fish) {
                // Fish accumulates `complete -c vulcan ...` definitions when a refreshed
                // script is sourced into an existing shell session, so clear them first.
                println!("complete -c vulcan -e");
            }
            print!("{patched}");
            if !dynamic.is_empty() {
                println!("{dynamic}");
            }
            Ok(())
        }
        Command::Complete {
            ref context,
            ref prefix,
        } => {
            run_complete_command(&paths, context, prefix.as_deref());
            Ok(())
        }
        Command::Plugin { ref command } => commands::plugin::handle_plugin_command(
            cli,
            &paths,
            command,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Tool { ref command } => commands::tool::handle_tool_command(cli, &paths, command),
        Command::Site { ref command } => {
            handle_site_command(cli.output, &paths, command, use_stderr_color)
        }
        Command::Agent { ref command } => {
            commands::agent::handle_agent_command(cli, &paths, command)
        }
        Command::Skill { ref command } => {
            commands::skill::handle_skill_command(cli, &paths, command)
        }
        Command::Status => {
            let report = run_status_command(&paths)?;
            print_status_report(cli.output, &report, use_stdout_color)
        }
        Command::Mcp {
            ref tool_pack,
            tool_pack_mode,
            transport,
            ref request_timeout,
            ref bind,
            ref endpoint,
            ref auth_token,
            ref public_url,
            ref oauth_issuer,
            ref oauth_audience,
            ref oauth_jwks_url,
            ref oauth_allowed_sub,
            ref oauth_allowed_email,
            ref oauth_local_client_id,
            ref oauth_local_client_secret,
            ref oauth_local_approval_token,
            ref oauth_local_subject,
            ref oauth_local_email,
            oauth_dcr,
            ref oauth_dcr_allowed_redirect_host,
            ref oauth_indieauth_authorization_endpoint,
            ref oauth_indieauth_token_endpoint,
            ref oauth_indieauth_client_id,
            ref oauth_indieauth_redirect_uri,
            ref oauth_indieauth_me,
            ref oauth_local_user,
        } => {
            let request_timeout =
                commands::runtime::parse_run_timeout(Some(request_timeout.as_str()))?
                    .unwrap_or(mcp::DEFAULT_MCP_REQUEST_TIMEOUT);
            mcp::run_mcp(
                &paths,
                cli.permissions.as_deref(),
                tool_pack,
                tool_pack_mode,
                transport,
                &mcp::McpHttpOptions {
                    bind: bind.clone(),
                    endpoint: endpoint.clone(),
                    auth_token: auth_token.clone(),
                    public_url: public_url.clone(),
                    oauth_issuer: oauth_issuer.clone(),
                    oauth_audience: oauth_audience.clone(),
                    oauth_jwks_url: oauth_jwks_url.clone(),
                    oauth_allowed_sub: oauth_allowed_sub.clone(),
                    oauth_allowed_email: oauth_allowed_email.clone(),
                    oauth_local_client_id: oauth_local_client_id.clone(),
                    oauth_local_client_secret: oauth_local_client_secret.clone(),
                    oauth_local_approval_token: oauth_local_approval_token.clone(),
                    oauth_local_subject: oauth_local_subject.clone(),
                    oauth_local_email: oauth_local_email.clone(),
                    oauth_dcr,
                    oauth_dcr_allowed_redirect_host: oauth_dcr_allowed_redirect_host.clone(),
                    oauth_indieauth_authorization_endpoint: oauth_indieauth_authorization_endpoint
                        .clone(),
                    oauth_indieauth_token_endpoint: oauth_indieauth_token_endpoint.clone(),
                    oauth_indieauth_client_id: oauth_indieauth_client_id.clone(),
                    oauth_indieauth_redirect_uri: oauth_indieauth_redirect_uri.clone(),
                    oauth_indieauth_me: oauth_indieauth_me.clone(),
                    oauth_local_user: oauth_local_user.clone(),
                    request_timeout,
                },
            )
        }
        Command::Trust { ref command } => {
            commands::runtime::handle_trust_command(cli, &paths, command.as_ref())
        }
        Command::Bases { ref command } => commands::bases::handle_bases_command(
            cli,
            &paths,
            command,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
            use_stderr_color,
        ),
        Command::Help {
            ref search,
            ref topic,
        } => commands::docs::handle_help_command(
            cli.output,
            topic,
            search.as_deref(),
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Describe {
            format,
            ref tool_pack,
            tool_pack_mode,
        } => commands::docs::handle_describe_command(
            &paths,
            cli.output,
            format,
            tool_pack,
            tool_pack_mode,
            cli.permissions.as_deref(),
        ),
        Command::Doctor {
            fix,
            dry_run,
            fail_on_issues,
        } => {
            if fix {
                let report = doctor_fix(&paths, dry_run).map_err(CliError::operation)?;
                print_doctor_fix_report(cli.output, &paths, &report)?;
                if fail_on_issues {
                    let summary = report
                        .issues_after
                        .as_ref()
                        .unwrap_or(&report.issues_before);
                    if doctor_summary_has_issues(summary) {
                        return Err(CliError::issues("doctor found remaining issues"));
                    }
                }
            } else {
                let report = doctor_vault(&paths).map_err(CliError::operation)?;
                print_doctor_report(cli.output, &paths, &report)?;
                if fail_on_issues && doctor_summary_has_issues(&report.summary) {
                    return Err(CliError::issues("doctor found issues"));
                }
            }
            Ok(())
        }
        Command::Init(ref args) => {
            selected_permission_guard(cli, &paths)?
                .check_index()
                .map_err(CliError::operation)?;
            let report = run_init_command(&paths, args)?;
            print_init_summary(cli.output, &paths, &report)?;
            Ok(())
        }
        Command::Rebuild { dry_run } => {
            selected_permission_guard(cli, &paths)?
                .check_index()
                .map_err(CliError::operation)?;
            let mut progress = (cli.output == OutputFormat::Human)
                .then(|| ScanProgressReporter::new(use_stderr_color));
            let report = rebuild_vault_with_progress(&paths, &RebuildQuery { dry_run }, |event| {
                if let Some(progress) = progress.as_mut() {
                    progress.record(&event);
                }
            })
            .map_err(CliError::operation)?;
            print_rebuild_report(cli.output, &report, use_stdout_color)
        }
        Command::Move {
            ref source,
            ref dest,
            dry_run,
            no_commit,
        } => {
            let guard = selected_permission_guard(cli, &paths)?;
            guard
                .check_refactor_path(source)
                .map_err(CliError::operation)?;
            guard
                .check_refactor_path(dest)
                .map_err(CliError::operation)?;
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let summary = move_note(&paths, source, dest, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                let changed_paths = move_changed_files(&summary);
                auto_commit
                    .commit(
                        &paths,
                        "move",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                let _ = crate::plugins::dispatch_plugin_event(
                    &paths,
                    cli.permissions.as_deref(),
                    PluginEvent::OnRefactor,
                    &serde_json::json!({
                        "kind": PluginEvent::OnRefactor,
                        "action": "move",
                        "paths": changed_paths,
                    }),
                    cli.quiet,
                );
            }
            print_move_summary(cli.output, &summary)?;
            Ok(())
        }
        Command::RenameProperty {
            ref old,
            ref new,
            dry_run,
            no_commit,
        } => {
            let guard = selected_permission_guard(cli, &paths)?;
            if !guard.refactor_filter().path_permission().is_unrestricted() {
                return Err(CliError::operation(
                    "permission denied: rename-property requires unrestricted refactor scope under the selected profile",
                ));
            }
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = rename_property(&paths, old, new, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                let changed_paths = refactor_changed_files(&report);
                auto_commit
                    .commit(
                        &paths,
                        "rename-property",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                let _ = crate::plugins::dispatch_plugin_event(
                    &paths,
                    cli.permissions.as_deref(),
                    PluginEvent::OnRefactor,
                    &serde_json::json!({
                        "kind": PluginEvent::OnRefactor,
                        "action": "rename-property",
                        "paths": changed_paths,
                    }),
                    cli.quiet,
                );
            }
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::MergeTags {
            ref source,
            ref dest,
            dry_run,
            no_commit,
        } => {
            let guard = selected_permission_guard(cli, &paths)?;
            if !guard.refactor_filter().path_permission().is_unrestricted() {
                return Err(CliError::operation(
                    "permission denied: merge-tags requires unrestricted refactor scope under the selected profile",
                ));
            }
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = merge_tags(&paths, source, dest, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                let changed_paths = refactor_changed_files(&report);
                auto_commit
                    .commit(
                        &paths,
                        "merge-tags",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                let _ = crate::plugins::dispatch_plugin_event(
                    &paths,
                    cli.permissions.as_deref(),
                    PluginEvent::OnRefactor,
                    &serde_json::json!({
                        "kind": PluginEvent::OnRefactor,
                        "action": "merge-tags",
                        "paths": changed_paths,
                    }),
                    cli.quiet,
                );
            }
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::RenameAlias {
            ref note,
            ref old,
            ref new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let note = resolve_note_argument(
                &paths,
                Some(note.as_str()),
                interactive_note_selection,
                "note to update",
            )?;
            selected_permission_guard(cli, &paths)?
                .check_refactor_path(&note)
                .map_err(CliError::operation)?;
            let report =
                rename_alias(&paths, &note, old, new, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                let changed_paths = refactor_changed_files(&report);
                auto_commit
                    .commit(
                        &paths,
                        "rename-alias",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                let _ = crate::plugins::dispatch_plugin_event(
                    &paths,
                    cli.permissions.as_deref(),
                    PluginEvent::OnRefactor,
                    &serde_json::json!({
                        "kind": PluginEvent::OnRefactor,
                        "action": "rename-alias",
                        "paths": changed_paths,
                    }),
                    cli.quiet,
                );
            }
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::RenameHeading {
            ref note,
            ref old,
            ref new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let note = resolve_note_argument(
                &paths,
                Some(note.as_str()),
                interactive_note_selection,
                "note containing heading",
            )?;
            selected_permission_guard(cli, &paths)?
                .check_refactor_path(&note)
                .map_err(CliError::operation)?;
            let report =
                rename_heading(&paths, &note, old, new, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                let changed_paths = refactor_changed_files(&report);
                auto_commit
                    .commit(
                        &paths,
                        "rename-heading",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                let _ = crate::plugins::dispatch_plugin_event(
                    &paths,
                    cli.permissions.as_deref(),
                    PluginEvent::OnRefactor,
                    &serde_json::json!({
                        "kind": PluginEvent::OnRefactor,
                        "action": "rename-heading",
                        "paths": changed_paths,
                    }),
                    cli.quiet,
                );
            }
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::RenameBlockRef {
            ref note,
            ref old,
            ref new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let note = resolve_note_argument(
                &paths,
                Some(note.as_str()),
                interactive_note_selection,
                "note containing block ref",
            )?;
            selected_permission_guard(cli, &paths)?
                .check_refactor_path(&note)
                .map_err(CliError::operation)?;
            let report =
                rename_block_ref(&paths, &note, old, new, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                let changed_paths = refactor_changed_files(&report);
                auto_commit
                    .commit(
                        &paths,
                        "rename-block-ref",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                let _ = crate::plugins::dispatch_plugin_event(
                    &paths,
                    cli.permissions.as_deref(),
                    PluginEvent::OnRefactor,
                    &serde_json::json!({
                        "kind": PluginEvent::OnRefactor,
                        "action": "rename-block-ref",
                        "paths": changed_paths,
                    }),
                    cli.quiet,
                );
            }
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::Cache { ref command } => match command {
            CacheCommand::Inspect => {
                let report = inspect_cache(&paths).map_err(CliError::operation)?;
                print_cache_inspect_report(cli.output, &report)
            }
            CacheCommand::Verify { fail_on_errors } => {
                let report = verify_cache(&paths).map_err(CliError::operation)?;
                print_cache_verify_report(cli.output, &report)?;
                if *fail_on_errors && !report.healthy {
                    Err(CliError::issues("cache verification failed"))
                } else {
                    Ok(())
                }
            }
            CacheCommand::Vacuum { dry_run } => {
                let report = cache_vacuum(&paths, &CacheVacuumQuery { dry_run: *dry_run })
                    .map_err(CliError::operation)?;
                print_cache_vacuum_report(cli.output, &report)
            }
        },
        Command::Repair { ref command } => match command {
            RepairCommand::Fts { dry_run } => {
                selected_permission_guard(cli, &paths)?
                    .check_index()
                    .map_err(CliError::operation)?;
                let report = repair_fts(&paths, &RepairFtsQuery { dry_run: *dry_run })
                    .map_err(CliError::operation)?;
                print_repair_fts_report(cli.output, &report)
            }
        },
        Command::Serve {
            ref bind,
            no_watch,
            debounce_ms,
            ref auth_token,
        } => {
            selected_permission_guard(cli, &paths)?
                .check_index()
                .map_err(CliError::operation)?;
            serve_forever(
                &paths,
                &ServeOptions {
                    bind: bind.clone(),
                    watch: !no_watch,
                    debounce_ms,
                    auth_token: auth_token.clone(),
                    permissions: cli.permissions.clone(),
                },
            )
        }
        Command::Watch {
            debounce_ms,
            no_commit,
        } => {
            selected_permission_guard(cli, &paths)?
                .check_index()
                .map_err(CliError::operation)?;
            let auto_commit = AutoCommitPolicy::for_scan(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            if cli.output == OutputFormat::Human && stdout_is_tty {
                println!(
                    "Watching {} (debounce {}ms)",
                    paths.vault_root().display(),
                    debounce_ms
                );
            }
            watch_vault(&paths, &WatchOptions { debounce_ms }, |report| {
                print_watch_report(cli.output, &report)?;
                if !report.startup
                    && report.summary.added + report.summary.updated + report.summary.deleted > 0
                {
                    auto_commit
                        .commit(
                            &paths,
                            "scan",
                            &report.paths,
                            cli.permissions.as_deref(),
                            cli.quiet,
                        )
                        .map_err(CliError::operation)?;
                }
                let _ = crate::plugins::dispatch_plugin_event(
                    &paths,
                    cli.permissions.as_deref(),
                    PluginEvent::OnScanComplete,
                    &serde_json::json!({
                        "kind": PluginEvent::OnScanComplete,
                        "mode": "watch",
                        "summary": &report.summary,
                        "paths": &report.paths,
                    }),
                    cli.quiet,
                );
                Ok::<(), CliError>(())
            })
            .map_err(CliError::operation)
        }
        Command::Links {
            ref note,
            ref export,
        } => commands::query::handle_links_command(
            cli,
            &paths,
            note.as_deref(),
            export,
            interactive_note_selection,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Query {
            ref dsl,
            ref json,
            ref filters,
            ref sort,
            desc,
            list_fields,
            engine,
            format,
            ref glob,
            explain,
            exit_code,
            ref export,
        } => commands::query::handle_query_command(
            cli,
            &paths,
            dsl.as_deref(),
            json.as_deref(),
            filters,
            sort.as_ref(),
            desc,
            list_fields,
            engine,
            format,
            glob.as_deref(),
            explain,
            exit_code,
            export,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Ls {
            ref filters,
            ref glob,
            ref tag,
            format,
            ref export,
        } => commands::query::handle_ls_command(
            cli,
            &paths,
            filters,
            glob.as_deref(),
            tag.as_deref(),
            format,
            export,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Update {
            ref filters,
            stdin,
            ref key,
            ref value,
            dry_run,
            no_commit,
        } => commands::query::handle_update_command(
            cli, &paths, filters, stdin, key, value, dry_run, no_commit,
        ),
        Command::Unset {
            ref filters,
            stdin,
            ref key,
            dry_run,
            no_commit,
        } => commands::query::handle_unset_command(
            cli, &paths, filters, stdin, key, dry_run, no_commit,
        ),
        Command::Tags {
            ref filters,
            sort,
            count,
        } => {
            commands::query::handle_tags_command(cli, &paths, filters, sort, count, &list_controls)
        }
        Command::Properties {
            count,
            r#type,
            sort,
        } => commands::query::handle_properties_command(
            cli,
            &paths,
            sort,
            count,
            r#type,
            &list_controls,
        ),
        Command::Dataview { ref command } => commands::dataview::handle_dataview_command(
            cli,
            &paths,
            command,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Tasks { ref command } => commands::tasks::handle_tasks_command(
            cli,
            &paths,
            command,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
            use_stderr_color,
        ),
        Command::Kanban { ref command } => commands::kanban::handle_kanban_command(
            cli,
            &paths,
            command,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Search {
            ref query,
            ref regex,
            ref filters,
            mode,
            ref tag,
            ref path_prefix,
            ref has_property,
            sort,
            match_case,
            context_size,
            raw_query,
            fuzzy,
            explain,
            exit_code,
            ref export,
        } => commands::query::handle_search_command(
            cli,
            &paths,
            query.as_deref(),
            regex.as_deref(),
            filters,
            mode,
            tag.as_deref(),
            path_prefix.as_deref(),
            has_property.as_deref(),
            sort,
            match_case,
            context_size,
            raw_query,
            fuzzy,
            explain,
            exit_code,
            export,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Refactor { ref command } => commands::refactor::handle_refactor_command(
            cli,
            &paths,
            command,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Suggest { ref command } => commands::refactor::handle_suggest_command(
            cli,
            &paths,
            command,
            interactive_note_selection,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Saved { ref command } => match command {
            SavedCommand::List => {
                let reports = list_saved_reports(&paths).map_err(CliError::operation)?;
                print_saved_report_list(
                    cli.output,
                    &reports,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                )
            }
            SavedCommand::Show { name } => {
                let definition = load_saved_report(&paths, name).map_err(CliError::operation)?;
                print_saved_report_definition(cli.output, &definition)
            }
            SavedCommand::Create { command } => {
                let definition = saved_report_definition_from_create(cli, command)?;
                save_saved_report(&paths, &definition).map_err(CliError::operation)?;
                print_saved_report_definition(cli.output, &definition)
            }
            SavedCommand::Delete { name } => {
                let path = delete_saved_report(&paths, name).map_err(CliError::operation)?;
                let report = SavedReportDeleteReport {
                    name: name.clone(),
                    path,
                    deleted: true,
                };
                print_saved_report_delete_report(cli.output, &report)
            }
            SavedCommand::Run { name, export } => {
                let definition = load_saved_report(&paths, name).map_err(CliError::operation)?;
                let effective_controls =
                    list_controls.with_saved_defaults(definition.fields.clone(), definition.limit);
                let resolved_export = resolve_runtime_export(&paths, &definition, export)?;
                let execution = execute_saved_report(
                    &paths,
                    &definition,
                    cli.provider.clone(),
                    &effective_controls,
                )?;
                match execution {
                    SavedExecution::Search(report) => print_search_report(
                        cli.output,
                        &report,
                        &effective_controls,
                        stdout_is_tty,
                        use_stdout_color,
                        resolved_export.as_ref(),
                    ),
                    SavedExecution::Notes(report) => print_notes_report(
                        cli.output,
                        &report,
                        &effective_controls,
                        stdout_is_tty,
                        use_stdout_color,
                        resolved_export.as_ref(),
                    ),
                    SavedExecution::Bases(report) => print_bases_report(
                        cli.output,
                        &report,
                        &effective_controls,
                        stdout_is_tty,
                        use_stdout_color,
                        resolved_export.as_ref(),
                    ),
                }
            }
        },
        Command::Checkpoint { ref command } => match command {
            CheckpointCommand::Create { name } => {
                let record = create_checkpoint(&paths, name).map_err(CliError::operation)?;
                print_checkpoint_record(cli.output, &record)
            }
            CheckpointCommand::List { export } => {
                let records = list_checkpoints(&paths).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_checkpoint_list(
                    cli.output,
                    &records,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )
            }
        },
        Command::Export { ref command } => {
            let read_filter = selected_read_permission_filter(cli, &paths)?;
            match command {
                ExportCommand::Profile { command } => match command {
                    ExportProfileCommand::List => {
                        let profiles = list_export_profiles(&paths);
                        print_export_profile_list(cli.output, &profiles)
                    }
                    ExportProfileCommand::Run { name } => {
                        run_export_profile(cli, &paths, name, read_filter.as_ref())
                    }
                    ExportProfileCommand::Serve {
                        name,
                        port,
                        debounce_ms,
                    } => run_export_profile_serve(&paths, name, *port, *debounce_ms),
                    ExportProfileCommand::Show { name } => {
                        run_export_profile_show(&paths, cli.output, name)
                    }
                    ExportProfileCommand::Create {
                        name,
                        format,
                        query,
                        query_json,
                        path,
                        site_profile,
                        title,
                        author,
                        toc,
                        backlinks,
                        frontmatter,
                        pretty,
                        graph_format,
                        replace,
                        dry_run,
                        no_commit,
                    } => {
                        let request = ExportProfileCreateRequest {
                            format: export_profile_format_from_arg(*format),
                            query: query.clone(),
                            query_json: query_json.clone(),
                            path: path.clone(),
                            site_profile: site_profile.clone(),
                            title: title.clone(),
                            author: author.clone(),
                            toc: export_epub_toc_style_config_from_cli(*toc),
                            backlinks: *backlinks,
                            frontmatter: *frontmatter,
                            pretty: *pretty,
                            graph_format: export_graph_format_config_from_cli(*graph_format),
                        };
                        run_export_profile_create(
                            &paths,
                            cli.output,
                            name,
                            &request,
                            *replace,
                            ConfigMutationOptions {
                                apply_mode: if *dry_run {
                                    ApplyMode::DryRun
                                } else {
                                    ApplyMode::Apply
                                },
                                commit_mode: if *no_commit {
                                    CommitMode::Skip
                                } else {
                                    CommitMode::Auto
                                },
                                quiet: cli.quiet,
                            },
                        )
                    }
                    ExportProfileCommand::Set {
                        name,
                        format,
                        query,
                        query_json,
                        clear_query,
                        path,
                        clear_path,
                        site_profile,
                        clear_site_profile,
                        title,
                        clear_title,
                        author,
                        clear_author,
                        toc,
                        clear_toc,
                        backlinks,
                        no_backlinks,
                        frontmatter,
                        no_frontmatter,
                        pretty,
                        no_pretty,
                        graph_format,
                        clear_graph_format,
                        dry_run,
                        no_commit,
                    } => {
                        let request = ExportProfileSetRequest {
                            format: format.map(export_profile_format_from_arg),
                            query: query.clone(),
                            query_json: query_json.clone(),
                            clear_query: *clear_query,
                            path: if *clear_path {
                                ConfigValueUpdate::Clear
                            } else if let Some(path) = path {
                                ConfigValueUpdate::Set(path.clone())
                            } else {
                                ConfigValueUpdate::Keep
                            },
                            site_profile: if *clear_site_profile {
                                ConfigValueUpdate::Clear
                            } else if let Some(site_profile) = site_profile {
                                ConfigValueUpdate::Set(site_profile.clone())
                            } else {
                                ConfigValueUpdate::Keep
                            },
                            title: if *clear_title {
                                ConfigValueUpdate::Clear
                            } else if let Some(title) = title {
                                ConfigValueUpdate::Set(title.clone())
                            } else {
                                ConfigValueUpdate::Keep
                            },
                            author: if *clear_author {
                                ConfigValueUpdate::Clear
                            } else if let Some(author) = author {
                                ConfigValueUpdate::Set(author.clone())
                            } else {
                                ConfigValueUpdate::Keep
                            },
                            toc: if *clear_toc {
                                ConfigValueUpdate::Clear
                            } else if let Some(toc) = *toc {
                                ConfigValueUpdate::Set(
                                    export_epub_toc_style_config_from_cli(Some(toc))
                                        .expect("toc setting should convert"),
                                )
                            } else {
                                ConfigValueUpdate::Keep
                            },
                            backlinks: if *backlinks {
                                BoolConfigUpdate::SetTrue
                            } else if *no_backlinks {
                                BoolConfigUpdate::Clear
                            } else {
                                BoolConfigUpdate::Keep
                            },
                            frontmatter: if *frontmatter {
                                BoolConfigUpdate::SetTrue
                            } else if *no_frontmatter {
                                BoolConfigUpdate::Clear
                            } else {
                                BoolConfigUpdate::Keep
                            },
                            pretty: if *pretty {
                                BoolConfigUpdate::SetTrue
                            } else if *no_pretty {
                                BoolConfigUpdate::Clear
                            } else {
                                BoolConfigUpdate::Keep
                            },
                            graph_format: if *clear_graph_format {
                                ConfigValueUpdate::Clear
                            } else if let Some(graph_format) = *graph_format {
                                ConfigValueUpdate::Set(
                                    export_graph_format_config_from_cli(Some(graph_format))
                                        .expect("graph format should convert"),
                                )
                            } else {
                                ConfigValueUpdate::Keep
                            },
                        };
                        run_export_profile_set(
                            &paths,
                            cli.output,
                            name,
                            &request,
                            ConfigMutationOptions {
                                apply_mode: if *dry_run {
                                    ApplyMode::DryRun
                                } else {
                                    ApplyMode::Apply
                                },
                                commit_mode: if *no_commit {
                                    CommitMode::Skip
                                } else {
                                    CommitMode::Auto
                                },
                                quiet: cli.quiet,
                            },
                        )
                    }
                    ExportProfileCommand::Delete {
                        name,
                        dry_run,
                        no_commit,
                    } => run_export_profile_delete(
                        &paths,
                        cli.output,
                        name,
                        ConfigMutationOptions {
                            apply_mode: if *dry_run {
                                ApplyMode::DryRun
                            } else {
                                ApplyMode::Apply
                            },
                            commit_mode: if *no_commit {
                                CommitMode::Skip
                            } else {
                                CommitMode::Auto
                            },
                            quiet: cli.quiet,
                        },
                    ),
                    ExportProfileCommand::Rule { command } => match command {
                        ExportProfileRuleCommand::List { profile } => {
                            run_export_profile_rule_list(&paths, cli.output, profile)
                        }
                        ExportProfileRuleCommand::Add {
                            profile,
                            before,
                            query,
                            query_json,
                            transforms,
                            dry_run,
                            no_commit,
                        } => {
                            let request = ExportProfileRuleRequest {
                                query: query.clone(),
                                query_json: query_json.clone(),
                                exclude_callouts: transforms.exclude_callouts.clone(),
                                exclude_headings: transforms.exclude_headings.clone(),
                                exclude_frontmatter_keys: transforms
                                    .exclude_frontmatter_keys
                                    .clone(),
                                exclude_inline_fields: transforms.exclude_inline_fields.clone(),
                                replacement_rules: transforms.replace_rules.clone(),
                            };
                            run_export_profile_rule_add(
                                &paths,
                                cli.output,
                                profile,
                                *before,
                                &request,
                                ConfigMutationOptions {
                                    apply_mode: if *dry_run {
                                        ApplyMode::DryRun
                                    } else {
                                        ApplyMode::Apply
                                    },
                                    commit_mode: if *no_commit {
                                        CommitMode::Skip
                                    } else {
                                        CommitMode::Auto
                                    },
                                    quiet: cli.quiet,
                                },
                            )
                        }
                        ExportProfileRuleCommand::Update {
                            profile,
                            index,
                            query,
                            query_json,
                            transforms,
                            dry_run,
                            no_commit,
                        } => {
                            let request = ExportProfileRuleRequest {
                                query: query.clone(),
                                query_json: query_json.clone(),
                                exclude_callouts: transforms.exclude_callouts.clone(),
                                exclude_headings: transforms.exclude_headings.clone(),
                                exclude_frontmatter_keys: transforms
                                    .exclude_frontmatter_keys
                                    .clone(),
                                exclude_inline_fields: transforms.exclude_inline_fields.clone(),
                                replacement_rules: transforms.replace_rules.clone(),
                            };
                            run_export_profile_rule_update(
                                &paths,
                                cli.output,
                                profile,
                                *index,
                                &request,
                                ConfigMutationOptions {
                                    apply_mode: if *dry_run {
                                        ApplyMode::DryRun
                                    } else {
                                        ApplyMode::Apply
                                    },
                                    commit_mode: if *no_commit {
                                        CommitMode::Skip
                                    } else {
                                        CommitMode::Auto
                                    },
                                    quiet: cli.quiet,
                                },
                            )
                        }
                        ExportProfileRuleCommand::Delete {
                            profile,
                            index,
                            dry_run,
                            no_commit,
                        } => run_export_profile_rule_delete(
                            &paths,
                            cli.output,
                            profile,
                            *index,
                            ConfigMutationOptions {
                                apply_mode: if *dry_run {
                                    ApplyMode::DryRun
                                } else {
                                    ApplyMode::Apply
                                },
                                commit_mode: if *no_commit {
                                    CommitMode::Skip
                                } else {
                                    CommitMode::Auto
                                },
                                quiet: cli.quiet,
                            },
                        ),
                        ExportProfileRuleCommand::Move {
                            profile,
                            index,
                            before,
                            after,
                            last,
                            dry_run,
                            no_commit,
                        } => run_export_profile_rule_move(
                            &paths,
                            cli.output,
                            profile,
                            ExportProfileRuleMoveRequest {
                                index: *index,
                                before: *before,
                                after: *after,
                                last: *last,
                            },
                            ConfigMutationOptions {
                                apply_mode: if *dry_run {
                                    ApplyMode::DryRun
                                } else {
                                    ApplyMode::Apply
                                },
                                commit_mode: if *no_commit {
                                    CommitMode::Skip
                                } else {
                                    CommitMode::Auto
                                },
                                quiet: cli.quiet,
                            },
                        ),
                    },
                },
                ExportCommand::Markdown {
                    query,
                    transforms,
                    path,
                    title,
                } => {
                    let report = execute_export_query(
                        &paths,
                        query.query.as_deref(),
                        query.query_json.as_deref(),
                        read_filter.as_ref(),
                    )
                    .map_err(CliError::operation)?;
                    let transform_rules = build_content_transform_rules(
                        &transforms.exclude_callouts,
                        &transforms.exclude_headings,
                        &transforms.exclude_frontmatter_keys,
                        &transforms.exclude_inline_fields,
                        &transforms.replace_rules,
                    )
                    .map_err(CliError::operation)?;
                    let prepared = prepare_export_data(
                        &paths,
                        &report,
                        read_filter.as_ref(),
                        transform_rules.as_deref(),
                    )
                    .map_err(CliError::operation)?;
                    let payload = render_markdown_export_payload(&prepared.notes, title.as_deref());
                    let summary = MarkdownExportSummary {
                        path: path
                            .as_ref()
                            .map_or_else(String::new, |path| path.display().to_string()),
                        result_count: prepared.notes.len(),
                    };
                    write_text_export(cli.output, path.as_ref(), &payload, &summary)
                }
                ExportCommand::Json {
                    query,
                    transforms,
                    path,
                    pretty,
                } => {
                    let report = execute_export_query(
                        &paths,
                        query.query.as_deref(),
                        query.query_json.as_deref(),
                        read_filter.as_ref(),
                    )
                    .map_err(CliError::operation)?;
                    let transform_rules = build_content_transform_rules(
                        &transforms.exclude_callouts,
                        &transforms.exclude_headings,
                        &transforms.exclude_frontmatter_keys,
                        &transforms.exclude_inline_fields,
                        &transforms.replace_rules,
                    )
                    .map_err(CliError::operation)?;
                    let prepared = prepare_export_data(
                        &paths,
                        &report,
                        read_filter.as_ref(),
                        transform_rules.as_deref(),
                    )
                    .map_err(CliError::operation)?;
                    let payload = render_json_export_payload(&report, &prepared.notes, *pretty)
                        .map_err(CliError::operation)?;
                    let summary = JsonExportSummary {
                        path: path
                            .as_ref()
                            .map_or_else(String::new, |path| path.display().to_string()),
                        result_count: prepared.notes.len(),
                    };
                    write_text_export(cli.output, path.as_ref(), &payload, &summary)
                }
                ExportCommand::Csv { query, path } => {
                    let report = execute_export_query(
                        &paths,
                        query.query.as_deref(),
                        query.query_json.as_deref(),
                        read_filter.as_ref(),
                    )
                    .map_err(CliError::operation)?;
                    let payload =
                        render_csv_export_payload(&report).map_err(CliError::operation)?;
                    let summary = CsvExportSummary {
                        path: path
                            .as_ref()
                            .map_or_else(String::new, |path| path.display().to_string()),
                        result_count: report.notes.len(),
                    };
                    write_text_export(cli.output, path.as_ref(), &payload, &summary)
                }
                ExportCommand::Graph { format, path } => {
                    let report =
                        vulcan_core::export_graph_with_filter(&paths, read_filter.as_ref())
                            .map_err(CliError::operation)?;
                    write_graph_export(cli.output, &report, *format, path.as_ref())
                }
                ExportCommand::Epub {
                    query,
                    transforms,
                    path,
                    title,
                    author,
                    toc,
                    backlinks,
                    frontmatter,
                } => {
                    let report = execute_export_query(
                        &paths,
                        query.query.as_deref(),
                        query.query_json.as_deref(),
                        read_filter.as_ref(),
                    )
                    .map_err(CliError::operation)?;
                    let transform_rules = build_content_transform_rules(
                        &transforms.exclude_callouts,
                        &transforms.exclude_headings,
                        &transforms.exclude_frontmatter_keys,
                        &transforms.exclude_inline_fields,
                        &transforms.replace_rules,
                    )
                    .map_err(CliError::operation)?;
                    let prepared = prepare_export_data(
                        &paths,
                        &report,
                        read_filter.as_ref(),
                        transform_rules.as_deref(),
                    )
                    .map_err(CliError::operation)?;
                    let summary = app_write_epub_export(
                        &paths,
                        path,
                        &prepared.notes,
                        &prepared.links,
                        AppEpubExportOptions {
                            title: title.as_deref(),
                            author: author.as_deref(),
                            backlinks: *backlinks,
                            frontmatter: *frontmatter,
                            toc_style: export_epub_toc_style_config_from_cli(Some(*toc))
                                .unwrap_or(ExportEpubTocStyleConfig::Tree),
                        },
                        AppEpubRenderCallbacks {
                            render_dataview_block: &render_epub_dataview_block_markdown,
                            render_base_embed: &render_epub_base_embed_markdown,
                            render_inline_value: &render_dataview_inline_value,
                        },
                    )
                    .map_err(CliError::operation)?;
                    match cli.output {
                        OutputFormat::Human | OutputFormat::Markdown => {
                            println!("{}", summary.path);
                            Ok(())
                        }
                        OutputFormat::Json => print_json(&summary),
                    }
                }
                ExportCommand::Zip {
                    query,
                    transforms,
                    path,
                } => {
                    let report = execute_export_query(
                        &paths,
                        query.query.as_deref(),
                        query.query_json.as_deref(),
                        read_filter.as_ref(),
                    )
                    .map_err(CliError::operation)?;
                    let transform_rules = build_content_transform_rules(
                        &transforms.exclude_callouts,
                        &transforms.exclude_headings,
                        &transforms.exclude_frontmatter_keys,
                        &transforms.exclude_inline_fields,
                        &transforms.replace_rules,
                    )
                    .map_err(CliError::operation)?;
                    let prepared = prepare_export_data(
                        &paths,
                        &report,
                        read_filter.as_ref(),
                        transform_rules.as_deref(),
                    )
                    .map_err(CliError::operation)?;
                    let summary =
                        write_zip_export(&paths, path, &report, &prepared.notes, &prepared.links)
                            .map_err(CliError::operation)?;
                    match cli.output {
                        OutputFormat::Human | OutputFormat::Markdown => {
                            println!("{}", summary.path);
                            Ok(())
                        }
                        OutputFormat::Json => print_json(&summary),
                    }
                }
                ExportCommand::Sqlite { query, path } => {
                    let report = execute_export_query(
                        &paths,
                        query.query.as_deref(),
                        query.query_json.as_deref(),
                        read_filter.as_ref(),
                    )
                    .map_err(CliError::operation)?;
                    let notes =
                        load_exported_notes(&paths, &report).map_err(CliError::operation)?;
                    let links = load_export_links(&paths, &notes).map_err(CliError::operation)?;
                    let summary = write_sqlite_export(path, &report, &notes, &links)
                        .map_err(CliError::operation)?;
                    match cli.output {
                        OutputFormat::Human | OutputFormat::Markdown => {
                            println!("{}", summary.path);
                            Ok(())
                        }
                        OutputFormat::Json => print_json(&summary),
                    }
                }
                ExportCommand::SearchIndex { path, pretty } => {
                    let report = export_static_search_index(&paths).map_err(CliError::operation)?;
                    print_static_search_index_report(cli.output, &report, path.as_ref(), *pretty)
                }
            }
        }
        Command::Config { ref command } => {
            commands::config::handle_config_command(cli, &paths, command, stdout_is_tty)
        }
        Command::Daily { ref command } => commands::periodic::handle_daily_command(
            cli,
            &paths,
            command,
            interactive_note_selection,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Today { no_edit, no_commit } => commands::periodic::handle_today_command(
            cli,
            &paths,
            no_edit,
            no_commit,
            interactive_note_selection,
        ),
        Command::Git { ref command } => commands::runtime::handle_git_command(cli, &paths, command),
        Command::Run {
            ref script,
            script_mode,
            ref eval,
            ref eval_file,
            ref timeout,
            ref sandbox,
            no_startup,
        } => commands::runtime::handle_run_command(
            cli,
            &paths,
            &commands::runtime::RunArgs {
                script: script.as_deref(),
                script_mode,
                eval,
                eval_file: eval_file.as_deref(),
                timeout: timeout.as_deref(),
                sandbox: sandbox.as_deref(),
                no_startup,
            },
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Web { ref command } => commands::runtime::handle_web_command(
            cli,
            &paths,
            command,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Periodic {
            ref command,
            ref period_type,
            ref date,
            no_edit,
            no_commit,
        } => commands::periodic::handle_periodic_command(
            cli,
            &paths,
            command.as_ref(),
            period_type.as_deref(),
            date.as_deref(),
            no_edit,
            no_commit,
            interactive_note_selection,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        Command::Changes {
            ref checkpoint,
            ref export,
        } => {
            let report = query_change_report(
                &paths,
                &checkpoint.as_ref().map_or(ChangeAnchor::LastScan, |name| {
                    ChangeAnchor::Checkpoint(name.clone())
                }),
            )
            .map_err(CliError::operation)?;
            let export = resolve_cli_export(export)?;
            print_change_report(
                cli.output,
                &report,
                &list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        Command::Diff {
            ref note,
            ref since,
        } => {
            if let Some(note) = note.as_deref() {
                let guard = selected_permission_guard(cli, &paths)?;
                let resolved = resolve_note_reference(&paths, note).map_err(CliError::operation)?;
                guard
                    .check_read_path(&resolved.path)
                    .map_err(CliError::operation)?;
                guard.check_git().map_err(CliError::operation)?;
            }
            let report = run_diff_command(
                &paths,
                note.as_deref(),
                since.as_deref(),
                interactive_note_selection,
            )?;
            print_diff_report(cli.output, &report)
        }
        Command::Inbox {
            ref text,
            ref file,
            no_commit,
        } => {
            let report =
                run_inbox_command(&paths, text.as_deref(), file.as_ref(), no_commit, cli.quiet)?;
            print_inbox_report(cli.output, &report)
        }
        Command::Template {
            ref command,
            ref name,
            list,
            ref path,
            ref render,
            no_commit,
        } => {
            let result = match command {
                Some(TemplateSubcommand::List) => run_template_command(
                    &paths,
                    None,
                    true,
                    None,
                    render.engine,
                    &render.vars,
                    no_commit,
                    cli.quiet,
                    stdout_is_tty,
                )?,
                Some(TemplateSubcommand::Show { name: tname }) => {
                    return run_template_show_command(&paths, tname, cli.output);
                }
                Some(TemplateSubcommand::Insert {
                    template,
                    note,
                    prepend,
                    append: _,
                    render,
                    no_commit,
                }) => TemplateCommandResult::Insert(run_template_insert_command(
                    &paths,
                    template,
                    note.as_deref(),
                    if *prepend {
                        TemplateInsertMode::Prepend
                    } else {
                        TemplateInsertMode::Append
                    },
                    render.engine,
                    &render.vars,
                    *no_commit,
                    cli.quiet,
                    interactive_note_selection,
                )?),
                Some(TemplateSubcommand::Preview {
                    template,
                    path,
                    render,
                }) => TemplateCommandResult::Preview(run_template_preview_command(
                    &paths,
                    template,
                    path.as_deref(),
                    render.engine,
                    &render.vars,
                )?),
                None => run_template_command(
                    &paths,
                    name.as_deref(),
                    list,
                    path.as_deref(),
                    render.engine,
                    &render.vars,
                    no_commit,
                    cli.quiet,
                    stdout_is_tty,
                )?,
            };

            match result {
                TemplateCommandResult::List(report) => {
                    print_template_list_report(cli.output, &report)
                }
                TemplateCommandResult::Create(report) => {
                    print_template_create_report(cli.output, &report)
                }
                TemplateCommandResult::Insert(report) => {
                    print_template_insert_report(cli.output, &report)
                }
                TemplateCommandResult::Preview(report) => print_template_preview_report(
                    cli.output,
                    &report,
                    stdout_is_tty,
                    use_stdout_color,
                ),
            }
        }
        Command::LinkMentions {
            ref note,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report =
                link_mentions(&paths, note.as_deref(), dry_run).map_err(CliError::operation)?;
            if !dry_run {
                let changed_paths = refactor_changed_files(&report);
                auto_commit
                    .commit(
                        &paths,
                        "link-mentions",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                let _ = crate::plugins::dispatch_plugin_event(
                    &paths,
                    cli.permissions.as_deref(),
                    PluginEvent::OnRefactor,
                    &serde_json::json!({
                        "kind": PluginEvent::OnRefactor,
                        "action": "link-mentions",
                        "paths": changed_paths,
                    }),
                    cli.quiet,
                );
            }
            print_refactor_report(cli.output, &report)
        }
        Command::Rewrite {
            ref filters,
            stdin,
            ref find,
            ref replace,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let selection = resolve_bulk_note_selection(filters, stdin)?;
            let report = match &selection {
                BulkNoteSelection::Filters(filters) => {
                    bulk_replace(&paths, filters, find, replace, dry_run)
                }
                BulkNoteSelection::Paths(note_paths) => {
                    vulcan_core::bulk_replace_on_paths(&paths, note_paths, find, replace, dry_run)
                }
            }
            .map_err(CliError::operation)?;
            if !dry_run {
                let changed_paths = refactor_changed_files(&report);
                auto_commit
                    .commit(
                        &paths,
                        "rewrite",
                        &changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
                let _ = crate::plugins::dispatch_plugin_event(
                    &paths,
                    cli.permissions.as_deref(),
                    PluginEvent::OnRefactor,
                    &serde_json::json!({
                        "kind": PluginEvent::OnRefactor,
                        "action": "rewrite",
                        "paths": changed_paths,
                    }),
                    cli.quiet,
                );
            }
            print_refactor_report(cli.output, &report)
        }
        Command::Automation { ref command } => match command {
            AutomationCommand::List => {
                let reports = list_saved_reports(&paths).map_err(CliError::operation)?;
                print_saved_report_list(
                    cli.output,
                    &reports,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                )
            }
            AutomationCommand::Run { fail_on_issues, .. } => {
                let report = execute_automation_run(
                    &paths,
                    cli.provider.as_ref(),
                    cli.output,
                    use_stderr_color,
                    &list_controls,
                    command,
                )?;
                let report_failures = report
                    .reports
                    .as_ref()
                    .is_some_and(|batch| batch.failed > 0);
                print_automation_run_report(cli.output, &report)?;
                if report_failures {
                    Err(CliError::operation(
                        "one or more automation report actions failed",
                    ))
                } else if *fail_on_issues && report.issues_detected {
                    Err(CliError::issues("automation detected issues"))
                } else {
                    Ok(())
                }
            }
        },
        Command::Vectors { ref command } => commands::vectors::handle_vectors_command(
            cli,
            &paths,
            command,
            interactive_note_selection,
            &list_controls,
            stdout_is_tty,
            use_stdout_color,
            use_stderr_color,
        ),
        Command::Scan { full, no_commit } => {
            selected_permission_guard(cli, &paths)?
                .check_index()
                .map_err(CliError::operation)?;
            let auto_commit = AutoCommitPolicy::for_scan(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let mut progress = (cli.output == OutputFormat::Human)
                .then(|| ScanProgressReporter::new(use_stderr_color));
            let summary = scan_vault_with_progress(
                &paths,
                if full {
                    ScanMode::Full
                } else {
                    ScanMode::Incremental
                },
                |event| {
                    if let Some(progress) = progress.as_mut() {
                        progress.record(&event);
                    }
                },
            )
            .map_err(CliError::operation)?;
            if summary.added + summary.updated + summary.deleted > 0 {
                auto_commit
                    .commit(&paths, "scan", &[], cli.permissions.as_deref(), cli.quiet)
                    .map_err(CliError::operation)?;
            }
            let _ = crate::plugins::dispatch_plugin_event(
                &paths,
                cli.permissions.as_deref(),
                PluginEvent::OnScanComplete,
                &serde_json::json!({
                    "kind": PluginEvent::OnScanComplete,
                    "mode": if full { "full" } else { "incremental" },
                    "summary": &summary,
                }),
                cli.quiet,
            );
            print_scan_summary(cli.output, &summary, use_stdout_color);
            Ok(())
        }
    }
}

fn print_search_report(
    output: OutputFormat,
    report: &SearchReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_hits = paginated_items(&report.hits, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = search_hit_rows(report, visible_hits);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!(
                    "{} {} {}",
                    palette.cyan("Search hits for"),
                    palette.bold(&report.query),
                    palette.dim(match report.mode {
                        vulcan_core::search::SearchMode::Keyword => "keyword",
                        vulcan_core::search::SearchMode::Hybrid => "hybrid",
                    }),
                );
            }
            if let Some(plan) = report.plan.as_ref() {
                print_search_plan(plan, palette);
            }
            if visible_hits.is_empty() {
                println!("No search hits.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, hit) in visible_hits.iter().enumerate() {
                    print_search_hit(index, hit, palette);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn run_render_command(
    paths: &VaultPaths,
    path: Option<&PathBuf>,
    mode: RenderMode,
) -> Result<RenderReport, CliError> {
    let (path, source, source_path) = match path {
        Some(path) if path.as_os_str() == "-" => {
            let mut source = String::new();
            io::stdin()
                .read_to_string(&mut source)
                .map_err(CliError::operation)?;
            (None, source, None)
        }
        Some(path) => {
            let contents = fs::read_to_string(path).map_err(CliError::operation)?;
            let absolute = fs::canonicalize(path).map_err(CliError::operation)?;
            let source_path = paths
                .relative_to_vault(&absolute)
                .map(|relative| path_buf_to_slash_string(&relative));
            (Some(path.display().to_string()), contents, source_path)
        }
        None => {
            if io::stdin().is_terminal() {
                return Err(CliError::operation(
                    "`vulcan render` requires a file path or piped stdin",
                ));
            }
            let mut source = String::new();
            io::stdin()
                .read_to_string(&mut source)
                .map_err(CliError::operation)?;
            (None, source, None)
        }
    };

    let rendered = match mode {
        RenderMode::Terminal => terminal_markdown::render_terminal_markdown(&source, false),
        RenderMode::Html => source_path.as_deref().map_or_else(
            || {
                render_vault_html(
                    paths,
                    &source,
                    &HtmlRenderOptions {
                        full_document: true,
                        ..HtmlRenderOptions::default()
                    },
                )
                .html
            },
            |relative_path| render_note_html(paths, relative_path, &source).html,
        ),
    };
    Ok(RenderReport {
        path,
        source,
        rendered,
        mode: match mode {
            RenderMode::Terminal => "terminal",
            RenderMode::Html => "html",
        }
        .to_string(),
    })
}

pub(crate) fn print_markdown_output(
    output: OutputFormat,
    markdown: &str,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => Err(CliError::operation(
            "markdown helper cannot print JSON output directly",
        )),
        OutputFormat::Human => {
            let rendered = if stdout_is_tty {
                terminal_markdown::render_terminal_markdown(markdown, use_color)
            } else {
                markdown.to_string()
            };
            if rendered.is_empty() {
                return Ok(());
            }
            print!("{rendered}");
            if !rendered.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        OutputFormat::Markdown => {
            if markdown.is_empty() {
                return Ok(());
            }
            print!("{markdown}");
            if !markdown.ends_with('\n') {
                println!();
            }
            Ok(())
        }
    }
}

fn print_render_report(
    output: OutputFormat,
    report: &RenderReport,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human => {
            let rendered = if report.mode == "terminal" && stdout_is_tty {
                terminal_markdown::render_terminal_markdown(&report.source, use_color)
            } else {
                report.rendered.clone()
            };
            print!("{rendered}");
            if !rendered.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        OutputFormat::Markdown => {
            let rendered = if report.mode == "html" {
                report.rendered.as_str()
            } else {
                report.source.as_str()
            };
            print!("{rendered}");
            if !rendered.ends_with('\n') {
                println!();
            }
            Ok(())
        }
    }
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

fn print_describe_report(
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

fn print_help_command(
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

fn print_saved_report_list(
    output: OutputFormat,
    reports: &[SavedReportSummary],
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    let visible_reports = paginated_items(reports, list_controls);
    let rows = saved_report_summary_rows(visible_reports);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Saved reports"));
            }
            if visible_reports.is_empty() {
                println!("No saved reports.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for report in visible_reports {
                    let description = report
                        .description
                        .as_deref()
                        .map(|description| format!(": {description}"))
                        .unwrap_or_default();
                    let export = report
                        .export
                        .as_ref()
                        .map(|export| format!(" -> {}", export.path))
                        .unwrap_or_default();
                    println!(
                        "- {} [{:?}]{}{}",
                        report.name, report.kind, description, export
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_saved_report_definition(
    output: OutputFormat,
    definition: &SavedReportDefinition,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Saved report: {}", definition.name);
            println!("Kind: {:?}", definition.query.kind());
            if let Some(description) = definition.description.as_deref() {
                println!("Description: {description}");
            }
            if let Some(fields) = definition.fields.as_deref() {
                println!("Fields: {}", fields.join(", "));
            }
            if let Some(limit) = definition.limit {
                println!("Limit: {limit}");
            }
            if let Some(export) = definition.export.as_ref() {
                println!("Export: {:?} -> {}", export.format, export.path);
            }
            match &definition.query {
                SavedReportQuery::Search {
                    query,
                    mode,
                    tag,
                    path_prefix,
                    has_property,
                    filters,
                    context_size,
                    sort,
                    match_case,
                    raw_query,
                    fuzzy,
                } => {
                    println!("Query: {query}");
                    println!("Mode: {mode:?}");
                    if let Some(tag) = tag.as_deref() {
                        println!("Tag: {tag}");
                    }
                    if let Some(path_prefix) = path_prefix.as_deref() {
                        println!("Path prefix: {path_prefix}");
                    }
                    if let Some(has_property) = has_property.as_deref() {
                        println!("Has property: {has_property}");
                    }
                    if !filters.is_empty() {
                        println!("Filters: {}", filters.join(" | "));
                    }
                    println!("Context size: {context_size}");
                    if let Some(sort) = sort {
                        println!("Sort: {}", display_search_sort(*sort));
                    }
                    if *match_case == Some(true) {
                        println!("Match case: true");
                    }
                    if *raw_query {
                        println!("Raw query: true");
                    }
                    if *fuzzy {
                        println!("Fuzzy fallback: true");
                    }
                }
                SavedReportQuery::Notes {
                    filters,
                    sort_by,
                    sort_descending,
                } => {
                    if !filters.is_empty() {
                        println!("Filters: {}", filters.join(" | "));
                    }
                    if let Some(sort_by) = sort_by.as_deref() {
                        println!(
                            "Sort: {}{}",
                            sort_by,
                            if *sort_descending { " desc" } else { "" }
                        );
                    }
                }
                SavedReportQuery::Bases { file } => {
                    println!("Base file: {file}");
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(definition),
    }
}

fn print_saved_report_delete_report(
    output: OutputFormat,
    report: &SavedReportDeleteReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Deleted saved report {} ({})",
                report.name,
                report.path.display()
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_notes_report(
    output: OutputFormat,
    report: &NotesReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = note_rows(report, visible_notes);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Notes query"));
            }
            if visible_notes.is_empty() {
                println!("No notes matched.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    print_note(note);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn query_report_rows(report: &QueryReport, notes: &[&NoteRecord]) -> Vec<Value> {
    let query_value = serde_json::to_value(&report.query).unwrap_or(Value::Null);
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "file_name": note.file_name,
                "file_ext": note.file_ext,
                "file_mtime": note.file_mtime,
                "tags": note.tags,
                "starred": note.starred,
                "properties": note.properties,
                "inline_expressions": note.inline_expressions,
                "query": query_value,
            })
        })
        .collect()
}

fn query_path_rows(notes: &[&NoteRecord]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| Value::String(note.document_path.clone()))
        .collect()
}

fn query_detail_rows(
    paths: &VaultPaths,
    report: &QueryReport,
    notes: &[&NoteRecord],
) -> Vec<Value> {
    let query_value = serde_json::to_value(&report.query).unwrap_or(Value::Null);
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "properties": note.properties,
                "preview_lines": load_note_preview_lines(paths, note.document_path.as_str(), 5),
                "query": query_value,
            })
        })
        .collect()
}

fn load_note_preview_lines(paths: &VaultPaths, document_path: &str, limit: usize) -> Vec<String> {
    fs::read_to_string(paths.vault_root().join(document_path))
        .ok()
        .map(|content| {
            content
                .lines()
                .map(str::trim_end)
                .filter(|line| !line.trim().is_empty())
                .take(limit)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn print_query_detail_human(paths: &VaultPaths, notes: &[&NoteRecord]) {
    for note in notes {
        println!("- {}", note.document_path);
        if let Some(properties) = note.properties.as_object() {
            if !properties.is_empty() {
                let summary = properties
                    .iter()
                    .take(6)
                    .map(|(key, value)| format!("{key}={}", render_human_value(value)))
                    .collect::<Vec<_>>();
                if !summary.is_empty() {
                    println!("  properties: {}", summary.join(" | "));
                }
            }
        }
        for line in load_note_preview_lines(paths, note.document_path.as_str(), 5) {
            println!("  {line}");
        }
        println!();
    }
}

fn glob_pattern_regex(pattern: &str) -> Result<Regex, CliError> {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' => {
                if chars.peek().is_some_and(|next| *next == '*') {
                    chars.next();
                    regex.push_str(".*");
                } else {
                    regex.push_str("[^/]*");
                }
            }
            '?' => regex.push_str("[^/]"),
            other => regex.push_str(&regex::escape(&other.to_string())),
        }
    }
    regex.push('$');
    Regex::new(&regex)
        .map_err(|error| CliError::operation(format!("invalid glob pattern: {error}")))
}

fn filter_notes_by_glob<'a>(
    notes: &'a [NoteRecord],
    glob: Option<&str>,
) -> Result<Vec<&'a NoteRecord>, CliError> {
    let Some(glob) = glob else {
        return Ok(notes.iter().collect());
    };
    let matcher = glob_pattern_regex(glob)?;
    Ok(notes
        .iter()
        .filter(|note| matcher.is_match(&note.document_path))
        .collect())
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
struct QueryReportRenderOptions<'a> {
    format: QueryFormatArg,
    glob: Option<&'a str>,
    explain: bool,
    verbose: bool,
    stdout_is_tty: bool,
    use_color: bool,
    no_header: bool,
    export: Option<&'a ResolvedExport>,
}

fn should_render_query_ast(output: OutputFormat, explain: bool, verbose: bool) -> bool {
    explain || (verbose && matches!(output, OutputFormat::Human | OutputFormat::Markdown))
}

#[allow(clippy::too_many_lines)]
fn print_query_report(
    paths: &VaultPaths,
    output: OutputFormat,
    report: &QueryReport,
    list_controls: &ListOutputControls,
    options: QueryReportRenderOptions<'_>,
) -> Result<(), CliError> {
    let filtered_notes = filter_notes_by_glob(&report.notes, options.glob)?;
    let start = list_controls.offset.min(filtered_notes.len());
    let end = list_controls.limit.map_or(filtered_notes.len(), |limit| {
        start.saturating_add(limit).min(filtered_notes.len())
    });
    let visible_notes = &filtered_notes[start..end];
    let palette = AnsiPalette::new(options.use_color);

    // TSV/CSV: write directly to stdout regardless of --output mode.
    if matches!(options.format, QueryFormatArg::Tsv | QueryFormatArg::Csv) {
        let rows = query_report_rows(report, visible_notes);
        let fields = list_controls.fields.as_deref();
        let headers = csv_headers(&rows, fields);
        let delimiter = if matches!(options.format, QueryFormatArg::Tsv) {
            b'\t'
        } else {
            b','
        };
        let mut writer = csv::WriterBuilder::new()
            .delimiter(delimiter)
            .from_writer(io::stdout().lock());
        if !options.no_header {
            writer
                .write_record(headers.iter().map(String::as_str))
                .map_err(CliError::operation)?;
        }
        for row in &rows {
            let selected = select_fields(row.clone(), fields);
            let record = headers
                .iter()
                .map(|h| csv_cell_for_value(selected.get(h)))
                .collect::<Vec<_>>();
            writer.write_record(record).map_err(CliError::operation)?;
        }
        writer.flush().map_err(CliError::operation)?;
        export_rows(&rows, fields, options.export)?;
        return Ok(());
    }

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if should_render_query_ast(output, options.explain, options.verbose) {
                let ast_json = serde_json::to_string_pretty(&report.query)
                    .unwrap_or_else(|_| "{}".to_string());
                println!("{}", palette.cyan("Query AST:"));
                println!("{ast_json}");
                println!();
            }
            match options.format {
                QueryFormatArg::Count => {
                    println!("{}", visible_notes.len());
                    return Ok(());
                }
                QueryFormatArg::Paths => {
                    let rows = query_path_rows(visible_notes);
                    for note in visible_notes {
                        println!("{}", note.document_path);
                    }
                    export_rows(&rows, list_controls.fields.as_deref(), options.export)?;
                    return Ok(());
                }
                QueryFormatArg::Detail => {
                    if visible_notes.is_empty() {
                        println!("No notes matched.");
                        return Ok(());
                    }
                    let rows = query_detail_rows(paths, report, visible_notes);
                    print_query_detail_human(paths, visible_notes);
                    export_rows(&rows, list_controls.fields.as_deref(), options.export)?;
                    return Ok(());
                }
                QueryFormatArg::Table => {}
                QueryFormatArg::Tsv | QueryFormatArg::Csv => {
                    unreachable!("tsv/csv handled above")
                }
            }
            let rows = query_report_rows(report, visible_notes);
            if visible_notes.is_empty() {
                println!("No notes matched.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                if options.stdout_is_tty
                    && matches!(options.format, QueryFormatArg::Table)
                    && !fields.is_empty()
                {
                    print_aligned_table(&rows, fields, options.no_header, options.use_color);
                } else {
                    for row in &rows {
                        print_selected_human_fields(row, fields);
                    }
                }
            } else {
                for note in visible_notes {
                    print_note(note);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), options.export)?;
            Ok(())
        }
        OutputFormat::Json => {
            if matches!(options.format, QueryFormatArg::Count) {
                let payload = serde_json::json!({ "count": visible_notes.len() });
                export_rows(
                    std::slice::from_ref(&payload),
                    list_controls.fields.as_deref(),
                    options.export,
                )?;
                return print_json(&payload);
            }
            let rows = match options.format {
                QueryFormatArg::Table => query_report_rows(report, visible_notes),
                QueryFormatArg::Paths => query_path_rows(visible_notes),
                QueryFormatArg::Detail => query_detail_rows(paths, report, visible_notes),
                QueryFormatArg::Count => unreachable!("count handled above"),
                QueryFormatArg::Tsv | QueryFormatArg::Csv => {
                    unreachable!("tsv/csv handled above")
                }
            };
            if should_render_query_ast(output, options.explain, options.verbose) {
                let payload = serde_json::json!({
                    "query": report.query,
                    "notes": rows,
                });
                export_rows(
                    std::slice::from_ref(&payload),
                    list_controls.fields.as_deref(),
                    options.export,
                )?;
                print_json(&payload)
            } else {
                export_rows(&rows, list_controls.fields.as_deref(), options.export)?;
                print_json_lines(rows, list_controls.fields.as_deref())
            }
        }
    }
}

fn print_rebuild_report(
    output: OutputFormat,
    report: &RebuildReport,
    use_color: bool,
) -> Result<(), CliError> {
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!(
                    "{}: would rebuild {} discovered files with {} cached documents",
                    palette.cyan("Dry run"),
                    report.discovered,
                    report.existing_documents
                );
            } else if let Some(summary) = report.summary.as_ref() {
                println!(
                    "{} from {} files: {} added, {} updated, {} unchanged, {} deleted",
                    palette.cyan("Rebuilt cache"),
                    summary.discovered,
                    palette.green(&summary.added.to_string()),
                    palette.yellow(&summary.updated.to_string()),
                    summary.unchanged,
                    palette.red(&summary.deleted.to_string())
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&report),
    }
}

fn print_repair_fts_report(output: OutputFormat, report: &RepairFtsReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!(
                    "Dry run: would rebuild FTS rows for {} chunks across {} documents",
                    report.indexed_chunks, report.indexed_documents
                );
            } else {
                println!(
                    "Rebuilt FTS rows for {} chunks across {} documents",
                    report.indexed_chunks, report.indexed_documents
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&report),
    }
}

fn print_watch_report(output: OutputFormat, report: &WatchReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.startup {
                println!(
                    "Initial scan: {} added, {} updated, {} unchanged, {} deleted",
                    report.summary.added,
                    report.summary.updated,
                    report.summary.unchanged,
                    report.summary.deleted
                );
            } else {
                println!(
                    "Watch update ({} events, {} paths): {} added, {} updated, {} unchanged, {} deleted",
                    report.event_count,
                    report.paths.len(),
                    report.summary.added,
                    report.summary.updated,
                    report.summary.unchanged,
                    report.summary.deleted
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_index_report(
    output: OutputFormat,
    report: &VectorIndexReport,
    use_color: bool,
) -> Result<(), CliError> {
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{} vectors with {}:{} {}: {} indexed, {} skipped, {} failed {}",
                if report.dry_run {
                    palette.cyan("Dry run for")
                } else {
                    palette.cyan("Indexed")
                },
                report.provider_name,
                report.model_name,
                palette.dim(&format!(
                    "(dims {}, batch size {}, concurrency {})",
                    report.dimensions, report.batch_size, report.max_concurrency
                )),
                palette.green(&report.indexed.to_string()),
                report.skipped,
                palette.red(&report.failed.to_string()),
                palette.dim(&format!("in {:.3}s", report.elapsed_seconds))
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_queue_report(
    output: OutputFormat,
    report: &VectorQueueReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Vector queue {}:{}: {} pending, {} indexed, {} stale{}",
                report.provider_name,
                report.model_name,
                report.pending_chunks,
                report.indexed_chunks,
                report.stale_vectors,
                if report.model_mismatch {
                    " (model mismatch)"
                } else {
                    ""
                }
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_repair_report(
    output: OutputFormat,
    report: &VectorRepairReport,
    use_color: bool,
) -> Result<(), CliError> {
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let status = if report.dry_run {
                palette.cyan("Dry run")
            } else if report.repaired {
                palette.green("Repaired")
            } else {
                palette.cyan("Checked")
            };
            println!(
                "{} vectors for {}:{}: {} pending, {} stale{}",
                status,
                report.provider_name,
                report.model_name,
                report.pending_chunks,
                report.stale_vectors,
                if report.model_mismatch {
                    " (model mismatch)"
                } else {
                    ""
                }
            );
            if let Some(index_report) = report.index_report.as_ref() {
                println!(
                    "{} indexed, {} skipped, {} failed",
                    index_report.indexed, index_report.skipped, index_report.failed
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_neighbors_report(
    output: OutputFormat,
    report: &VectorNeighborsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_hits = paginated_items(&report.hits, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = vector_neighbor_rows(report, visible_hits);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                if let Some(query_text) = report.query_text.as_deref() {
                    println!(
                        "{} {}",
                        palette.cyan("Vector neighbors for"),
                        palette.bold(query_text)
                    );
                } else if let Some(note_path) = report.note_path.as_deref() {
                    println!(
                        "{} {}",
                        palette.cyan("Vector neighbors for note"),
                        palette.bold(note_path)
                    );
                }
            }
            if visible_hits.is_empty() {
                println!("No vector neighbors.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, hit) in visible_hits.iter().enumerate() {
                    print_vector_neighbor(index, hit, palette);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_related_notes_report(
    output: OutputFormat,
    report: &RelatedNotesReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_hits = paginated_items(&report.hits, list_controls);
    let rows = related_note_rows(report, visible_hits);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!(
                    "{} {}",
                    palette.cyan("Related notes for"),
                    palette.bold(&report.note_path)
                );
            }
            if visible_hits.is_empty() {
                println!("No related notes.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, hit) in visible_hits.iter().enumerate() {
                    println!(
                        "{}. {} ({:.3}, {} chunks)",
                        index + 1,
                        hit.document_path,
                        hit.similarity,
                        hit.matched_chunks
                    );
                    if !hit.heading_path.is_empty() {
                        println!("   {}", hit.heading_path.join(" > "));
                    }
                    println!("   {}", hit.snippet);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_vector_duplicates_report(
    output: OutputFormat,
    report: &VectorDuplicatesReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_pairs = paginated_items(&report.pairs, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = vector_duplicate_rows(report, visible_pairs);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Vector duplicates"));
            }
            if visible_pairs.is_empty() {
                println!("No duplicate pairs.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for pair in visible_pairs {
                    print_vector_duplicate(pair);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_vector_models_report(
    output: OutputFormat,
    models: &[StoredModelInfo],
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows: Vec<Value> = models
        .iter()
        .map(|m| {
            serde_json::json!({
                "cache_key": m.cache_key,
                "provider": m.provider_name,
                "model": m.model_name,
                "dimensions": m.dimensions,
                "normalized": m.normalized,
                "chunks": m.chunk_count,
                "active": m.is_active,
            })
        })
        .collect();

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let palette = AnsiPalette::new(use_color);
            if stdout_is_tty {
                println!("{}", palette.cyan("Vector models"));
            }
            if models.is_empty() {
                println!("No stored models.");
                return Ok(());
            }
            for model in models {
                let active_marker = if model.is_active { " (active)" } else { "" };
                println!(
                    "{}{}\n  provider: {}  model: {}  dimensions: {}  normalized: {}  chunks: {}",
                    palette.bold(&model.cache_key),
                    active_marker,
                    model.provider_name,
                    model.model_name,
                    model.dimensions,
                    model.normalized,
                    model.chunk_count,
                );
            }
            export_rows(&rows, None, export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, None, export)?;
            print_json_lines(rows, None)
        }
    }
}

fn print_cluster_report(
    output: OutputFormat,
    report: &ClusterReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_clusters = paginated_items(&report.clusters, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = cluster_rows(report, visible_clusters);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                if report.dry_run {
                    println!("Vector clusters (dry run)");
                } else {
                    println!("Vector clusters");
                }
            }
            if visible_clusters.is_empty() {
                println!("No vector clusters.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, cluster) in visible_clusters.iter().enumerate() {
                    print_cluster_summary(index, cluster, palette);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_bases_report(
    output: OutputFormat,
    report: &BasesEvalReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = bases_rows(report);
    let visible_rows = paginated_items(&rows, list_controls);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if output == OutputFormat::Human && stdout_is_tty {
                println!(
                    "{} {}",
                    palette.cyan("Bases eval"),
                    palette.bold(&report.file)
                );
            }
            if rows.is_empty() {
                println!("No bases rows.");
            } else if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                print_markdown_output(
                    output,
                    &render_bases_markdown(report, list_controls),
                    stdout_is_tty,
                    use_color,
                )?;
            }

            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            if list_controls.fields.is_some() {
                print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
            } else {
                print_json(report)
            }
        }
    }
}

fn print_bases_view_edit_report(
    output: OutputFormat,
    report: &BasesViewEditReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Dry run: {}", report.action);
            } else {
                println!("{}", report.action);
            }
            println!(
                "{} views, {} diagnostics",
                report.eval.views.len(),
                report.eval.diagnostics.len()
            );
            for diag in &report.eval.diagnostics {
                let path = diag.path.as_deref().unwrap_or("(root)");
                println!("  warning [{path}]: {}", diag.message);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_bases_create_report(
    output: OutputFormat,
    report: &BasesCreateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Would create {} from {}.", report.path, report.file);
            } else {
                println!("Created {} from {}.", report.path, report.file);
            }

            let view = report
                .view_name
                .as_deref()
                .map_or_else(|| format!("#{}", report.view_index + 1), ToOwned::to_owned);
            println!("View: {view}");
            println!(
                "Folder: {}",
                report.folder.as_deref().unwrap_or("<vault root>")
            );
            println!(
                "Template: {}",
                report.template.as_deref().unwrap_or("<none>")
            );

            if report.properties.is_empty() {
                println!("Properties: <none>");
            } else {
                println!("Properties:");
                for (key, value) in &report.properties {
                    println!("  {key}: {}", render_human_value(value));
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_mention_suggestions_report(
    output: OutputFormat,
    report: &MentionSuggestionsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_suggestions = paginated_items(&report.suggestions, list_controls);
    let rows = mention_suggestion_rows(visible_suggestions);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Mention suggestions"));
            }
            if visible_suggestions.is_empty() {
                println!("No mention suggestions.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for suggestion in visible_suggestions {
                    print_mention_suggestion(suggestion, palette);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_link_suggestions_report(
    output: OutputFormat,
    report: &LinkSuggestionsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_suggestions = paginated_items(&report.suggestions, list_controls);
    let rows = link_suggestion_rows(visible_suggestions);
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Link suggestions"));
            }
            if visible_suggestions.is_empty() {
                println!("No link suggestions.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for suggestion in visible_suggestions {
                    println!(
                        "- {} -> {} [{:.3}, {}, id {}]",
                        suggestion.source_path,
                        suggestion.target_path,
                        suggestion.display_score,
                        suggestion.status.as_str(),
                        suggestion.id
                    );
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_duplicate_suggestions_report(
    output: OutputFormat,
    report: &DuplicateSuggestionsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = duplicate_suggestion_rows(report);
    let visible_rows = paginated_items(&rows, list_controls);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Duplicate suggestions"));
            }
            if rows.is_empty() {
                println!("No duplicate suggestions.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                print_duplicate_groups("Duplicate titles", &report.duplicate_titles);
                print_duplicate_groups("Alias collisions", &report.alias_collisions);
                print_merge_candidates(&report.merge_candidates, palette);
            }
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
        }
    }
}

fn print_links_report(
    output: OutputFormat,
    report: &OutgoingLinksReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_links = paginated_items(&report.links, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = outgoing_link_rows(report, visible_links);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!(
                    "{} {} {}",
                    palette.cyan("Links for"),
                    palette.bold(&report.note_path),
                    palette.dim(&format!("({:?})", report.matched_by))
                );
            }
            if visible_links.is_empty() {
                println!("No outgoing links.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for link in visible_links {
                    print_outgoing_link(link);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_backlinks_report(
    output: OutputFormat,
    report: &BacklinksReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_backlinks = paginated_items(&report.backlinks, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = backlink_rows(report, visible_backlinks);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!(
                    "{} {} {}",
                    palette.cyan("Backlinks for"),
                    palette.bold(&report.note_path),
                    palette.dim(&format!("({:?})", report.matched_by))
                );
            }
            if visible_backlinks.is_empty() {
                println!("No backlinks.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for backlink in visible_backlinks {
                    print_backlink(backlink);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn run_init_command(paths: &VaultPaths, args: &InitArgs) -> Result<InitReport, CliError> {
    let summary = initialize_vault(paths).map_err(CliError::operation)?;
    let support_files = if args.agent_files {
        write_bundled_support_files(paths, false, args.example_tool)?
    } else {
        Vec::new()
    };
    let importable_sources = if args.no_import {
        Vec::new()
    } else {
        discover_config_importers(paths)
            .into_iter()
            .filter_map(|(_, discovery)| discovery.detected.then_some(discovery))
            .collect()
    };
    let imported = if args.import {
        let target = ImportTarget::Shared;
        let mut reports = Vec::new();
        for importer in all_importers()
            .into_iter()
            .filter(|importer| importer.detect(paths))
        {
            reports.push(
                importer
                    .import(paths, target)
                    .map_err(CliError::operation)?,
            );
        }
        annotate_import_conflicts(&mut reports);
        Some(ConfigImportBatchReport {
            dry_run: false,
            target,
            detected_count: reports.len(),
            imported_count: reports.len(),
            updated_count: reports.iter().filter(|report| report.updated).count(),
            reports,
        })
    } else {
        None
    };
    Ok(InitReport {
        summary,
        importable_sources,
        support_files,
        imported,
    })
}

pub(crate) fn run_agent_install_command(
    paths: &VaultPaths,
    args: &AgentInstallArgs,
) -> Result<AgentInstallReport, CliError> {
    Ok(AgentInstallReport {
        support_files: write_bundled_support_files(paths, args.overwrite, args.example_tool)?,
    })
}

fn print_init_summary(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &InitReport,
) -> Result<(), CliError> {
    let normalized_importable = report
        .importable_sources
        .iter()
        .map(|item| normalize_import_discovery_item(paths, item))
        .collect::<Vec<_>>();
    let normalized_imported = report
        .imported
        .as_ref()
        .map(|batch| ConfigImportBatchReport {
            dry_run: batch.dry_run,
            target: batch.target,
            detected_count: batch.detected_count,
            imported_count: batch.imported_count,
            updated_count: batch.updated_count,
            reports: batch
                .reports
                .iter()
                .map(|item| normalize_config_import_report(paths, item))
                .collect(),
        });
    let normalized = InitReport {
        summary: report.summary.clone(),
        importable_sources: normalized_importable,
        support_files: report.support_files.clone(),
        imported: normalized_imported,
    };

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Initialized {} (config {}, cache {})",
                normalized.summary.vault_root.display(),
                if normalized.summary.created_config {
                    "created"
                } else {
                    "existing"
                },
                if normalized.summary.created_cache {
                    "created"
                } else {
                    "existing"
                },
            );
            if let Some(imported) = &normalized.imported {
                println!(
                    "Imported {} detected importer{} ({} updated).",
                    imported.imported_count,
                    if imported.imported_count == 1 {
                        ""
                    } else {
                        "s"
                    },
                    imported.updated_count
                );
            } else if !normalized.importable_sources.is_empty() {
                println!("Importable settings detected:");
                for importer in &normalized.importable_sources {
                    println!("- {} ({})", importer.plugin, importer.display_name);
                }
                println!("Run `vulcan config import --all` to import them.");
            }
            if !normalized.support_files.is_empty() {
                println!("Bundled agent support files:");
                print_support_file_reports(&normalized.support_files);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&normalized),
    }
}

pub(crate) fn print_agent_install_summary(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &AgentInstallReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Installed bundled agent support files for {}",
                paths.vault_root().display()
            );
            print_support_file_reports(&report.support_files);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_support_file_reports(files: &[SupportFileReport]) {
    for file in files {
        println!("- {} [{}; {}]", file.path, file.kind, file.status.label());
    }
}

fn write_bundled_support_files(
    paths: &VaultPaths,
    overwrite: bool,
    include_example_tool: bool,
) -> Result<Vec<SupportFileReport>, CliError> {
    let mut reports = Vec::new();
    reports.push(write_bundled_text_file(
        paths,
        &BUNDLED_AGENT_TEMPLATE,
        overwrite,
    )?);
    for file in BUNDLED_SKILL_FILES {
        reports.push(write_bundled_text_file(paths, file, overwrite)?);
    }
    for file in BUNDLED_PROMPT_FILES {
        reports.push(write_bundled_text_file(paths, file, overwrite)?);
    }
    if include_example_tool {
        for file in BUNDLED_TOOL_FILES {
            reports.push(write_bundled_text_file(paths, file, overwrite)?);
        }
    }
    Ok(reports)
}

fn write_bundled_text_file(
    paths: &VaultPaths,
    file: &BundledTextFile,
    overwrite: bool,
) -> Result<SupportFileReport, CliError> {
    let destination = bundled_text_file_destination(paths, file);
    let status = write_bundled_text_contents(&destination, file.contents, overwrite)?;
    if bundled_text_file_should_be_executable(file) && destination.exists() {
        set_executable_permissions(&destination)?;
    }
    Ok(SupportFileReport {
        path: bundled_text_file_display_path(paths, file),
        kind: file.kind.to_string(),
        status,
    })
}

fn bundled_text_file_should_be_executable(file: &BundledTextFile) -> bool {
    file.target == BundledFileTarget::SkillsFolder
        && Path::new(file.relative_path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("js"))
}

fn bundled_text_file_destination(paths: &VaultPaths, file: &BundledTextFile) -> PathBuf {
    let config = load_vault_config(paths).config.assistant;
    match file.target {
        BundledFileTarget::VaultRoot => paths.vault_root().join(file.relative_path),
        BundledFileTarget::SkillsFolder => paths
            .vault_root()
            .join(config.skills_folder)
            .join(file.relative_path),
        BundledFileTarget::PromptsFolder => paths
            .vault_root()
            .join(config.prompts_folder)
            .join(file.relative_path),
    }
}

fn bundled_text_file_display_path(paths: &VaultPaths, file: &BundledTextFile) -> String {
    let destination = bundled_text_file_destination(paths, file);
    destination
        .strip_prefix(paths.vault_root())
        .unwrap_or(&destination)
        .to_string_lossy()
        .replace('\\', "/")
}

fn slash_display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn write_bundled_text_contents(
    path: &Path,
    contents: &str,
    overwrite: bool,
) -> Result<SupportFileStatus, CliError> {
    let rendered = if contents.ends_with('\n') {
        contents.as_bytes().to_vec()
    } else {
        format!("{contents}\n").into_bytes()
    };
    let existed_before = match fs::read(path) {
        Ok(existing) => {
            if existing == rendered {
                return Ok(SupportFileStatus::Kept);
            }
            if !overwrite {
                return Ok(SupportFileStatus::Kept);
            }
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(CliError::operation(error)),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    fs::write(path, &rendered).map_err(CliError::operation)?;
    Ok(if existed_before {
        SupportFileStatus::Updated
    } else {
        SupportFileStatus::Created
    })
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(CliError::operation)?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions).map_err(CliError::operation)
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

pub(crate) fn wrap_config_section_toml(section: &str, value: TomlValue) -> TomlValue {
    let mut wrapped = value;
    for part in section.split('.').rev() {
        let mut table = toml::map::Map::new();
        table.insert(part.to_string(), wrapped);
        wrapped = TomlValue::Table(table);
    }
    wrapped
}

fn export_profile_format_from_arg(format: ExportProfileFormatArg) -> ExportProfileFormat {
    match format {
        ExportProfileFormatArg::Markdown => ExportProfileFormat::Markdown,
        ExportProfileFormatArg::Json => ExportProfileFormat::Json,
        ExportProfileFormatArg::Csv => ExportProfileFormat::Csv,
        ExportProfileFormatArg::Graph => ExportProfileFormat::Graph,
        ExportProfileFormatArg::Epub => ExportProfileFormat::Epub,
        ExportProfileFormatArg::Zip => ExportProfileFormat::Zip,
        ExportProfileFormatArg::Sqlite => ExportProfileFormat::Sqlite,
        ExportProfileFormatArg::SearchIndex => ExportProfileFormat::SearchIndex,
        ExportProfileFormatArg::FrontendBundle => ExportProfileFormat::FrontendBundle,
    }
}

fn export_graph_format_config_from_cli(
    format: Option<GraphExportFormat>,
) -> Option<ExportGraphFormatConfig> {
    format.map(|format| match format {
        GraphExportFormat::Json => ExportGraphFormatConfig::Json,
        GraphExportFormat::Dot => ExportGraphFormatConfig::Dot,
        GraphExportFormat::Graphml => ExportGraphFormatConfig::Graphml,
    })
}

fn export_epub_toc_style_config_from_cli(
    style: Option<EpubTocStyle>,
) -> Option<ExportEpubTocStyleConfig> {
    style.map(|style| match style {
        EpubTocStyle::Tree => ExportEpubTocStyleConfig::Tree,
        EpubTocStyle::Flat => ExportEpubTocStyleConfig::Flat,
    })
}

fn print_export_profile_show_report(
    output: OutputFormat,
    report: &ExportProfileShowReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            print!("{}", report.rendered_toml);
            for diagnostic in &report.diagnostics {
                eprintln!(
                    "warning: {}: {}",
                    diagnostic.path.display(),
                    diagnostic.message
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_export_profile_write_report(
    output: OutputFormat,
    report: &ExportProfileWriteReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                if report.action == ExportProfileWriteAction::Unchanged {
                    println!(
                        "No changes for export profile `{}` in {}",
                        report.name,
                        report.config_path.display()
                    );
                } else {
                    match report.action {
                        ExportProfileWriteAction::Created => println!(
                            "Would create export profile `{}` in {}",
                            report.name,
                            report.config_path.display()
                        ),
                        ExportProfileWriteAction::Replaced => println!(
                            "Would replace export profile `{}` in {}",
                            report.name,
                            report.config_path.display()
                        ),
                        ExportProfileWriteAction::Updated => println!(
                            "Would update export profile `{}` in {}",
                            report.name,
                            report.config_path.display()
                        ),
                        ExportProfileWriteAction::Unchanged => {}
                    }
                    print!("{}", report.rendered_toml);
                }
            } else {
                match report.action {
                    ExportProfileWriteAction::Created => {
                        println!(
                            "Created export profile `{}` in {}",
                            report.name,
                            report.config_path.display()
                        );
                        print!("{}", report.rendered_toml);
                    }
                    ExportProfileWriteAction::Replaced => {
                        println!(
                            "Replaced export profile `{}` in {}",
                            report.name,
                            report.config_path.display()
                        );
                        print!("{}", report.rendered_toml);
                    }
                    ExportProfileWriteAction::Updated => {
                        println!(
                            "Updated export profile `{}` in {}",
                            report.name,
                            report.config_path.display()
                        );
                        print!("{}", report.rendered_toml);
                    }
                    ExportProfileWriteAction::Unchanged => {
                        println!(
                            "No changes for export profile `{}` in {}",
                            report.name,
                            report.config_path.display()
                        );
                    }
                }
            }
            for diagnostic in &report.diagnostics {
                eprintln!(
                    "warning: {}: {}",
                    diagnostic.path.display(),
                    diagnostic.message
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_export_profile_delete_report(
    output: OutputFormat,
    report: &ExportProfileDeleteReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                if report.deleted {
                    println!(
                        "Would delete export profile `{}` from {}",
                        report.name,
                        report.config_path.display()
                    );
                } else {
                    println!(
                        "Export profile `{}` was not present in {}",
                        report.name,
                        report.config_path.display()
                    );
                }
            } else if report.deleted {
                println!(
                    "Deleted export profile `{}` from {}",
                    report.name,
                    report.config_path.display()
                );
            } else {
                println!(
                    "Export profile `{}` was not present in {}",
                    report.name,
                    report.config_path.display()
                );
            }
            for diagnostic in &report.diagnostics {
                eprintln!(
                    "warning: {}: {}",
                    diagnostic.path.display(),
                    diagnostic.message
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_export_profile_rule_list(
    output: OutputFormat,
    name: &str,
    entries: &[ExportProfileRuleListEntry],
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(&entries.to_vec()),
        OutputFormat::Human | OutputFormat::Markdown => {
            if entries.is_empty() {
                println!("No content transform rules configured for export profile `{name}`");
                return Ok(());
            }
            for entry in entries {
                let rule = entry.rule.as_object().ok_or_else(|| {
                    CliError::operation("export profile rule did not serialize to an object")
                })?;
                let query = rule
                    .get("query")
                    .and_then(Value::as_str)
                    .or_else(|| rule.get("query_json").and_then(Value::as_str))
                    .unwrap_or("<all exported notes>");
                println!(
                    "{}  query={}  callouts={}  headings={}  frontmatter_keys={}  inline_fields={}  replace={}",
                    entry.index,
                    query,
                    rule.get("exclude_callouts")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len),
                    rule.get("exclude_headings")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len),
                    rule.get("exclude_frontmatter_keys")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len),
                    rule.get("exclude_inline_fields")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len),
                    rule.get("replace")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len),
                );
            }
            Ok(())
        }
    }
}

fn handle_site_command(
    output: OutputFormat,
    paths: &VaultPaths,
    command: &SiteCommand,
    use_stderr_color: bool,
) -> Result<(), CliError> {
    match command {
        SiteCommand::Build {
            profile,
            output_dir,
            clean,
            dry_run,
            watch,
            debounce_ms,
            strict,
            fail_on_warning,
        } => {
            let request = SiteBuildRequest {
                profile: profile.clone(),
                output_dir: output_dir.clone(),
                clean: *clean,
                dry_run: *dry_run,
            };
            let mut progress = (output == OutputFormat::Human)
                .then(|| SiteBuildProgressReporter::new(use_stderr_color));
            let report = build_site_with_policy_and_progress(
                paths,
                &request,
                *strict,
                *fail_on_warning,
                |event| {
                    if let Some(progress) = progress.as_mut() {
                        progress.record(event);
                    }
                },
            )?;
            print_site_build_report(output, &report)?;
            if *watch {
                watch_site_builds_forever(
                    output,
                    paths,
                    &request,
                    *debounce_ms,
                    *strict,
                    *fail_on_warning,
                    use_stderr_color,
                )?;
            }
            Ok(())
        }
        SiteCommand::Serve {
            profile,
            output_dir,
            port,
            watch,
            debounce_ms,
            strict,
            fail_on_warning,
        } => {
            let handle = spawn_site_server(
                paths.clone(),
                SiteServeOptions {
                    profile: profile.clone(),
                    output_dir: output_dir.clone(),
                    port: *port,
                    watch: *watch,
                    debounce_ms: *debounce_ms,
                    strict: *strict,
                    fail_on_warning: *fail_on_warning,
                },
            )?;
            let addr = handle.addr();
            match output {
                OutputFormat::Json => print_json(&json!({
                    "ok": true,
                    "profile": profile.clone().unwrap_or_else(|| "default".to_string()),
                    "url": format!("http://{addr}/"),
                    "watch": watch,
                    "strict": *strict || *fail_on_warning,
                }))?,
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!(
                        "Serving static site at http://{addr}/{}",
                        if *watch { " (watch enabled)" } else { "" }
                    );
                }
            }
            handle.join()
        }
        SiteCommand::Profiles => {
            let report = app_build_site_profiles_report(paths).map_err(CliError::operation)?;
            print_site_profile_list_report(output, &report)
        }
        SiteCommand::Doctor { profile } => {
            let report = app_build_site_doctor_report(paths, profile.as_deref())
                .map_err(CliError::operation)?;
            print_site_doctor_report(output, &report)
        }
    }
}

fn print_export_profile_rule_write_report(
    output: OutputFormat,
    report: &ExportProfileRuleWriteReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let message = match (report.dry_run, report.action) {
                (true, ExportProfileRuleWriteAction::Added) => "Would add",
                (true, ExportProfileRuleWriteAction::Updated) => "Would update",
                (true, ExportProfileRuleWriteAction::Moved) => "Would move",
                (true, ExportProfileRuleWriteAction::Deleted) => "Would delete",
                (true | false, ExportProfileRuleWriteAction::Unchanged) => "No changes for",
                (false, ExportProfileRuleWriteAction::Added) => "Added",
                (false, ExportProfileRuleWriteAction::Updated) => "Updated",
                (false, ExportProfileRuleWriteAction::Moved) => "Moved",
                (false, ExportProfileRuleWriteAction::Deleted) => "Deleted",
            };
            match report.action {
                ExportProfileRuleWriteAction::Deleted | ExportProfileRuleWriteAction::Unchanged => {
                    if let Some(rule_index) = report.rule_index {
                        println!(
                            "{message} content_transforms rule {} in export profile `{}` in {}",
                            rule_index,
                            report.name,
                            report.config_path.display()
                        );
                    } else {
                        println!(
                            "{message} content_transforms for export profile `{}` in {}",
                            report.name,
                            report.config_path.display()
                        );
                    }
                }
                ExportProfileRuleWriteAction::Moved => {
                    println!(
                        "{message} content_transforms rule {} to rule {} in export profile `{}` in {}",
                        report.previous_rule_index.unwrap_or_default(),
                        report.rule_index.unwrap_or_default(),
                        report.name,
                        report.config_path.display()
                    );
                }
                ExportProfileRuleWriteAction::Added | ExportProfileRuleWriteAction::Updated => {
                    println!(
                        "{message} content_transforms rule {} in export profile `{}` in {}",
                        report.rule_index.unwrap_or_default(),
                        report.name,
                        report.config_path.display()
                    );
                }
            }
            if report.action != ExportProfileRuleWriteAction::Unchanged {
                print!("{}", report.rendered_toml);
            }
            for diagnostic in &report.diagnostics {
                eprintln!(
                    "warning: {}: {}",
                    diagnostic.path.display(),
                    diagnostic.message
                );
            }
            Ok(())
        }
    }
}

fn config_changed_files(
    paths: &VaultPaths,
    config_path: &Path,
    had_gitignore: bool,
) -> Vec<String> {
    let mut changed = vec![config_path.to_string_lossy().replace('\\', "/")];
    if !had_gitignore && paths.gitignore_file().exists() {
        changed.push(".vulcan/.gitignore".to_string());
    }
    changed
}

fn config_set_changed_files(paths: &VaultPaths, had_gitignore: bool) -> Vec<String> {
    config_changed_files(paths, Path::new(".vulcan/config.toml"), had_gitignore)
}

fn config_target(target: ConfigTargetArg) -> app_config::ConfigTarget {
    match target {
        ConfigTargetArg::Shared => app_config::ConfigTarget::Shared,
        ConfigTargetArg::Local => app_config::ConfigTarget::Local,
    }
}

fn watch_site_builds_forever(
    output: OutputFormat,
    paths: &VaultPaths,
    request: &SiteBuildRequest,
    debounce_ms: u64,
    strict: bool,
    fail_on_warning: bool,
    use_stderr_color: bool,
) -> Result<(), CliError> {
    watch_vault(paths, &WatchOptions { debounce_ms }, |watch_report| {
        if watch_report.startup {
            return Ok(());
        }
        let mut progress = (output == OutputFormat::Human)
            .then(|| SiteBuildProgressReporter::new(use_stderr_color));
        let report = build_site_with_policy_and_progress(
            paths,
            &SiteBuildRequest {
                profile: request.profile.clone(),
                output_dir: request.output_dir.clone(),
                clean: false,
                dry_run: request.dry_run,
            },
            strict,
            fail_on_warning,
            |event| {
                if let Some(progress) = progress.as_mut() {
                    progress.record(event);
                }
            },
        )?;
        print_site_build_report(output, &report)
    })
    .map_err(CliError::operation)
}

fn print_site_profile_list_report(
    output: OutputFormat,
    entries: &[SiteProfileListEntry],
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(&entries.to_vec()),
        OutputFormat::Markdown => {
            for entry in entries {
                let implicit = if entry.implicit { " (implicit)" } else { "" };
                println!(
                    "- `{}`{}: {} note(s), theme `{}`, output `{}`",
                    entry.name, implicit, entry.note_count, entry.theme, entry.output_dir
                );
            }
            Ok(())
        }
        OutputFormat::Human => {
            if entries.is_empty() {
                println!("No site profiles configured.");
                return Ok(());
            }
            for entry in entries {
                let implicit = if entry.implicit { " (implicit)" } else { "" };
                println!(
                    "{}{}	{} notes	{}",
                    entry.name, implicit, entry.note_count, entry.output_dir
                );
            }
            Ok(())
        }
    }
}

fn print_site_build_report(output: OutputFormat, report: &SiteBuildReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Markdown => {
            println!(
                "# Site Build
"
            );
            println!("- Profile: `{}`", report.profile);
            println!("- Output: `{}`", report.output_dir);
            println!("- Notes: {}", report.note_count);
            println!("- Pages: {}", report.page_count);
            println!("- Assets: {}", report.asset_count);
            if !report.diagnostics.is_empty() {
                println!(
                    "
## Diagnostics"
                );
                for diagnostic in &report.diagnostics {
                    println!(
                        "- {} {} {}",
                        diagnostic.level, diagnostic.kind, diagnostic.message
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Human => {
            println!(
                "Built {} note(s) into {} ({} pages, {} assets)",
                report.note_count, report.output_dir, report.page_count, report.asset_count
            );
            if !report.diagnostics.is_empty() {
                println!("Diagnostics:");
                for diagnostic in &report.diagnostics {
                    match diagnostic.source_path.as_deref() {
                        Some(path) => println!(
                            "- [{}] {} {} ({})",
                            diagnostic.level, diagnostic.kind, diagnostic.message, path
                        ),
                        None => println!(
                            "- [{}] {} {}",
                            diagnostic.level, diagnostic.kind, diagnostic.message
                        ),
                    }
                }
            }
            Ok(())
        }
    }
}

fn print_site_doctor_report(
    output: OutputFormat,
    report: &SiteDoctorReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Markdown => {
            println!(
                "# Site Doctor
"
            );
            println!("- Profile: `{}`", report.profile);
            println!("- Published notes: {}", report.note_count);
            println!("- Diagnostics: {}", report.diagnostics.len());
            for diagnostic in &report.diagnostics {
                println!(
                    "- [{}] {} {}",
                    diagnostic.level, diagnostic.kind, diagnostic.message
                );
            }
            Ok(())
        }
        OutputFormat::Human => {
            if report.diagnostics.is_empty() {
                println!(
                    "No publish diagnostics for profile `{}` ({} note(s)).",
                    report.profile, report.note_count
                );
                return Ok(());
            }
            println!(
                "Publish diagnostics for profile `{}` ({} note(s)):",
                report.profile, report.note_count
            );
            for diagnostic in &report.diagnostics {
                match diagnostic.source_path.as_deref() {
                    Some(path) => println!(
                        "- [{}] {} {} ({})",
                        diagnostic.level, diagnostic.kind, diagnostic.message, path
                    ),
                    None => println!(
                        "- [{}] {} {}",
                        diagnostic.level, diagnostic.kind, diagnostic.message
                    ),
                }
            }
            Ok(())
        }
    }
}

fn print_scan_summary(output: OutputFormat, summary: &ScanSummary, use_color: bool) {
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{} {} files: {} added, {} updated, {} unchanged, {} deleted",
                palette.cyan("Scanned"),
                summary.discovered,
                palette.green(&summary.added.to_string()),
                palette.yellow(&summary.updated.to_string()),
                summary.unchanged,
                palette.red(&summary.deleted.to_string())
            );
        }
        OutputFormat::Json => {
            print_json(summary).expect("scan summary JSON serialization should succeed");
        }
    }
}

fn print_move_summary(output: OutputFormat, summary: &MoveSummary) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if summary.dry_run {
                println!(
                    "Dry run: move {} -> {}",
                    summary.source_path, summary.destination_path
                );
            } else {
                println!(
                    "Moved {} -> {}",
                    summary.source_path, summary.destination_path
                );
            }

            if summary.rewritten_files.is_empty() {
                println!("No link rewrites.");
                return Ok(());
            }

            for file in &summary.rewritten_files {
                println!("- {}", file.path);
                for change in &file.changes {
                    println!("  {} -> {}", change.before, change.after);
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(summary),
    }
}

fn print_refactor_report(output: OutputFormat, report: &RefactorReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Dry run for {}", report.action);
            } else {
                println!("Applied {}", report.action);
            }

            if report.files.is_empty() {
                println!("No files changed.");
                return Ok(());
            }

            for file in &report.files {
                println!("- {}", file.path);
                for change in &file.changes {
                    println!("  {} -> {}", change.before, change.after);
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_bulk_mutation_report(
    output: OutputFormat,
    report: &BulkMutationReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Dry run for {}", report.action);
            } else {
                println!("Applied {}", report.action);
            }

            if report.files.is_empty() {
                println!("No files changed.");
                return Ok(());
            }

            for file in &report.files {
                if file.changes.is_empty() {
                    println!("- {} (no change)", file.path);
                } else {
                    println!("- {}", file.path);
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_doctor_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &DoctorReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Doctor summary for {}", paths.vault_root().display());
            println!("- unresolved links: {}", report.summary.unresolved_links);
            println!(
                "- ambiguous link targets: {}",
                report.summary.ambiguous_links
            );
            println!("- broken embeds: {}", report.summary.broken_embeds);
            println!("- parse failures: {}", report.summary.parse_failures);
            println!("- type mismatches: {}", report.summary.type_mismatches);
            println!(
                "- unsupported syntax: {}",
                report.summary.unsupported_syntax
            );
            println!("- stale index rows: {}", report.summary.stale_index_rows);
            println!(
                "- missing index rows: {}",
                report.summary.missing_index_rows
            );
            println!("- orphan notes: {}", report.summary.orphan_notes);
            println!("- orphan assets: {}", report.summary.orphan_assets);
            println!("- HTML links: {}", report.summary.html_links);

            if report.summary == zero_summary() {
                println!("No issues found.");
                return Ok(());
            }

            print_link_section("Unresolved links", &report.unresolved_links);
            print_link_section("Ambiguous link targets", &report.ambiguous_links);
            print_link_section("Broken embeds", &report.broken_embeds);
            print_diagnostic_section("Parse failures", &report.parse_failures);
            print_diagnostic_section("Type mismatches", &report.type_mismatches);
            print_diagnostic_section("Unsupported syntax", &report.unsupported_syntax);
            print_path_section("Stale index rows", &report.stale_index_rows);
            print_path_section("Missing index rows", &report.missing_index_rows);
            print_path_section("Orphan notes", &report.orphan_notes);
            print_path_section("Orphan assets", &report.orphan_assets);
            print_diagnostic_section("HTML links", &report.html_links);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_doctor_fix_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &DoctorFixReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Doctor fix plan for {}", paths.vault_root().display());
            } else {
                println!("Doctor fix run for {}", paths.vault_root().display());
            }
            if report.fixes.is_empty() {
                println!("No deterministic fixes needed.");
            } else {
                for fix in &report.fixes {
                    println!("- {}: {}", fix.kind, fix.description);
                }
            }

            if !report.suggestions.is_empty() {
                println!("Suggestions:");
                for suggestion in &report.suggestions {
                    println!("- {suggestion}");
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_graph_path_report(output: OutputFormat, report: &GraphPathReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.path.is_empty() {
                println!(
                    "No resolved path from {} to {}.",
                    report.from_path, report.to_path
                );
            } else {
                println!("{}", report.path.join(" -> "));
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_graph_hubs_report(
    output: OutputFormat,
    report: &GraphHubsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_hub_rows(visible_notes);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph hubs"));
            }
            if visible_notes.is_empty() {
                println!("No graph hubs.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    println!(
                        "- {} [{} inbound, {} outbound]",
                        note.document_path, note.inbound, note.outbound
                    );
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_graph_moc_report(
    output: OutputFormat,
    report: &GraphMocReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_moc_rows(visible_notes);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("MOC candidates"));
            }
            if visible_notes.is_empty() {
                println!("No MOC candidates.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    println!(
                        "- {} [score {}, {} inbound, {} outbound]",
                        note.document_path, note.score, note.inbound, note.outbound
                    );
                    if !note.reasons.is_empty() {
                        println!("  {}", note.reasons.join("; "));
                    }
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_graph_dead_ends_report(
    output: OutputFormat,
    report: &GraphDeadEndsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_dead_end_rows(visible_notes);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph dead ends"));
            }
            if visible_notes.is_empty() {
                println!("No dead ends.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    println!("- {note}");
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_graph_components_report(
    output: OutputFormat,
    report: &GraphComponentsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_components = paginated_items(&report.components, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_component_rows(visible_components);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph components"));
            }
            if visible_components.is_empty() {
                println!("No components.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for component in visible_components {
                    println!("- size {}: {}", component.size, component.notes.join(", "));
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn print_graph_communities_report(
    output: OutputFormat,
    report: &GraphCommunitiesReport,
    community: Option<usize>,
    orphans: bool,
    bridges: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let palette = AnsiPalette::new(use_color);
    let rows = graph_community_rows(report, community, orphans, bridges);
    let visible_rows = paginated_items(&rows, list_controls);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph communities"));
            }
            if visible_rows.is_empty() {
                println!("No graph communities.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else if orphans {
                for row in visible_rows {
                    println!(
                        "- {} -> community {} [tag overlap {:.3}]",
                        row["document_path"].as_str().unwrap_or(""),
                        row["closest_community"]
                            .as_u64()
                            .map_or_else(|| "none".to_string(), |id| id.to_string()),
                        row["tag_overlap"].as_f64().unwrap_or(0.0)
                    );
                }
            } else if bridges {
                for row in visible_rows {
                    println!(
                        "- {} [community {}, {} cross-community edges]",
                        row["document_path"].as_str().unwrap_or(""),
                        row["community_id"].as_u64().unwrap_or(0),
                        row["cross_community_edges"].as_u64().unwrap_or(0)
                    );
                }
            } else {
                for row in visible_rows {
                    println!(
                        "- community {}: {} [{} notes, cohesion {:.3}]",
                        row["id"].as_u64().unwrap_or(0),
                        row["label"].as_str().unwrap_or(""),
                        row["size"].as_u64().unwrap_or(0),
                        row["cohesion"].as_f64().unwrap_or(0.0)
                    );
                }
            }
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
        }
    }
}

fn print_graph_analytics_report(
    output: OutputFormat,
    report: &GraphAnalyticsReport,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = graph_analytics_rows(report);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Notes: {}", report.note_count);
            println!("Attachments: {}", report.attachment_count);
            println!("Bases: {}", report.base_count);
            println!("Resolved note links: {}", report.resolved_note_links);
            println!(
                "Confidence: {} EXTRACTED, {} INFERRED, {} AMBIGUOUS",
                report.confidence.extracted,
                report.confidence.inferred,
                report.confidence.ambiguous
            );
            println!(
                "Average outbound links: {:.3}",
                report.average_outbound_links
            );
            println!("Orphan notes: {}", report.orphan_notes);
            print_named_count_section("Top tags", &report.top_tags);
            print_named_count_section("Top properties", &report.top_properties);
            export_rows(&rows, None, export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, None, export)?;
            print_json(report)
        }
    }
}

fn print_graph_trends_report(
    output: OutputFormat,
    report: &GraphTrendsReport,
    list_controls: &ListOutputControls,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = graph_trend_rows(report);
    let visible_rows = paginated_items(&rows, list_controls);
    let visible_points = paginated_items(&report.points, list_controls);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.points.is_empty() {
                println!("No graph trend checkpoints.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for point in visible_points {
                    println!(
                        "- {}: {} notes, {} orphan, {} stale, {} resolved links",
                        point.label,
                        point.note_count,
                        point.orphan_notes,
                        point.stale_notes,
                        point.resolved_links
                    );
                }
            }
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            if list_controls.fields.is_some() || export.is_some() {
                print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
            } else {
                print_json(report)
            }
        }
    }
}

fn print_checkpoint_record(
    output: OutputFormat,
    record: &CheckpointRecord,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Checkpoint {} [{}]: {} notes, {} orphan, {} stale, {} links",
                record.name.as_deref().unwrap_or(&record.id),
                record.source,
                record.note_count,
                record.orphan_notes,
                record.stale_notes,
                record.resolved_links
            );
            Ok(())
        }
        OutputFormat::Json => print_json(record),
    }
}

fn print_checkpoint_list(
    output: OutputFormat,
    records: &[CheckpointRecord],
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible = paginated_items(records, list_controls);
    let rows = checkpoint_rows(visible);
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Checkpoints"));
            }
            if visible.is_empty() {
                println!("No checkpoints.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for record in visible {
                    println!(
                        "- {} [{}] notes={}, orphan={}, stale={}, links={}",
                        record.name.as_deref().unwrap_or(&record.id),
                        record.source,
                        record.note_count,
                        record.orphan_notes,
                        record.stale_notes,
                        record.resolved_links
                    );
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_change_report(
    output: OutputFormat,
    report: &ChangeReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = change_rows(report);
    let visible = paginated_items(&rows, list_controls);
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!(
                    "{} {}",
                    palette.cyan("Changes since"),
                    palette.bold(&report.anchor)
                );
            }
            if visible.is_empty() {
                println!("No recorded changes.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for row in visible {
                    println!(
                        "- {} {} ({})",
                        row["status"].as_str().unwrap_or("updated"),
                        row["path"].as_str().unwrap_or_default(),
                        row["kind"].as_str().unwrap_or_default()
                    );
                }
            }
            export_rows(visible, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible, list_controls.fields.as_deref(), export)?;
            print_json_lines(visible.to_vec(), list_controls.fields.as_deref())
        }
    }
}

fn print_dataview_js_result(
    output: OutputFormat,
    result: &DataviewJsResult,
    show_result_count: bool,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => print_markdown_output(
            output,
            &render_dataview_js_markdown(result, show_result_count),
            stdout_is_tty,
            use_color,
        ),
        OutputFormat::Json => print_json(result),
    }
}

fn print_dql_query_result(
    output: OutputFormat,
    result: &DqlQueryResult,
    show_result_count: bool,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => print_markdown_output(
            output,
            &render_dql_query_markdown(result, show_result_count),
            stdout_is_tty,
            use_color,
        ),
        OutputFormat::Json => print_json(result),
    }
}

fn render_dql_query_markdown(result: &DqlQueryResult, show_result_count: bool) -> String {
    let mut sections = Vec::new();
    let body = match result.query_type {
        vulcan_core::dql::DqlQueryType::Table => {
            render_dql_table_markdown(result, show_result_count)
        }
        vulcan_core::dql::DqlQueryType::List => render_dql_list_markdown(result),
        vulcan_core::dql::DqlQueryType::Task => render_dql_task_markdown(result, show_result_count),
        vulcan_core::dql::DqlQueryType::Calendar => render_dql_calendar_markdown(result),
    };
    if !body.is_empty() {
        sections.push(body);
    }
    let diagnostics = render_dql_diagnostics_markdown(&result.diagnostics);
    if !diagnostics.is_empty() {
        sections.push(diagnostics);
    }
    sections.join("\n\n")
}

fn render_dataview_block_markdown(result: &DataviewBlockResult, show_result_count: bool) -> String {
    match result {
        DataviewBlockResult::Dql(result) => render_dql_query_markdown(result, show_result_count),
        DataviewBlockResult::Js(result) => render_dataview_js_markdown(result, show_result_count),
    }
}

pub(crate) fn render_dataview_eval_markdown(
    report: &DataviewEvalReport,
    show_result_count: bool,
) -> String {
    if report.blocks.is_empty() {
        return format!("No Dataview blocks in {}", report.file);
    }

    let mut sections = vec![format!("# Dataview blocks for {}", report.file)];
    for block in &report.blocks {
        let mut section = vec![format!(
            "## Block {} (`{}`, line {})",
            block.block_index, block.language, block.line_number
        )];
        if let Some(error) = &block.error {
            section.push(format!("error: {error}"));
        } else if let Some(result) = &block.result {
            let rendered = render_dataview_block_markdown(result, show_result_count);
            if !rendered.is_empty() {
                section.push(rendered);
            }
        }
        sections.push(section.join("\n\n"));
    }
    sections.join("\n\n")
}

fn render_dataview_js_markdown(result: &DataviewJsResult, show_result_count: bool) -> String {
    if result.outputs.is_empty() {
        return result
            .value
            .as_ref()
            .map(render_dataview_inline_value)
            .unwrap_or_default();
    }

    result
        .outputs
        .iter()
        .map(|output| match output {
            DataviewJsOutput::Query { result } => {
                render_dql_query_markdown(result, show_result_count)
            }
            DataviewJsOutput::Table { headers, rows } => {
                render_dataview_table_markdown(headers, rows)
            }
            DataviewJsOutput::List { items } => items
                .iter()
                .map(|item| format!("- {}", render_dataview_inline_value(item)))
                .collect::<Vec<_>>()
                .join("\n"),
            DataviewJsOutput::TaskList {
                tasks,
                group_by_file,
            } => render_dataview_task_list_markdown(tasks, *group_by_file),
            DataviewJsOutput::Paragraph { text } | DataviewJsOutput::Span { text } => text.clone(),
            DataviewJsOutput::Header { level, text } => {
                format!("{} {text}", "#".repeat((*level).max(1)))
            }
            DataviewJsOutput::Element {
                element,
                text,
                attrs: _,
            } => format!("<{element}> {text}"),
        })
        .filter(|section| !section.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_dataview_table_markdown(headers: &[String], rows: &[Vec<Value>]) -> String {
    let column_count = markdown_table_column_count(headers.len(), rows.iter().map(Vec::len));
    let mut lines = Vec::new();
    if column_count > 0 {
        let [header, separator] = markdown_table_header_lines(headers, column_count);
        lines.push(header);
        lines.push(separator);
    }
    lines.extend(
        rows.iter().map(|row| {
            markdown_table_row(row.iter().map(render_dataview_inline_value), column_count)
        }),
    );
    lines.join("\n")
}

fn render_dataview_task_list_markdown(tasks: &[Value], group_by_file: bool) -> String {
    let mut lines = Vec::new();
    let mut current_file: Option<&str> = None;
    for task in tasks {
        let file = task
            .get("path")
            .and_then(Value::as_str)
            .or_else(|| {
                task.get("file")
                    .and_then(|file| file.get("path"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("<unknown>");
        if group_by_file && current_file != Some(file) {
            current_file = Some(file);
            lines.push(format!("### {file}"));
        }
        let status = task.get("status").and_then(Value::as_str).unwrap_or(" ");
        let text = task
            .get("text")
            .map(render_dataview_inline_value)
            .unwrap_or_default();
        lines.push(format!("- [{status}] {text}"));
    }
    lines.join("\n")
}

fn render_dql_table_markdown(result: &DqlQueryResult, show_result_count: bool) -> String {
    let column_count = result.columns.len();
    let mut lines = Vec::new();
    if column_count > 0 {
        let [header, separator] = markdown_table_header_lines(&result.columns, column_count);
        lines.push(header);
        lines.push(separator);
    }
    lines.extend(result.rows.iter().map(|row| {
        markdown_table_row(
            result
                .columns
                .iter()
                .map(|column| render_dataview_inline_value(&row[column])),
            column_count,
        )
    }));
    if show_result_count {
        lines.push(format!("{} result(s)", result.result_count));
    }
    lines.join("\n")
}

fn render_dql_list_markdown(result: &DqlQueryResult) -> String {
    if result.rows.is_empty() {
        return String::new();
    }
    result
        .rows
        .iter()
        .map(|row| match result.columns.as_slice() {
            [column] => format!("- {}", render_dataview_inline_value(&row[column])),
            [left, right, ..] => format!(
                "- {}: {}",
                render_dataview_inline_value(&row[left]),
                render_dataview_inline_value(&row[right])
            ),
            [] => format!("- {}", serde_json::to_string(row).unwrap_or_default()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_dql_task_markdown(result: &DqlQueryResult, show_result_count: bool) -> String {
    if result.rows.is_empty() {
        return String::new();
    }

    let file_column = result.columns.first().map_or("File", String::as_str);
    let mut current_file: Option<&str> = None;
    let mut lines = Vec::new();
    for row in &result.rows {
        let file = row[file_column].as_str().unwrap_or_default();
        if current_file != Some(file) {
            current_file = Some(file);
            lines.push(format!("### {file}"));
        }
        let status = row["status"].as_str().unwrap_or(" ");
        let text = render_dataview_inline_value(&row["text"]);
        lines.push(format!("- [{status}] {text}"));
    }
    if show_result_count {
        lines.push(format!("{} task(s)", result.result_count));
    }
    lines.join("\n")
}

fn render_dql_calendar_markdown(result: &DqlQueryResult) -> String {
    if result.rows.is_empty() {
        return "No calendar entries.".to_string();
    }

    let file_column = result.columns.get(1).map_or("File", String::as_str);
    let mut current_date: Option<&str> = None;
    let mut lines = Vec::new();
    for row in &result.rows {
        let date = row["date"].as_str().unwrap_or_default();
        if current_date != Some(date) {
            current_date = Some(date);
            lines.push(format!("### {date}"));
        }
        lines.push(format!(
            "- {}",
            render_dataview_inline_value(&row[file_column])
        ));
    }
    lines.join("\n")
}

fn render_dql_diagnostics_markdown(diagnostics: &[vulcan_core::DqlDiagnostic]) -> String {
    if diagnostics.is_empty() {
        return String::new();
    }
    std::iter::once("Diagnostics:".to_string())
        .chain(
            diagnostics
                .iter()
                .map(|diagnostic| format!("- {}", diagnostic.message)),
        )
        .collect::<Vec<_>>()
        .join("\n")
}

fn print_open_report(output: OutputFormat, report: &OpenReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Opened {} in Obsidian", report.path);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_static_search_index_report(
    output: OutputFormat,
    report: &vulcan_core::StaticSearchIndexReport,
    path: Option<&PathBuf>,
    pretty: bool,
) -> Result<(), CliError> {
    let rendered = if pretty {
        serde_json::to_string_pretty(report).map_err(CliError::operation)?
    } else {
        serde_json::to_string(report).map_err(CliError::operation)?
    };

    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        fs::write(path, format!("{rendered}\n")).map_err(CliError::operation)?;
        match output {
            OutputFormat::Human | OutputFormat::Markdown => {
                println!(
                    "Exported static search index: {} documents, {} chunks -> {}",
                    report.documents,
                    report.chunks,
                    path.display()
                );
                Ok(())
            }
            OutputFormat::Json => print_json(&serde_json::json!({
                "path": path.display().to_string(),
                "documents": report.documents,
                "chunks": report.chunks,
            })),
        }
    } else {
        println!("{rendered}");
        Ok(())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// export targets
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct GraphExportSummary {
    path: String,
    format: String,
    nodes: usize,
    edges: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ExportProfileRunSummary {
    name: String,
    format: String,
    summary: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyMode {
    DryRun,
    Apply,
}

impl ApplyMode {
    fn is_dry_run(self) -> bool {
        matches!(self, Self::DryRun)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitMode {
    Auto,
    Skip,
}

impl CommitMode {
    fn is_disabled(self) -> bool {
        matches!(self, Self::Skip)
    }
}

#[derive(Debug, Clone, Copy)]
struct ConfigMutationOptions {
    apply_mode: ApplyMode,
    commit_mode: CommitMode,
    quiet: bool,
}

#[derive(Debug, Clone, Copy)]
struct ExportContentRequest<'a> {
    query: Option<&'a str>,
    query_json: Option<&'a str>,
    read_filter: Option<&'a PermissionFilter>,
    transforms: Option<&'a [ContentTransformRuleConfig]>,
}

fn write_text_export(
    output: OutputFormat,
    path: Option<&PathBuf>,
    payload: &str,
    summary: &impl Serialize,
) -> Result<(), CliError> {
    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        fs::write(path, payload).map_err(CliError::operation)?;
        match output {
            OutputFormat::Human | OutputFormat::Markdown => {
                println!("{}", path.display());
                Ok(())
            }
            OutputFormat::Json => print_json(summary),
        }
    } else {
        print!("{payload}");
        if !payload.ends_with('\n') {
            println!();
        }
        Ok(())
    }
}

fn write_text_file(path: &Path, payload: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    fs::write(path, payload).map_err(CliError::operation)
}

fn graph_export_format_from_config(format: Option<ExportGraphFormatConfig>) -> GraphExportFormat {
    match format.unwrap_or(ExportGraphFormatConfig::Json) {
        ExportGraphFormatConfig::Json => GraphExportFormat::Json,
        ExportGraphFormatConfig::Dot => GraphExportFormat::Dot,
        ExportGraphFormatConfig::Graphml => GraphExportFormat::Graphml,
    }
}

fn list_export_profiles(paths: &VaultPaths) -> Vec<ExportProfileListEntry> {
    build_export_profile_list(paths)
}

fn print_export_profile_list(
    output: OutputFormat,
    entries: &[ExportProfileListEntry],
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(&entries.to_vec()),
        OutputFormat::Human | OutputFormat::Markdown => {
            if entries.is_empty() {
                println!("No export profiles configured");
                return Ok(());
            }
            for entry in entries {
                let format = entry.format.as_deref().unwrap_or("?");
                let path = entry.path.as_deref().unwrap_or("<missing path>");
                println!("{}  {}  {}", entry.name, format, path);
            }
            Ok(())
        }
    }
}

fn run_export_profile_show(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
) -> Result<(), CliError> {
    let report = build_export_profile_show_report(paths, name).map_err(CliError::operation)?;
    print_export_profile_show_report(output, &report)
}

fn commit_export_profile_changes(
    paths: &VaultPaths,
    commit_scope: &str,
    changed_paths: &[String],
    mutation: ConfigMutationOptions,
) -> Result<(), CliError> {
    if mutation.apply_mode.is_dry_run() || changed_paths.is_empty() {
        return Ok(());
    }
    let auto_commit = AutoCommitPolicy::for_mutation(paths, mutation.commit_mode.is_disabled());
    warn_auto_commit_if_needed(&auto_commit, mutation.quiet);
    auto_commit
        .commit(paths, commit_scope, changed_paths, None, mutation.quiet)
        .map_err(CliError::operation)?;
    Ok(())
}

fn run_export_profile_create(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
    request: &ExportProfileCreateRequest,
    replace_existing: bool,
    mutation: ConfigMutationOptions,
) -> Result<(), CliError> {
    let report = apply_export_profile_create(
        paths,
        name,
        request,
        replace_existing,
        mutation.apply_mode.is_dry_run(),
    )
    .map_err(CliError::operation)?;
    commit_export_profile_changes(
        paths,
        "export-profile-create",
        &report.changed_paths,
        mutation,
    )?;
    print_export_profile_write_report(output, &report)
}

fn run_export_profile_set(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
    request: &ExportProfileSetRequest,
    mutation: ConfigMutationOptions,
) -> Result<(), CliError> {
    let report = apply_export_profile_set(paths, name, request, mutation.apply_mode.is_dry_run())
        .map_err(CliError::operation)?;
    commit_export_profile_changes(paths, "export-profile-set", &report.changed_paths, mutation)?;
    print_export_profile_write_report(output, &report)
}

fn run_export_profile_delete(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
    mutation: ConfigMutationOptions,
) -> Result<(), CliError> {
    let report = apply_export_profile_delete(paths, name, mutation.apply_mode.is_dry_run())
        .map_err(CliError::operation)?;
    commit_export_profile_changes(
        paths,
        "export-profile-delete",
        &report.changed_paths,
        mutation,
    )?;
    print_export_profile_delete_report(output, &report)
}

fn run_export_profile_rule_list(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
) -> Result<(), CliError> {
    let rules = build_export_profile_rule_list(paths, name).map_err(CliError::operation)?;
    print_export_profile_rule_list(output, name, &rules)
}

fn run_export_profile_rule_add(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
    before: Option<usize>,
    request: &ExportProfileRuleRequest,
    mutation: ConfigMutationOptions,
) -> Result<(), CliError> {
    let report = apply_export_profile_rule_add(
        paths,
        name,
        before,
        request,
        mutation.apply_mode.is_dry_run(),
    )
    .map_err(CliError::operation)?;
    commit_export_profile_changes(
        paths,
        "export-profile-rule-add",
        &report.changed_paths,
        mutation,
    )?;
    print_export_profile_rule_write_report(output, &report)
}

fn run_export_profile_rule_update(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
    index: usize,
    request: &ExportProfileRuleRequest,
    mutation: ConfigMutationOptions,
) -> Result<(), CliError> {
    let report = apply_export_profile_rule_update(
        paths,
        name,
        index,
        request,
        mutation.apply_mode.is_dry_run(),
    )
    .map_err(CliError::operation)?;
    commit_export_profile_changes(
        paths,
        "export-profile-rule-update",
        &report.changed_paths,
        mutation,
    )?;
    print_export_profile_rule_write_report(output, &report)
}

fn run_export_profile_rule_delete(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
    index: usize,
    mutation: ConfigMutationOptions,
) -> Result<(), CliError> {
    let report =
        apply_export_profile_rule_delete(paths, name, index, mutation.apply_mode.is_dry_run())
            .map_err(CliError::operation)?;
    commit_export_profile_changes(
        paths,
        "export-profile-rule-delete",
        &report.changed_paths,
        mutation,
    )?;
    print_export_profile_rule_write_report(output, &report)
}

fn run_export_profile_rule_move(
    paths: &VaultPaths,
    output: OutputFormat,
    name: &str,
    request: ExportProfileRuleMoveRequest,
    mutation: ConfigMutationOptions,
) -> Result<(), CliError> {
    let report =
        apply_export_profile_rule_move(paths, name, request, mutation.apply_mode.is_dry_run())
            .map_err(CliError::operation)?;
    commit_export_profile_changes(
        paths,
        "export-profile-rule-move",
        &report.changed_paths,
        mutation,
    )?;
    print_export_profile_rule_write_report(output, &report)
}

fn require_export_profile_config(
    paths: &VaultPaths,
    name: &str,
) -> Result<ExportProfileConfig, CliError> {
    load_vault_config(paths)
        .config
        .export
        .profiles
        .get(name)
        .cloned()
        .ok_or_else(|| CliError::operation(format!("unknown export profile `{name}`")))
}

fn finish_export_profile_text<T: Serialize>(
    output: OutputFormat,
    output_path: &Path,
    payload: &str,
    summary: &T,
) -> Result<Value, CliError> {
    write_text_file(output_path, payload)?;
    if output != OutputFormat::Json {
        println!("{}", output_path.display());
    }
    serde_json::to_value(summary).map_err(CliError::operation)
}

fn finish_export_profile_binary<T: Serialize>(
    output: OutputFormat,
    path: &str,
    summary: &T,
) -> Result<Value, CliError> {
    if output != OutputFormat::Json {
        println!("{path}");
    }
    serde_json::to_value(summary).map_err(CliError::operation)
}

fn run_markdown_export_profile(
    output: OutputFormat,
    paths: &VaultPaths,
    output_path: &Path,
    title: Option<&str>,
    request: ExportContentRequest<'_>,
) -> Result<Value, CliError> {
    let report = execute_export_query(
        paths,
        request.query,
        request.query_json,
        request.read_filter,
    )
    .map_err(CliError::operation)?;
    let prepared = prepare_export_data(paths, &report, request.read_filter, request.transforms)
        .map_err(CliError::operation)?;
    let payload = render_markdown_export_payload(&prepared.notes, title);
    let summary = MarkdownExportSummary {
        path: output_path.display().to_string(),
        result_count: prepared.notes.len(),
    };
    finish_export_profile_text(output, output_path, &payload, &summary)
}

fn run_json_export_profile(
    output: OutputFormat,
    paths: &VaultPaths,
    output_path: &Path,
    pretty: bool,
    request: ExportContentRequest<'_>,
) -> Result<Value, CliError> {
    let report = execute_export_query(
        paths,
        request.query,
        request.query_json,
        request.read_filter,
    )
    .map_err(CliError::operation)?;
    let prepared = prepare_export_data(paths, &report, request.read_filter, request.transforms)
        .map_err(CliError::operation)?;
    let payload = render_json_export_payload(&report, &prepared.notes, pretty)
        .map_err(CliError::operation)?;
    let summary = JsonExportSummary {
        path: output_path.display().to_string(),
        result_count: prepared.notes.len(),
    };
    finish_export_profile_text(output, output_path, &payload, &summary)
}

fn run_csv_export_profile(
    output: OutputFormat,
    paths: &VaultPaths,
    output_path: &Path,
    query: Option<&str>,
    query_json: Option<&str>,
    read_filter: Option<&PermissionFilter>,
) -> Result<Value, CliError> {
    let report =
        execute_export_query(paths, query, query_json, read_filter).map_err(CliError::operation)?;
    let payload = render_csv_export_payload(&report).map_err(CliError::operation)?;
    let summary = CsvExportSummary {
        path: output_path.display().to_string(),
        result_count: report.notes.len(),
    };
    finish_export_profile_text(output, output_path, &payload, &summary)
}

fn run_graph_export_profile(
    output: OutputFormat,
    paths: &VaultPaths,
    output_path: &Path,
    read_filter: Option<&PermissionFilter>,
    graph_format: Option<ExportGraphFormatConfig>,
) -> Result<Value, CliError> {
    let report =
        vulcan_core::export_graph_with_filter(paths, read_filter).map_err(CliError::operation)?;
    let graph_format = graph_export_format_from_config(graph_format);
    let payload = render_graph_export_payload(&report, graph_format)?;
    let summary = GraphExportSummary {
        path: output_path.display().to_string(),
        format: match graph_format {
            GraphExportFormat::Json => "json",
            GraphExportFormat::Dot => "dot",
            GraphExportFormat::Graphml => "graphml",
        }
        .to_string(),
        nodes: report.nodes.len(),
        edges: report.edges.len(),
    };
    finish_export_profile_text(output, output_path, &payload, &summary)
}

fn run_epub_export_profile(
    output: OutputFormat,
    paths: &VaultPaths,
    output_path: &Path,
    query: Option<&str>,
    query_json: Option<&str>,
    read_filter: Option<&PermissionFilter>,
    profile: &ExportProfileConfig,
) -> Result<Value, CliError> {
    let report =
        execute_export_query(paths, query, query_json, read_filter).map_err(CliError::operation)?;
    let prepared = prepare_export_data(
        paths,
        &report,
        read_filter,
        profile.content_transform_rules.as_deref(),
    )
    .map_err(CliError::operation)?;
    let summary = app_write_epub_export(
        paths,
        output_path,
        &prepared.notes,
        &prepared.links,
        AppEpubExportOptions {
            title: profile.title.as_deref(),
            author: profile.author.as_deref(),
            backlinks: profile.backlinks.unwrap_or(false),
            frontmatter: profile.frontmatter.unwrap_or(false),
            toc_style: profile.toc.unwrap_or(ExportEpubTocStyleConfig::Tree),
        },
        AppEpubRenderCallbacks {
            render_dataview_block: &render_epub_dataview_block_markdown,
            render_base_embed: &render_epub_base_embed_markdown,
            render_inline_value: &render_dataview_inline_value,
        },
    )
    .map_err(CliError::operation)?;
    finish_export_profile_binary(output, &summary.path, &summary)
}

fn run_zip_export_profile(
    output: OutputFormat,
    paths: &VaultPaths,
    output_path: &Path,
    request: ExportContentRequest<'_>,
) -> Result<Value, CliError> {
    let report = execute_export_query(
        paths,
        request.query,
        request.query_json,
        request.read_filter,
    )
    .map_err(CliError::operation)?;
    let prepared = prepare_export_data(paths, &report, request.read_filter, request.transforms)
        .map_err(CliError::operation)?;
    let summary = write_zip_export(
        paths,
        output_path,
        &report,
        &prepared.notes,
        &prepared.links,
    )
    .map_err(CliError::operation)?;
    finish_export_profile_binary(output, &summary.path, &summary)
}

fn run_sqlite_export_profile(
    output: OutputFormat,
    paths: &VaultPaths,
    output_path: &Path,
    query: Option<&str>,
    query_json: Option<&str>,
    read_filter: Option<&PermissionFilter>,
) -> Result<Value, CliError> {
    let report =
        execute_export_query(paths, query, query_json, read_filter).map_err(CliError::operation)?;
    let notes = load_exported_notes(paths, &report).map_err(CliError::operation)?;
    let links = load_export_links(paths, &notes).map_err(CliError::operation)?;
    let summary =
        write_sqlite_export(output_path, &report, &notes, &links).map_err(CliError::operation)?;
    finish_export_profile_binary(output, &summary.path, &summary)
}

fn run_search_index_export_profile(
    output: OutputFormat,
    paths: &VaultPaths,
    output_path: &Path,
    pretty: bool,
) -> Result<Value, CliError> {
    let report = export_static_search_index(paths).map_err(CliError::operation)?;
    let rendered = if pretty {
        serde_json::to_string_pretty(&report).map_err(CliError::operation)?
    } else {
        serde_json::to_string(&report).map_err(CliError::operation)?
    };
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    fs::write(output_path, format!("{rendered}\n")).map_err(CliError::operation)?;
    if output != OutputFormat::Json {
        println!(
            "Exported static search index: {} documents, {} chunks -> {}",
            report.documents,
            report.chunks,
            output_path.display()
        );
    }
    Ok(serde_json::json!({
        "path": output_path.display().to_string(),
        "documents": report.documents,
        "chunks": report.chunks,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FrontendBundleExportSummary {
    path: String,
    site_profile: String,
    contract_path: String,
    note_count: usize,
    asset_count: usize,
    search_enabled: bool,
    graph_enabled: bool,
    diagnostic_count: usize,
}

fn require_frontend_bundle_site_profile<'a>(
    name: &str,
    profile: &'a ExportProfileConfig,
) -> Result<&'a str, CliError> {
    profile.site_profile.as_deref().ok_or_else(|| {
        CliError::operation(format!(
            "export profile `{name}` requires `site_profile` for frontend-bundle exports"
        ))
    })
}

fn run_frontend_bundle_export_profile(
    output: OutputFormat,
    paths: &VaultPaths,
    profile_name: &str,
    output_path: &Path,
    profile: &ExportProfileConfig,
) -> Result<Value, CliError> {
    let site_profile = require_frontend_bundle_site_profile(profile_name, profile)?;
    let report = app_build_frontend_bundle(
        paths,
        &FrontendBundleRequest {
            profile: Some(site_profile.to_string()),
            output_dir: output_path.to_path_buf(),
            clean: false,
            dry_run: false,
            pretty: profile.pretty.unwrap_or(true),
        },
    )
    .map_err(CliError::operation)?;
    let contract_path = PathBuf::from(&report.output_dir).join("frontend-bundle.json");
    let summary = FrontendBundleExportSummary {
        path: report.output_dir.clone(),
        site_profile: site_profile.to_string(),
        contract_path: slash_display_path(&contract_path),
        note_count: report.note_count,
        asset_count: report.asset_count,
        search_enabled: report.contract.profile.search,
        graph_enabled: report.contract.profile.graph,
        diagnostic_count: report.diagnostics.len(),
    };
    finish_export_profile_binary(output, &summary.path, &summary)
}

fn run_export_profile_serve(
    paths: &VaultPaths,
    name: &str,
    port: u16,
    debounce_ms: u64,
) -> Result<(), CliError> {
    let profile = require_export_profile_config(paths, name)?;
    validate_export_profile_config(name, &profile).map_err(CliError::operation)?;
    let format = require_export_profile_format(name, &profile).map_err(CliError::operation)?;
    if format != ExportProfileFormat::FrontendBundle {
        return Err(CliError::operation(format!(
            "export profile `{name}` must use format `frontend-bundle` for `export profile serve`"
        )));
    }
    let output_path =
        require_export_profile_path(paths, name, &profile).map_err(CliError::operation)?;
    let site_profile = require_frontend_bundle_site_profile(name, &profile)?;
    serve_frontend_bundle_profile(
        paths,
        &FrontendBundleServeOptions {
            export_profile_name: name.to_string(),
            site_profile_name: site_profile.to_string(),
            output_dir: output_path,
            port,
            debounce_ms,
            pretty: profile.pretty.unwrap_or(true),
        },
    )
}

fn run_export_profile(
    cli: &Cli,
    paths: &VaultPaths,
    name: &str,
    read_filter: Option<&PermissionFilter>,
) -> Result<(), CliError> {
    let profile = require_export_profile_config(paths, name)?;
    validate_export_profile_config(name, &profile).map_err(CliError::operation)?;
    let format = require_export_profile_format(name, &profile).map_err(CliError::operation)?;
    let output_path =
        require_export_profile_path(paths, name, &profile).map_err(CliError::operation)?;
    let (query, query_json) =
        export_profile_query_args(name, format, &profile).map_err(CliError::operation)?;

    let summary = match format {
        ExportProfileFormat::Markdown => run_markdown_export_profile(
            cli.output,
            paths,
            &output_path,
            profile.title.as_deref(),
            ExportContentRequest {
                query,
                query_json,
                read_filter,
                transforms: profile.content_transform_rules.as_deref(),
            },
        )?,
        ExportProfileFormat::Json => run_json_export_profile(
            cli.output,
            paths,
            &output_path,
            profile.pretty.unwrap_or(false),
            ExportContentRequest {
                query,
                query_json,
                read_filter,
                transforms: profile.content_transform_rules.as_deref(),
            },
        )?,
        ExportProfileFormat::Csv => run_csv_export_profile(
            cli.output,
            paths,
            &output_path,
            query,
            query_json,
            read_filter,
        )?,
        ExportProfileFormat::Graph => run_graph_export_profile(
            cli.output,
            paths,
            &output_path,
            read_filter,
            profile.graph_format,
        )?,
        ExportProfileFormat::Epub => run_epub_export_profile(
            cli.output,
            paths,
            &output_path,
            query,
            query_json,
            read_filter,
            &profile,
        )?,
        ExportProfileFormat::Zip => run_zip_export_profile(
            cli.output,
            paths,
            &output_path,
            ExportContentRequest {
                query,
                query_json,
                read_filter,
                transforms: profile.content_transform_rules.as_deref(),
            },
        )?,
        ExportProfileFormat::Sqlite => run_sqlite_export_profile(
            cli.output,
            paths,
            &output_path,
            query,
            query_json,
            read_filter,
        )?,
        ExportProfileFormat::SearchIndex => run_search_index_export_profile(
            cli.output,
            paths,
            &output_path,
            profile.pretty.unwrap_or(false),
        )?,
        ExportProfileFormat::FrontendBundle => {
            run_frontend_bundle_export_profile(cli.output, paths, name, &output_path, &profile)?
        }
    };

    if cli.output == OutputFormat::Json {
        print_json(&ExportProfileRunSummary {
            name: name.to_string(),
            format: export_profile_format_label(format).to_string(),
            summary,
        })?;
    }

    Ok(())
}

fn render_epub_message_html(title: &str, message: &str) -> String {
    format!(
        "<div class=\"render-message\"><strong>{}</strong> {}</div>",
        escape_xml_text(title),
        escape_xml_text(message)
    )
}

fn escape_xml_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn render_epub_dataview_block_markdown(
    paths: &VaultPaths,
    note_path: &str,
    language: &str,
    source: &str,
) -> String {
    if language == "dataview" {
        match evaluate_dql_with_filter(paths, source, Some(note_path), None) {
            Ok(result) => render_dql_query_markdown(&result, false),
            Err(error) => render_epub_message_html("Dataview error:", &error.to_string()),
        }
    } else if language == "dataviewjs" {
        match commands::dataview::run_dataview_query_js_command(
            paths,
            source,
            Some(note_path),
            None,
        ) {
            Ok(result) => render_dataview_js_markdown(&result, false),
            Err(error) => render_epub_message_html("DataviewJS error:", &error.to_string()),
        }
    } else {
        render_epub_message_html(
            "Dataview error:",
            &format!("unsupported block language `{language}`"),
        )
    }
}

fn render_epub_base_embed_markdown(
    paths: &VaultPaths,
    base_path: &str,
    view_name: Option<&str>,
) -> String {
    match evaluate_base_file(paths, base_path) {
        Ok(mut report) => {
            if let Some(view_name) = view_name.map(str::trim).filter(|value| !value.is_empty()) {
                if let Some(view) = report
                    .views
                    .iter()
                    .find(|view| view.name.as_deref() == Some(view_name))
                    .cloned()
                {
                    report.views = vec![view];
                } else {
                    return render_epub_message_html(
                        "Bases error:",
                        &format!("view `{view_name}` was not found in {base_path}"),
                    );
                }
            }

            render_bases_markdown(
                &report,
                &ListOutputControls {
                    fields: None,
                    limit: None,
                    offset: 0,
                },
            )
        }
        Err(error) => render_epub_message_html("Bases error:", &error.to_string()),
    }
}

fn render_graph_export_payload(
    report: &vulcan_core::GraphExportReport,
    format: GraphExportFormat,
) -> Result<String, CliError> {
    match format {
        GraphExportFormat::Json => serde_json::to_string(report).map_err(CliError::operation),
        GraphExportFormat::Dot => {
            let mut rendered = String::from("digraph vault {\n");
            for node in &report.nodes {
                let label = node.path.trim_end_matches(".md").replace('"', "\\\"");
                let id = node.path.replace('"', "\\\"");
                writeln!(rendered, "  \"{id}\" [label=\"{label}\"];")
                    .expect("writing to string cannot fail");
            }
            for edge in &report.edges {
                let source = edge.source.replace('"', "\\\"");
                let target = edge.target.replace('"', "\\\"");
                writeln!(
                    rendered,
                    "  \"{source}\" -> \"{target}\" [confidence=\"{}\", confidence_score=\"{:.3}\"];",
                    edge.confidence.as_str(),
                    edge.confidence_score
                )
                    .expect("writing to string cannot fail");
            }
            rendered.push_str("}\n");
            Ok(rendered)
        }
        GraphExportFormat::Graphml => {
            let mut rendered = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<graphml xmlns=\"http://graphml.graphdrawing.org/graphml\">\n  <graph id=\"vault\" edgedefault=\"directed\">\n",
            );
            for node in &report.nodes {
                let id = node.path.replace('"', "&quot;");
                writeln!(rendered, "    <node id=\"{id}\"/>")
                    .expect("writing to string cannot fail");
            }
            for (index, edge) in report.edges.iter().enumerate() {
                let source = edge.source.replace('"', "&quot;");
                let target = edge.target.replace('"', "&quot;");
                writeln!(
                    rendered,
                    "    <edge id=\"e{index}\" source=\"{source}\" target=\"{target}\"><data key=\"confidence\">{}</data><data key=\"confidence_score\">{:.3}</data></edge>",
                    edge.confidence.as_str(),
                    edge.confidence_score
                )
                .expect("writing to string cannot fail");
            }
            rendered.push_str("  </graph>\n</graphml>\n");
            Ok(rendered)
        }
    }
}

fn write_graph_export(
    output: OutputFormat,
    report: &vulcan_core::GraphExportReport,
    format: GraphExportFormat,
    path: Option<&PathBuf>,
) -> Result<(), CliError> {
    let payload = render_graph_export_payload(report, format)?;
    let summary = path.map(|path| GraphExportSummary {
        path: path.display().to_string(),
        format: match format {
            GraphExportFormat::Json => "json",
            GraphExportFormat::Dot => "dot",
            GraphExportFormat::Graphml => "graphml",
        }
        .to_string(),
        nodes: report.nodes.len(),
        edges: report.edges.len(),
    });
    match summary.as_ref() {
        Some(summary) => write_text_export(output, path, &payload, summary),
        None => write_text_export(output, path, &payload, &serde_json::json!({})),
    }
}

fn print_automation_run_report(
    output: OutputFormat,
    report: &AutomationRunReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Automation actions: {}", report.actions.join(", "));
            if let Some(scan) = report.scan.as_ref() {
                println!(
                    "- scan: {} added, {} updated, {} unchanged, {} deleted",
                    scan.added, scan.updated, scan.unchanged, scan.deleted
                );
            }
            if let Some(summary) = report.doctor_issues.as_ref() {
                println!(
                    "- doctor: unresolved={}, ambiguous={}, parse_failures={}, type_mismatches={}, unsupported_syntax={}, stale={}, missing={}",
                    summary.unresolved_links,
                    summary.ambiguous_links,
                    summary.parse_failures,
                    summary.type_mismatches,
                    summary.unsupported_syntax,
                    summary.stale_index_rows,
                    summary.missing_index_rows
                );
            }
            if let Some(fix) = report.doctor_fix.as_ref() {
                let summary = fix.issues_after.as_ref().unwrap_or(&fix.issues_before);
                println!(
                    "- doctor-fix: {} actions, unresolved={}, ambiguous={}, parse_failures={}, type_mismatches={}, unsupported_syntax={}, stale={}, missing={}",
                    fix.fixes.len(),
                    summary.unresolved_links,
                    summary.ambiguous_links,
                    summary.parse_failures,
                    summary.type_mismatches,
                    summary.unsupported_syntax,
                    summary.stale_index_rows,
                    summary.missing_index_rows
                );
            }
            if let Some(cache) = report.cache_verify.as_ref() {
                println!("- cache-verify: healthy={}", cache.healthy);
            }
            if let Some(fts) = report.repair_fts.as_ref() {
                println!(
                    "- repair-fts: {} chunks across {} documents",
                    fts.indexed_chunks, fts.indexed_documents
                );
            }
            if let Some(batch) = report.reports.as_ref() {
                println!(
                    "- saved-reports: {} succeeded, {} failed",
                    batch.succeeded, batch.failed
                );
            }
            if report.issues_detected {
                println!("Issues detected.");
            } else {
                println!("No issues detected.");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_cache_inspect_report(
    output: OutputFormat,
    report: &CacheInspectReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Cache: {}", report.cache_path);
            println!("Bytes: {}", report.database_bytes);
            println!("Documents: {}", report.documents);
            println!("Notes: {}", report.notes);
            println!("Attachments: {}", report.attachments);
            println!("Bases: {}", report.bases);
            println!("Links: {}", report.links);
            println!("Chunks: {}", report.chunks);
            println!("Diagnostics: {}", report.diagnostics);
            println!("Search rows: {}", report.search_rows);
            println!("Vector rows: {}", report.vector_rows);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_cache_verify_report(
    output: OutputFormat,
    report: &CacheVerifyReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Cache healthy: {}", report.healthy);
            for check in &report.checks {
                println!(
                    "- {} [{}] {}",
                    check.name,
                    if check.ok { "ok" } else { "fail" },
                    check.detail
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_cache_vacuum_report(
    output: OutputFormat,
    report: &CacheVacuumReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Dry run: cache is {} bytes", report.before_bytes);
            } else {
                println!(
                    "Vacuumed cache: {} -> {} bytes (reclaimed {})",
                    report.before_bytes,
                    report.after_bytes.unwrap_or(report.before_bytes),
                    report.reclaimed_bytes.unwrap_or(0)
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn resolve_vault_root(vault: &Path) -> Result<PathBuf, CliError> {
    let expanded = expand_home_path(vault).unwrap_or_else(|| vault.to_path_buf());

    if expanded.is_absolute() {
        return Ok(expanded);
    }

    Ok(std::env::current_dir()
        .map_err(|error| CliError::io(&error))?
        .join(expanded))
}

fn expand_home_path(path: &Path) -> Option<PathBuf> {
    let path_str = path.to_str()?;
    if path_str == "~" {
        return current_home_dir();
    }
    if let Some(rest) = path_str.strip_prefix("~/") {
        return current_home_dir().map(|home| home.join(rest));
    }
    #[cfg(windows)]
    if let Some(rest) = path_str.strip_prefix("~\\") {
        return current_home_dir().map(|home| home.join(rest));
    }
    None
}

fn current_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            let mut home = PathBuf::from(drive);
            home.push(path);
            Some(home)
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CliDescribeReport {
    name: String,
    about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_help: Option<String>,
    version: Option<String>,
    global_options: Vec<CliArgDescribe>,
    commands: Vec<CliCommandDescribe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CliCommandDescribe {
    name: String,
    about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_help: Option<String>,
    options: Vec<CliArgDescribe>,
    subcommands: Vec<CliCommandDescribe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CliArgDescribe {
    id: String,
    long: Option<String>,
    short: Option<char>,
    help: Option<String>,
    required: bool,
    value_names: Vec<String>,
    possible_values: Vec<String>,
}

fn describe_cli() -> CliDescribeReport {
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
struct OpenAiToolDefinition {
    #[serde(rename = "type")]
    kind: String,
    function: OpenAiFunctionDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct OpenAiFunctionDefinition {
    name: String,
    description: String,
    parameters: Value,
    examples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct McpToolsReport {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    #[serde(rename = "toolPackMode")]
    tool_pack_mode: String,
    #[serde(rename = "selectedToolPacks")]
    selected_tool_packs: Vec<String>,
    tools: Vec<McpToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct McpToolDefinition {
    name: String,
    title: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    output_schema: Option<Value>,
    annotations: McpToolAnnotations,
    #[serde(rename = "toolPacks")]
    tool_packs: Vec<String>,
    examples: Vec<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct McpToolAnnotations {
    #[serde(rename = "readOnlyHint")]
    read_only_hint: bool,
    #[serde(rename = "destructiveHint")]
    destructive_hint: bool,
    #[serde(rename = "idempotentHint")]
    idempotent_hint: bool,
    #[serde(rename = "openWorldHint")]
    open_world_hint: bool,
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
    fn into_openai_definition(self) -> OpenAiToolDefinition {
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

fn cli_command_tree() -> clap::Command {
    Cli::command().bin_name("vulcan")
}

fn collect_cli_leaf_tool_names(command: &clap::Command) -> Vec<String> {
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

fn resolve_help_topic(topic: &[String]) -> Result<HelpTopicReport, CliError> {
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

fn collect_help_command_topics(command: &clap::Command) -> Vec<HelpTopicReport> {
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

fn render_config_reference_markdown(include_title: bool) -> String {
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

fn print_link_section(title: &str, issues: &[DoctorLinkIssue]) {
    if issues.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for issue in issues {
        let path = issue.document_path.as_deref().unwrap_or("<unknown>");
        if issue.matches.is_empty() {
            if let Some(target) = issue.target.as_deref() {
                println!("- {path}: {target} ({})", issue.message);
            } else {
                println!("- {path}: {}", issue.message);
            }
        } else if let Some(target) = issue.target.as_deref() {
            println!(
                "- {path}: {target} ({}) [{}]",
                issue.message,
                issue.matches.join(", ")
            );
        } else {
            println!("- {path}: {} [{}]", issue.message, issue.matches.join(", "));
        }
    }
}

pub(crate) fn print_diagnostic_section(title: &str, issues: &[DoctorDiagnosticIssue]) {
    if issues.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for issue in issues {
        let path = issue.document_path.as_deref().unwrap_or("<unknown>");
        if let Some(byte_range) = issue.byte_range.as_ref() {
            println!(
                "- {path}: {} (bytes {}-{})",
                issue.message, byte_range.start, byte_range.end
            );
        } else {
            println!("- {path}: {}", issue.message);
        }
    }
}

fn print_path_section(title: &str, paths: &[String]) {
    if paths.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for path in paths {
        println!("- {path}");
    }
}

fn outgoing_link_rows(report: &OutgoingLinksReport, links: &[OutgoingLinkRecord]) -> Vec<Value> {
    links
        .iter()
        .map(|link| {
            serde_json::json!({
                "note_path": report.note_path,
                "matched_by": report.matched_by,
                "raw_text": link.raw_text,
                "link_kind": link.link_kind,
                "display_text": link.display_text,
                "target_path_candidate": link.target_path_candidate,
                "target_heading": link.target_heading,
                "target_block": link.target_block,
                "resolved_target_path": link.resolved_target_path,
                "resolution_status": link.resolution_status,
                "context": link.context,
            })
        })
        .collect()
}

fn backlink_rows(report: &BacklinksReport, backlinks: &[BacklinkRecord]) -> Vec<Value> {
    backlinks
        .iter()
        .map(|backlink| {
            serde_json::json!({
                "note_path": report.note_path,
                "matched_by": report.matched_by,
                "source_path": backlink.source_path,
                "raw_text": backlink.raw_text,
                "link_kind": backlink.link_kind,
                "display_text": backlink.display_text,
                "context": backlink.context,
            })
        })
        .collect()
}

fn search_hit_rows(report: &SearchReport, hits: &[SearchHit]) -> Vec<Value> {
    if hits.is_empty() && report.plan.is_some() {
        return vec![serde_json::json!({
            "query": report.query,
            "mode": report.mode,
            "tag": report.tag,
            "path_prefix": report.path_prefix,
            "has_property": report.has_property,
            "filters": report.filters,
            "effective_query": report.plan.as_ref().map(|plan| plan.effective_query.clone()),
            "parsed_query_explanation": report
                .plan
                .as_ref()
                .map(|plan| plan.parsed_query_explanation.clone()),
            "document_path": Value::Null,
            "chunk_id": Value::Null,
            "heading_path": Vec::<String>::new(),
            "snippet": Value::Null,
            "matched_line": Value::Null,
            "section_id": Value::Null,
            "line_spans": Vec::<vulcan_core::NoteLineSpan>::new(),
            "rank": Value::Null,
            "explain": Value::Null,
            "no_results": true,
        })];
    }

    hits.iter()
        .map(|hit| {
            serde_json::json!({
                "query": report.query,
                "mode": report.mode,
                "tag": report.tag,
                "path_prefix": report.path_prefix,
                "has_property": report.has_property,
                "filters": report.filters,
                "effective_query": report.plan.as_ref().map(|plan| plan.effective_query.clone()),
                "parsed_query_explanation": report
                    .plan
                    .as_ref()
                    .map(|plan| plan.parsed_query_explanation.clone()),
                "document_path": hit.document_path,
                "chunk_id": hit.chunk_id,
                "heading_path": hit.heading_path,
                "snippet": hit.snippet,
                "matched_line": hit.matched_line,
                "section_id": hit.section_id,
                "line_spans": hit.line_spans,
                "rank": hit.rank,
                "explain": hit.explain,
            })
        })
        .collect()
}

fn vector_neighbor_rows(report: &VectorNeighborsReport, hits: &[VectorNeighborHit]) -> Vec<Value> {
    hits.iter()
        .map(|hit| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "query_text": report.query_text,
                "note_path": report.note_path,
                "document_path": hit.document_path,
                "chunk_id": hit.chunk_id,
                "heading_path": hit.heading_path,
                "snippet": hit.snippet,
                "distance": hit.distance,
            })
        })
        .collect()
}

fn related_note_rows(report: &RelatedNotesReport, hits: &[RelatedNoteHit]) -> Vec<Value> {
    hits.iter()
        .map(|hit| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "note_path": report.note_path,
                "document_path": hit.document_path,
                "heading_path": hit.heading_path,
                "snippet": hit.snippet,
                "similarity": hit.similarity,
                "matched_chunks": hit.matched_chunks,
            })
        })
        .collect()
}

fn vector_duplicate_rows(
    report: &VectorDuplicatesReport,
    pairs: &[VectorDuplicatePair],
) -> Vec<Value> {
    pairs
        .iter()
        .map(|pair| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "threshold": report.threshold,
                "left_document_path": pair.left_document_path,
                "left_chunk_id": pair.left_chunk_id,
                "right_document_path": pair.right_document_path,
                "right_chunk_id": pair.right_chunk_id,
                "similarity": pair.similarity,
            })
        })
        .collect()
}

fn cluster_rows(report: &ClusterReport, clusters: &[vulcan_core::ClusterSummary]) -> Vec<Value> {
    clusters
        .iter()
        .map(|cluster| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "cluster_count": report.cluster_count,
                "cluster_id": cluster.cluster_id,
                "cluster_label": cluster.cluster_label,
                "keywords": cluster.keywords,
                "chunk_count": cluster.chunk_count,
                "document_count": cluster.document_count,
                "document_path": cluster.exemplar_document_path,
                "heading_path": cluster.exemplar_heading_path,
                "exemplar_document_path": cluster.exemplar_document_path,
                "exemplar_heading_path": cluster.exemplar_heading_path,
                "exemplar_snippet": cluster.exemplar_snippet,
                "top_documents": cluster.top_documents,
            })
        })
        .collect()
}

fn note_rows(report: &NotesReport, notes: &[NoteRecord]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "filters": report.filters,
                "sort_by": report.sort_by,
                "sort_descending": report.sort_descending,
                "document_path": note.document_path,
                "file_name": note.file_name,
                "file_ext": note.file_ext,
                "file_mtime": note.file_mtime,
                "tags": note.tags,
                "starred": note.starred,
                "properties": note.properties,
                "inline_expressions": note.inline_expressions,
            })
        })
        .collect()
}

fn bases_rows(report: &BasesEvalReport) -> Vec<Value> {
    report
        .views
        .iter()
        .flat_map(|view| {
            view.rows.iter().map(|row| {
                serde_json::json!({
                    "file": report.file,
                    "view_name": view.name,
                    "view_type": view.view_type,
                    "filters": view.filters,
                    "sort_by": view.sort_by,
                    "sort_descending": view.sort_descending,
                    "columns": view.columns,
                    "group_by": view.group_by,
                    "group_value": row.group_value,
                    "document_path": row.document_path,
                    "file_name": row.file_name,
                    "file_ext": row.file_ext,
                    "file_mtime": row.file_mtime,
                    "properties": row.properties,
                    "formulas": row.formulas,
                    "cells": row.cells,
                })
            })
        })
        .collect()
}

fn mention_suggestion_rows(suggestions: &[MentionSuggestion]) -> Vec<Value> {
    suggestions
        .iter()
        .map(|suggestion| {
            serde_json::json!({
                "kind": if suggestion.target_path.is_some() { "mention" } else { "ambiguous_mention" },
                "status": if suggestion.target_path.is_some() { "unambiguous" } else { "ambiguous" },
                "source_path": suggestion.source_path,
                "matched_text": suggestion.matched_text,
                "target_path": suggestion.target_path,
                "candidate_paths": suggestion.candidate_paths,
                "candidate_count": suggestion.candidate_paths.len(),
                "line": suggestion.line,
                "column": suggestion.column,
                "context": suggestion.context,
            })
        })
        .collect()
}

fn link_suggestion_rows(suggestions: &[LinkSuggestion]) -> Vec<Value> {
    suggestions
        .iter()
        .map(|suggestion| {
            serde_json::json!({
                "id": suggestion.id,
                "source_path": suggestion.source_path,
                "target_path": suggestion.target_path,
                "score": suggestion.score,
                "display_score": suggestion.display_score,
                "status": suggestion.status.as_str(),
                "created_at": suggestion.created_at,
                "accepted_at": suggestion.accepted_at,
                "rejected_at": suggestion.rejected_at,
                "embedding_score": suggestion.signals.embedding_score,
                "graph_score": suggestion.signals.graph_score,
                "mention_score": suggestion.signals.mention_score,
                "tag_score": suggestion.signals.tag_score,
                "cross_community_bonus": suggestion.signals.cross_community_bonus,
                "signals": suggestion.signals,
            })
        })
        .collect()
}

fn duplicate_suggestion_rows(report: &DuplicateSuggestionsReport) -> Vec<Value> {
    let mut rows = Vec::new();
    rows.extend(report.duplicate_titles.iter().map(|group| {
        serde_json::json!({
            "kind": "duplicate_title",
            "value": group.value,
            "paths": group.paths,
            "path_count": group.paths.len(),
            "left_path": Value::Null,
            "right_path": Value::Null,
            "score": Value::Null,
            "reasons": Value::Null,
        })
    }));
    rows.extend(report.alias_collisions.iter().map(|group| {
        serde_json::json!({
            "kind": "alias_collision",
            "value": group.value,
            "paths": group.paths,
            "path_count": group.paths.len(),
            "left_path": Value::Null,
            "right_path": Value::Null,
            "score": Value::Null,
            "reasons": Value::Null,
        })
    }));
    rows.extend(report.merge_candidates.iter().map(|candidate| {
        serde_json::json!({
            "kind": "merge_candidate",
            "value": Value::Null,
            "paths": Value::Null,
            "path_count": 2,
            "left_path": candidate.left_path,
            "right_path": candidate.right_path,
            "score": candidate.score,
            "reasons": candidate.reasons,
        })
    }));
    rows
}

fn saved_report_summary_rows(reports: &[SavedReportSummary]) -> Vec<Value> {
    reports
        .iter()
        .map(|report| {
            serde_json::json!({
                "name": report.name,
                "kind": report.kind,
                "description": report.description,
                "fields": report.fields,
                "limit": report.limit,
                "export_format": report.export.as_ref().map(|export| export.format),
                "export_path": report.export.as_ref().map(|export| export.path.clone()),
            })
        })
        .collect()
}

fn print_outgoing_link(link: &OutgoingLinkRecord) {
    let target = link
        .resolved_target_path
        .as_deref()
        .or(link.target_path_candidate.as_deref())
        .unwrap_or("(self)");

    if let Some(context) = link.context.as_ref() {
        println!(
            "- {} [{} {:?}] line {}: {}",
            target, link.link_kind, link.resolution_status, context.line, link.raw_text
        );
    } else {
        println!(
            "- {} [{} {:?}]: {}",
            target, link.link_kind, link.resolution_status, link.raw_text
        );
    }
}

fn print_backlink(backlink: &BacklinkRecord) {
    if let Some(context) = backlink.context.as_ref() {
        println!(
            "- {} [{}] line {}: {}",
            backlink.source_path, backlink.link_kind, context.line, backlink.raw_text
        );
    } else {
        println!(
            "- {} [{}]: {}",
            backlink.source_path, backlink.link_kind, backlink.raw_text
        );
    }
}

fn print_mention_suggestion(suggestion: &MentionSuggestion, palette: AnsiPalette) {
    let location = format!(
        "{}:{}:{}",
        suggestion.source_path, suggestion.line, suggestion.column
    );
    let summary = match suggestion.target_path.as_deref() {
        Some(target_path) => format!(
            "{} -> {}",
            palette.bold(&suggestion.matched_text),
            target_path
        ),
        None => format!(
            "{} -> {}",
            palette.bold(&suggestion.matched_text),
            suggestion.candidate_paths.join(", ")
        ),
    };
    let label = if suggestion.target_path.is_some() {
        palette.green("link")
    } else {
        palette.yellow("review")
    };
    println!("- {location} [{label}] {summary}");
    println!("  {}", suggestion.context.trim());
}

fn print_duplicate_groups(title: &str, groups: &[vulcan_core::DuplicateGroup]) {
    if groups.is_empty() {
        return;
    }

    println!("{title}:");
    for group in groups {
        println!("- {} -> {}", group.value, group.paths.join(", "));
    }
    println!();
}

fn print_merge_candidates(candidates: &[MergeCandidate], palette: AnsiPalette) {
    if candidates.is_empty() {
        return;
    }

    println!("Merge candidates:");
    for candidate in candidates {
        println!(
            "- {} <-> {} ({:.2})",
            candidate.left_path, candidate.right_path, candidate.score
        );
        println!("  {}", palette.dim(&candidate.reasons.join(", ")));
    }
}

fn print_search_hit(index: usize, hit: &SearchHit, palette: AnsiPalette) {
    let location = if hit.heading_path.is_empty() {
        hit.document_path.clone()
    } else {
        format!("{} > {}", hit.document_path, hit.heading_path.join(" > "))
    };

    println!("{}. {}", index + 1, palette.bold(&location));
    println!("   {}: {:.3}", palette.cyan("Rank"), hit.rank);
    if let Some(line) = hit.matched_line {
        println!("   {}: {line}", palette.cyan("Line"));
    }

    if let Some(explain) = hit.explain.as_ref() {
        println!(
            "   {}: {}",
            palette.cyan("Explain"),
            render_search_hit_explain(explain)
        );
    }

    let lines = hit
        .snippet
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if let Some((first, rest)) = lines.split_first() {
        println!("   {}: {first}", palette.cyan("Snippet"));
        for line in rest {
            println!("            {line}");
        }
    } else {
        println!("   {}: <empty>", palette.cyan("Snippet"));
    }

    println!();
}

fn print_vector_neighbor(index: usize, hit: &VectorNeighborHit, palette: AnsiPalette) {
    print_ranked_snippet_hit(
        index,
        &hit.document_path,
        &hit.heading_path,
        "Distance",
        f64::from(hit.distance),
        &hit.snippet,
        palette,
    );
}

fn print_vector_duplicate(pair: &VectorDuplicatePair) {
    println!(
        "- {} <-> {} [{:.3}]",
        pair.left_document_path, pair.right_document_path, pair.similarity
    );
}

fn print_cluster_summary(
    index: usize,
    cluster: &vulcan_core::ClusterSummary,
    palette: AnsiPalette,
) {
    println!(
        "{}. {}",
        index + 1,
        palette.bold(&format!(
            "[{}] {}",
            cluster.cluster_id + 1,
            cluster.cluster_label
        ))
    );
    println!(
        "   {}: {} chunks across {} notes",
        palette.cyan("Size"),
        cluster.chunk_count,
        cluster.document_count
    );
    println!(
        "   {}: {}",
        palette.cyan("Exemplar"),
        if cluster.exemplar_heading_path.is_empty() {
            cluster.exemplar_document_path.clone()
        } else {
            format!(
                "{} > {}",
                cluster.exemplar_document_path,
                cluster.exemplar_heading_path.join(" > ")
            )
        }
    );
    let snippet_lines = cluster.exemplar_snippet.lines().collect::<Vec<&str>>();
    if let Some((first, rest)) = snippet_lines.split_first() {
        println!("   {}: {}", palette.cyan("Snippet"), first.trim());
        for line in rest
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
        {
            println!("            {line}");
        }
    }
    if !cluster.top_documents.is_empty() {
        println!("   {}:", palette.cyan("Top notes"));
        for document in &cluster.top_documents {
            println!("   - {} ({})", document.document_path, document.chunk_count);
        }
    }
    println!();
}

fn print_ranked_snippet_hit(
    index: usize,
    document_path: &str,
    heading_path: &[String],
    metric_label: &str,
    metric_value: f64,
    snippet: &str,
    palette: AnsiPalette,
) {
    let location = if heading_path.is_empty() {
        document_path.to_string()
    } else {
        format!("{document_path} > {}", heading_path.join(" > "))
    };

    println!("{}. {}", index + 1, palette.bold(&location));
    println!("   {}: {metric_value:.3}", palette.cyan(metric_label));

    let lines = snippet
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if let Some((first, rest)) = lines.split_first() {
        println!("   {}: {first}", palette.cyan("Snippet"));
        for line in rest {
            println!("            {line}");
        }
    } else {
        println!("   {}: <empty>", palette.cyan("Snippet"));
    }

    println!();
}

fn print_search_plan(plan: &vulcan_core::SearchPlan, palette: AnsiPalette) {
    println!("{}: {}", palette.cyan("Plan"), plan.effective_query);
    if !plan.semantic_text.is_empty() && plan.semantic_text != plan.effective_query {
        println!("{}: {}", palette.cyan("Vector text"), plan.semantic_text);
    }
    if let Some(tag) = plan.tag.as_deref() {
        println!("{}: {tag}", palette.cyan("Tag"));
    }
    if let Some(path_prefix) = plan.path_prefix.as_deref() {
        println!("{}: {path_prefix}", palette.cyan("Path prefix"));
    }
    if let Some(has_property) = plan.has_property.as_deref() {
        println!("{}: {has_property}", palette.cyan("Has property"));
    }
    if !plan.property_filters.is_empty() {
        println!(
            "{}: {}",
            palette.cyan("Filters"),
            plan.property_filters.join(" | ")
        );
    }
    if !plan.parsed_query_explanation.is_empty() {
        println!("{}:", palette.cyan("Query plan"));
        for line in &plan.parsed_query_explanation {
            println!("  {line}");
        }
    }
    if plan.fuzzy_fallback_used {
        for expansion in &plan.fuzzy_expansions {
            println!(
                "{}: {} -> {}",
                palette.cyan("Fuzzy"),
                expansion.term,
                expansion.candidates.join(", ")
            );
        }
    }
    println!();
}

fn render_search_hit_explain(explain: &vulcan_core::SearchHitExplain) -> String {
    match explain.strategy.as_str() {
        "keyword" => format!(
            "bm25={:.3}, keyword_rank={}",
            explain.bm25.unwrap_or(explain.score),
            explain.keyword_rank.unwrap_or(1)
        ),
        "hybrid" => {
            let mut parts = vec![format!("score={:.3}", explain.score)];
            if let Some(rank) = explain.keyword_rank {
                parts.push(format!(
                    "keyword#{} ({:.3})",
                    rank,
                    explain.keyword_contribution.unwrap_or_default()
                ));
            }
            if let Some(rank) = explain.vector_rank {
                parts.push(format!(
                    "vector#{} ({:.3})",
                    rank,
                    explain.vector_contribution.unwrap_or_default()
                ));
            }
            parts.join(", ")
        }
        _ => format!("score={:.3}", explain.score),
    }
}

/// Format a slice of JSON objects as an aligned text table.
///
/// The first row is used as column headers. Each column is padded to the widest
/// value (header or data) in that column.  When `use_color` is true the header
/// row is rendered in bold.
fn print_aligned_table(rows: &[Value], fields: &[String], no_header: bool, use_color: bool) {
    if rows.is_empty() {
        return;
    }

    // Collect cell strings for every row.
    let data: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            fields
                .iter()
                .map(|f| {
                    let v = select_fields(row.clone(), Some(std::slice::from_ref(f)));
                    match v.get(f) {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Null) | None => String::new(),
                        Some(other) => render_human_value(other),
                    }
                })
                .collect()
        })
        .collect();

    // Compute column widths.
    let mut widths: Vec<usize> = fields.iter().map(String::len).collect();
    for row in &data {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }

    let palette = AnsiPalette::new(use_color);

    // Print header.
    if !no_header {
        let header: String = fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let padded = format!("{:<width$}", f, width = widths[i]);
                palette.bold(&padded)
            })
            .collect::<Vec<_>>()
            .join("  ");
        println!("{header}");
        let sep: String = widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  ");
        println!("{}", palette.dim(&sep));
    }

    // Print data rows.
    for row in &data {
        let line: String = row
            .iter()
            .enumerate()
            .map(|(i, cell)| format!("{:<width$}", cell, width = widths[i]))
            .collect::<Vec<_>>()
            .join("  ");
        println!("{line}");
    }
}

fn print_note(note: &NoteRecord) {
    println!("- {}", note.document_path);
}

fn render_bases_markdown(report: &BasesEvalReport, list_controls: &ListOutputControls) -> String {
    let mut row_index = 0_usize;
    let mut printed_any = false;
    let end = list_controls.limit.map_or(usize::MAX, |limit| {
        list_controls.offset.saturating_add(limit)
    });
    let mut sections = Vec::new();

    for view in &report.views {
        let mut visible_rows = Vec::new();
        for row in &view.rows {
            if row_index < list_controls.offset {
                row_index += 1;
                continue;
            }
            if row_index >= end {
                break;
            }
            visible_rows.push(row);
            row_index += 1;
        }

        if !visible_rows.is_empty() {
            sections.push(render_bases_view_markdown(view, &visible_rows));
            printed_any = true;
        }

        if row_index >= end {
            break;
        }
    }

    if !printed_any {
        sections.push("No bases rows.".to_string());
    }

    if !report.diagnostics.is_empty() {
        let mut diagnostics = vec!["## Diagnostics".to_string()];
        diagnostics.extend(report.diagnostics.iter().map(|diagnostic| {
            if let Some(path) = diagnostic.path.as_deref() {
                format!("- {path}: {}", diagnostic.message)
            } else {
                format!("- {}", diagnostic.message)
            }
        }));
        sections.push(diagnostics.join("\n"));
    }

    sections.join("\n\n")
}

fn render_bases_view_markdown(
    view: &vulcan_core::BasesEvaluatedView,
    rows: &[&vulcan_core::BasesRow],
) -> String {
    let visible_rows = rows.len();
    let name = view.name.as_deref().unwrap_or("view");
    let row_summary = if visible_rows == view.rows.len() {
        format!("{} rows", view.rows.len())
    } else {
        format!("{visible_rows} of {} rows", view.rows.len())
    };
    let mut lines = vec![format!("## {name} ({row_summary})")];
    if !view.columns.is_empty() {
        let columns = view
            .columns
            .iter()
            .map(|column| column.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Columns: {columns}"));
    }
    if let Some(group_by) = view.group_by.as_ref() {
        lines.push(format!(
            "Grouped by: {}{}",
            group_by.display_name,
            if group_by.descending { " (desc)" } else { "" }
        ));
    }
    lines.push(render_bases_table_markdown(view, rows));
    lines.join("\n\n")
}

fn render_bases_table_markdown(
    view: &vulcan_core::BasesEvaluatedView,
    rows: &[&vulcan_core::BasesRow],
) -> String {
    let group_key = view
        .group_by
        .as_ref()
        .map(|group_by| group_by.property.as_str());
    let mut columns = view
        .columns
        .iter()
        .filter(|column| Some(column.key.as_str()) != group_key)
        .collect::<Vec<_>>();
    if columns.is_empty() {
        columns = view.columns.iter().collect();
    }

    let mut sections = Vec::new();
    if view.group_by.is_some() {
        let mut start = 0_usize;
        while start < rows.len() {
            let group_name = bases_group_name(rows[start]);
            let mut end = start + 1;
            while end < rows.len() && bases_group_name(rows[end]) == group_name {
                end += 1;
            }

            let group_rows = &rows[start..end];
            sections.push(format!(
                "### Group: {group_name} ({} rows)\n\n{}",
                group_rows.len(),
                render_bases_table_block_markdown(&columns, group_rows)
            ));
            start = end;
        }
    } else {
        sections.push(render_bases_table_block_markdown(&columns, rows));
    }
    sections.join("\n\n")
}

fn render_bases_table_block_markdown(
    columns: &[&vulcan_core::BasesColumn],
    rows: &[&vulcan_core::BasesRow],
) -> String {
    let headers = columns
        .iter()
        .map(|column| column.display_name.clone())
        .collect::<Vec<_>>();
    let column_count = headers.len();
    if column_count == 0 {
        return String::new();
    }

    let [header, separator] = markdown_table_header_lines(&headers, column_count);
    let mut lines = vec![header, separator];

    lines.extend(rows.iter().map(|row| {
        markdown_table_row(
            columns
                .iter()
                .map(|column| bases_cell_text(row, &column.key)),
            column_count,
        )
    }));
    lines.join("\n")
}

fn bases_group_name(row: &vulcan_core::BasesRow) -> String {
    row.group_value
        .as_ref()
        .map(render_human_value)
        .filter(|value| !value.is_empty() && value != "null")
        .unwrap_or_else(|| "Ungrouped".to_string())
}

fn bases_cell_text(row: &vulcan_core::BasesRow, key: &str) -> String {
    bases_value_for_key(row, key)
        .filter(|value| !value.is_null())
        .map(|value| render_human_value(&value))
        .filter(|value| !value.is_empty() && value != "null")
        .unwrap_or_else(|| "-".to_string())
}

fn bases_value_for_key(row: &vulcan_core::BasesRow, key: &str) -> Option<Value> {
    if let Some(value) = row.cells.get(key) {
        return Some(value.clone());
    }
    if let Some(value) = row.formulas.get(key) {
        return Some(value.clone());
    }

    match key {
        "file.path" => Some(Value::String(row.document_path.clone())),
        "file.name" => Some(Value::String(row.file_name.clone())),
        "file.ext" => Some(Value::String(row.file_ext.clone())),
        "file.mtime" => Some(Value::Number(row.file_mtime.into())),
        property => row.properties.get(property).cloned(),
    }
}

fn print_named_count_section(title: &str, counts: &[NamedCount]) {
    if counts.is_empty() {
        return;
    }
    println!("{title}:");
    for count in counts {
        println!("- {} ({})", count.name, count.count);
    }
}

fn zero_summary() -> vulcan_core::DoctorSummary {
    vulcan_core::DoctorSummary {
        unresolved_links: 0,
        ambiguous_links: 0,
        broken_embeds: 0,
        parse_failures: 0,
        type_mismatches: 0,
        unsupported_syntax: 0,
        stale_index_rows: 0,
        missing_index_rows: 0,
        orphan_notes: 0,
        orphan_assets: 0,
        html_links: 0,
    }
}

fn graph_hub_rows(notes: &[vulcan_core::GraphNodeScore]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "inbound": note.inbound,
                "outbound": note.outbound,
                "total": note.total,
                "confidence": note.confidence,
            })
        })
        .collect()
}

fn graph_moc_rows(notes: &[GraphMocCandidate]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "inbound": note.inbound,
                "outbound": note.outbound,
                "score": note.score,
                "reasons": note.reasons,
            })
        })
        .collect()
}

fn graph_dead_end_rows(notes: &[String]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| serde_json::json!({ "document_path": note }))
        .collect()
}

fn graph_component_rows(components: &[vulcan_core::GraphComponent]) -> Vec<Value> {
    components
        .iter()
        .map(|component| {
            serde_json::json!({
                "size": component.size,
                "notes": component.notes,
            })
        })
        .collect()
}

fn graph_community_rows(
    report: &GraphCommunitiesReport,
    community: Option<usize>,
    orphans: bool,
    bridges: bool,
) -> Vec<Value> {
    if orphans {
        return report
            .orphans
            .iter()
            .map(|orphan| {
                serde_json::json!({
                    "kind": "orphan",
                    "document_path": orphan.document_path,
                    "closest_community": orphan.closest_community,
                    "tag_overlap": orphan.tag_overlap,
                })
            })
            .collect();
    }
    if bridges {
        return report
            .bridges
            .iter()
            .map(|bridge| {
                serde_json::json!({
                    "kind": "bridge",
                    "document_path": bridge.document_path,
                    "community_id": bridge.community_id,
                    "cross_community_edges": bridge.cross_community_edges,
                    "betweenness_score": bridge.betweenness_score,
                })
            })
            .collect();
    }
    report
        .communities
        .iter()
        .filter(|candidate| match community {
            Some(id) => candidate.id == id,
            None => true,
        })
        .map(|community| {
            serde_json::json!({
                "kind": "community",
                "id": community.id,
                "label": community.label,
                "size": community.size,
                "cohesion": community.cohesion,
                "top_nodes": community.top_nodes,
                "boundary_notes": community.boundary_notes,
                "inter_community_edges": community.inter_community_edges,
                "notes": community.notes,
                "persisted": report.persisted,
            })
        })
        .collect()
}

fn graph_analytics_rows(report: &GraphAnalyticsReport) -> Vec<Value> {
    vec![serde_json::json!({
        "note_count": report.note_count,
        "attachment_count": report.attachment_count,
        "base_count": report.base_count,
        "resolved_note_links": report.resolved_note_links,
        "average_outbound_links": report.average_outbound_links,
        "orphan_notes": report.orphan_notes,
        "confidence": report.confidence,
        "top_tags": report.top_tags,
        "top_properties": report.top_properties,
    })]
}

fn graph_trend_rows(report: &GraphTrendsReport) -> Vec<Value> {
    report
        .points
        .iter()
        .map(|point| {
            serde_json::json!({
                "label": point.label,
                "created_at": point.created_at,
                "note_count": point.note_count,
                "orphan_notes": point.orphan_notes,
                "stale_notes": point.stale_notes,
                "resolved_links": point.resolved_links,
            })
        })
        .collect()
}

fn checkpoint_rows(records: &[CheckpointRecord]) -> Vec<Value> {
    records
        .iter()
        .map(|record| {
            serde_json::json!({
                "id": record.id,
                "name": record.name,
                "source": record.source,
                "created_at": record.created_at,
                "note_count": record.note_count,
                "orphan_notes": record.orphan_notes,
                "stale_notes": record.stale_notes,
                "resolved_links": record.resolved_links,
            })
        })
        .collect()
}

fn change_rows(report: &ChangeReport) -> Vec<Value> {
    let mut rows = Vec::new();
    append_change_rows(&mut rows, &report.anchor, ChangeKind::Note, &report.notes);
    append_change_rows(&mut rows, &report.anchor, ChangeKind::Link, &report.links);
    append_change_rows(
        &mut rows,
        &report.anchor,
        ChangeKind::Property,
        &report.properties,
    );
    append_change_rows(
        &mut rows,
        &report.anchor,
        ChangeKind::Embedding,
        &report.embeddings,
    );
    rows
}

fn append_change_rows(rows: &mut Vec<Value>, anchor: &str, kind: ChangeKind, items: &[ChangeItem]) {
    let kind_name = serde_json::to_value(kind)
        .expect("change kind should serialize")
        .as_str()
        .expect("change kind should serialize to a string")
        .to_string();
    for item in items {
        rows.push(serde_json::json!({
            "anchor": anchor,
            "kind": kind_name,
            "path": item.path,
            "status": item.status,
        }));
    }
}

// ────────────────────────────────────────────────────────────────────────────
// vulcan status
// ────────────────────────────────────────────────────────────────────────────

fn run_status_command(paths: &VaultPaths) -> Result<VaultStatusReport, CliError> {
    app_build_vault_status_report(paths).map_err(CliError::operation)
}

fn print_status_report(
    output: OutputFormat,
    report: &VaultStatusReport,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let palette = AnsiPalette::new(use_color);
            println!("Vault:      {}", report.vault_root);
            println!(
                "Notes:      {}  attachments: {}",
                report.note_count, report.attachment_count
            );
            println!("Cache:      {} bytes", report.cache_bytes);
            if let Some(last_scan) = &report.last_scan {
                println!("Last scan:  {last_scan}");
            } else {
                println!("Last scan:  {}", palette.dim("never"));
            }
            if let Some(confidence) = &report.graph_confidence {
                println!(
                    "Confidence: {} EXTRACTED, {} INFERRED, {} AMBIGUOUS",
                    confidence.extracted, confidence.inferred, confidence.ambiguous
                );
            }
            if let Some(branch) = &report.git_branch {
                let dirty_flag = if report.git_dirty {
                    format!(
                        " {}",
                        palette.yellow(&format!(
                            "(dirty: {} staged, {} unstaged, {} untracked)",
                            report.git_staged, report.git_unstaged, report.git_untracked
                        ))
                    )
                } else {
                    format!(" {}", palette.green("(clean)"))
                };
                println!("Git:        {branch}{dirty_flag}");
            } else {
                println!("Git:        {}", palette.dim("not a git repository"));
            }
            Ok(())
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// graph export
// ────────────────────────────────────────────────────────────────────────────

fn print_graph_export_report(
    output: OutputFormat,
    report: &vulcan_core::GraphExportReport,
    format: GraphExportFormat,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            match format {
                GraphExportFormat::Json => {
                    print_json(report)?;
                }
                GraphExportFormat::Dot => {
                    println!("digraph vault {{");
                    for node in &report.nodes {
                        let label = node.path.trim_end_matches(".md").replace('"', "\\\"");
                        let id = node.path.replace('"', "\\\"");
                        println!("  \"{id}\" [label=\"{label}\"];");
                    }
                    for edge in &report.edges {
                        let src = edge.source.replace('"', "\\\"");
                        let tgt = edge.target.replace('"', "\\\"");
                        println!(
                            "  \"{src}\" -> \"{tgt}\" [confidence=\"{}\", confidence_score=\"{:.3}\"];",
                            edge.confidence.as_str(),
                            edge.confidence_score
                        );
                    }
                    println!("}}");
                }
                GraphExportFormat::Graphml => {
                    println!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
                    println!("<graphml xmlns=\"http://graphml.graphdrawing.org/graphml\">");
                    println!("  <graph id=\"vault\" edgedefault=\"directed\">");
                    for node in &report.nodes {
                        let id = node.path.replace('"', "&quot;");
                        println!("    <node id=\"{id}\"/>");
                    }
                    for (i, edge) in report.edges.iter().enumerate() {
                        let src = edge.source.replace('"', "&quot;");
                        let tgt = edge.target.replace('"', "&quot;");
                        println!(
                            "    <edge id=\"e{i}\" source=\"{src}\" target=\"{tgt}\"><data key=\"confidence\">{}</data><data key=\"confidence_score\">{:.3}</data></edge>",
                            edge.confidence.as_str(),
                            edge.confidence_score
                        );
                    }
                    println!("  </graph>");
                    println!("</graphml>");
                }
            }
            Ok(())
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// template show
// ────────────────────────────────────────────────────────────────────────────

fn run_complete_command(paths: &VaultPaths, context: &str, prefix: Option<&str>) {
    let candidates = collect_complete_candidates(paths, context, prefix);
    for candidate in &candidates {
        println!("{candidate}");
    }
}

/// Candidates for contexts that require no vault (safe to call before path resolution).
fn collect_complete_candidates_no_vault(context: &str, prefix: Option<&str>) -> Vec<String> {
    let candidates = match context {
        "daily-date" => {
            let mut dates = vec![
                "today".to_string(),
                "yesterday".to_string(),
                "tomorrow".to_string(),
            ];
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            for offset in 1..=14i64 {
                let past_secs = now_secs - offset * 86400;
                let ms = past_secs * 1000;
                let (year, month, day, _, _, _, _) = date_components(ms);
                dates.push(format!("{year:04}-{month:02}-{day:02}"));
            }
            dates
        }
        _ => Vec::new(),
    };
    filter_completion_candidates(candidates, prefix)
}

fn filter_completion_candidates(mut candidates: Vec<String>, prefix: Option<&str>) -> Vec<String> {
    let Some(prefix) = prefix.filter(|value| !value.is_empty()) else {
        return candidates;
    };
    candidates.retain(|candidate| candidate.starts_with(prefix));
    candidates
}

fn collect_vault_path_candidates(paths: &VaultPaths, prefix: Option<&str>) -> Vec<String> {
    let prefix = prefix.unwrap_or_default().replace('\\', "/");
    let trimmed = prefix.trim_start_matches("./");
    let (dir_prefix, partial_name) = match trimmed.rsplit_once('/') {
        Some((directory, partial)) => (directory.trim_end_matches('/'), partial),
        None => ("", trimmed),
    };

    if Path::new(dir_prefix).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Vec::new();
    }

    let directory = if dir_prefix.is_empty() {
        paths.vault_root().to_path_buf()
    } else {
        paths.vault_root().join(dir_prefix)
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut candidates = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.is_empty() {
            continue;
        }
        if dir_prefix.is_empty() && matches!(name.as_str(), ".git" | ".vulcan") {
            continue;
        }
        if !name.starts_with(partial_name) {
            continue;
        }

        let mut candidate = if dir_prefix.is_empty() {
            name
        } else {
            format!("{dir_prefix}/{name}")
        };
        if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            candidate.push('/');
        }
        candidates.insert(candidate.replace('\\', "/"));
    }
    candidates.into_iter().collect()
}

#[allow(clippy::too_many_lines)]
fn collect_complete_candidates(
    paths: &VaultPaths,
    context: &str,
    prefix: Option<&str>,
) -> Vec<String> {
    if context != "daily-date" {
        let vault_free = collect_complete_candidates_no_vault(context, prefix);
        if !vault_free.is_empty() {
            return vault_free;
        }
    }

    let candidates = match context {
        "vault-path" => collect_vault_path_candidates(paths, prefix),
        "script" => {
            let scripts_dir = paths.vulcan_dir().join("scripts");
            if !scripts_dir.is_dir() {
                return Vec::new();
            }
            fs::read_dir(&scripts_dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter_map(|entry| {
                            let name = entry.file_name();
                            let candidate = name.to_string_lossy();
                            if candidate.ends_with(".js") {
                                Some(candidate.trim_end_matches(".js").to_string())
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
        _ => app_collect_complete_candidates(paths, context).unwrap_or_default(),
    };
    if context == "vault-path" {
        candidates
    } else {
        filter_completion_candidates(candidates, prefix)
    }
}

/// `clap_complete` generates Fish nested-subcommand completions with a condition like
///   `__fish_seen_subcommand_from PARENT`
/// but never adds a `not __fish_seen_subcommand_from CHILD1 CHILD2` guard.  That
/// means that once the user types e.g. `tasks view show`, Fish still offers `show`
/// and `list` as candidates for the next word (cycling ad nauseam).
///
/// This function collects all lines that:
///   1. have a condition ending with `; and __fish_seen_subcommand_from \w+`  (no `not` after)
///   2. offer a bare word subcommand via `-f -a "word"`
///
/// groups them by condition, then re-emits those lines with the missing
/// `; and not __fish_seen_subcommand_from WORD1 WORD2 …` guard appended.
fn fix_fish_nested_subcommand_guards(script: &str) -> String {
    use std::collections::HashMap;

    // Capture (full_line, condition_base, subcommand_word)
    // Pattern: ...-n "COND" ... -f -a "WORD" ...
    // where COND ends with `; and __fish_seen_subcommand_from \w+` (no trailing `; and not`)
    // Match lines of the form:
    //   complete -c vulcan -n "COND; and __fish_seen_subcommand_from WORD" -f -a "SUB" -d '...'
    // where the condition ends with `__fish_seen_subcommand_from WORD` (no further `; and ...`).
    // These are the nested-subcommand candidate lines that need a `not` guard.
    let line_re = Regex::new(
        r#"^(complete -c vulcan -n ")(.*; and __fish_seen_subcommand_from \w+)(" .*-f -a ")(\w+)(" .*)$"#,
    )
    .expect("regex should compile");

    let mut condition_to_words: HashMap<String, Vec<String>> = HashMap::new();
    for line in script.lines() {
        if let Some(caps) = line_re.captures(line) {
            let cond = caps[2].to_string();
            let word = caps[4].to_string();
            condition_to_words.entry(cond).or_default().push(word);
        }
    }

    // Only patch conditions that have more than one subcommand — a single-child
    // parent has nothing else to cycle through so no guard is needed.
    let mut out = String::with_capacity(script.len() + 512);
    for line in script.lines() {
        if let Some(caps) = line_re.captures(line) {
            let cond = caps[2].to_string();
            if let Some(words) = condition_to_words.get(&cond) {
                if words.len() > 1 {
                    let not_guard =
                        format!("; and not __fish_seen_subcommand_from {}", words.join(" "));
                    let patched = format!(
                        "{}{}{}{}{}{}",
                        &caps[1], &caps[2], not_guard, &caps[3], &caps[4], &caps[5]
                    );
                    out.push_str(&patched);
                    out.push('\n');
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if !script.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

fn generate_dynamic_completions(shell: clap_complete::Shell) -> String {
    match shell {
        clap_complete::Shell::Fish => generate_fish_dynamic_completions(),
        clap_complete::Shell::Bash => generate_bash_dynamic_completions(),
        clap_complete::Shell::Zsh => generate_zsh_dynamic_completions(),
        _ => String::new(),
    }
}

fn shell_double_quote_literal(value: &str) -> String {
    let mut rendered = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => rendered.push_str("\\\\"),
            '"' => rendered.push_str("\\\""),
            '$' => rendered.push_str("\\$"),
            '`' => rendered.push_str("\\`"),
            _ => rendered.push(ch),
        }
    }
    rendered
}

fn completion_command_path_literal() -> String {
    std::env::current_exe().ok().map_or_else(
        || "vulcan".to_string(),
        |path| shell_double_quote_literal(&path.to_string_lossy()),
    )
}

fn generate_fish_dynamic_completions() -> String {
    render_dynamic_completion_template(include_str!("completions_fish.fish"))
}

fn render_dynamic_completion_template(template: &str) -> String {
    template
        .trim()
        .to_string()
        .replace("__VULCAN_CMD__", &completion_command_path_literal())
}

fn generate_bash_dynamic_completions() -> String {
    render_dynamic_completion_template(include_str!("completions_bash.sh"))
}

fn generate_zsh_dynamic_completions() -> String {
    render_dynamic_completion_template(include_str!("completions_zsh.zsh"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_yaml::Value as YamlValue;
    use std::fs;
    use std::process::Command as ProcessCommand;
    use tempfile::TempDir;
    use vulcan_core::expression::functions::parse_date_like_string;

    const CLI_TEST_STACK_BYTES: &str = "8388608";

    extern "C" fn configure_cli_test_thread_stack() {
        std::env::set_var("RUST_MIN_STACK", CLI_TEST_STACK_BYTES);
    }

    #[used]
    #[cfg_attr(
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ),
        link_section = ".init_array"
    )]
    #[cfg_attr(
        any(target_os = "macos", target_os = "ios"),
        link_section = "__DATA,__mod_init_func"
    )]
    #[cfg_attr(target_os = "windows", link_section = ".CRT$XCU")]
    static CONFIGURE_CLI_TEST_THREAD_STACK: extern "C" fn() = configure_cli_test_thread_stack;

    fn run_git(vault_root: &Path, args: &[&str]) {
        let status = ProcessCommand::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(args)
            .status()
            .expect("git should launch");
        assert!(status.success(), "git command failed: {args:?}");
    }

    fn init_git_repo(vault_root: &Path) {
        run_git(vault_root, &["-c", "init.defaultBranch=main", "init"]);
        run_git(vault_root, &["config", "user.name", "Vulcan Test"]);
        run_git(vault_root, &["config", "user.email", "vulcan@example.com"]);
    }

    fn git_head_summary(vault_root: &Path) -> String {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(["log", "-1", "--pretty=%s"])
            .output()
            .expect("git log should launch");
        assert!(output.status.success(), "git log should succeed");
        String::from_utf8(output.stdout)
            .expect("git stdout should be utf8")
            .trim()
            .to_string()
    }

    #[test]
    fn formats_eta_compactly_for_progress_reporting() {
        assert_eq!(format_eta(0, 12.0), "0s");
        assert_eq!(format_eta(5, 10.0), "<1s");
        assert_eq!(format_eta(120, 10.0), "12.0s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 5s");
    }

    #[test]
    fn query_ast_rendering_is_hidden_by_default() {
        assert!(!should_render_query_ast(OutputFormat::Human, false, false));
        assert!(!should_render_query_ast(
            OutputFormat::Markdown,
            false,
            false
        ));
        assert!(!should_render_query_ast(OutputFormat::Json, false, false));
    }

    #[test]
    fn query_ast_rendering_requires_explicit_diagnostics() {
        assert!(should_render_query_ast(OutputFormat::Human, true, false));
        assert!(should_render_query_ast(OutputFormat::Json, true, false));
        assert!(should_render_query_ast(OutputFormat::Human, false, true));
        assert!(should_render_query_ast(OutputFormat::Markdown, false, true));
        assert!(!should_render_query_ast(OutputFormat::Json, false, true));
    }

    #[test]
    fn parses_defaults_for_doctor_command() {
        let cli = Cli::try_parse_from(["vulcan", "doctor"]).expect("cli should parse");

        assert_eq!(cli.vault, PathBuf::from("."));
        assert_eq!(cli.output, OutputFormat::Human);
        assert_eq!(cli.fields, None);
        assert_eq!(cli.limit, None);
        assert_eq!(cli.offset, 0);
        assert!(!cli.verbose);
        assert_eq!(
            cli.command,
            Command::Doctor {
                fix: false,
                dry_run: false,
                fail_on_issues: false,
            }
        );
    }

    #[test]
    fn parses_dataview_inline_command() {
        let cli = Cli::try_parse_from(["vulcan", "dataview", "inline", "Dashboard"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Dataview {
                command: DataviewCommand::Inline {
                    file: "Dashboard".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_dataview_query_command() {
        let cli = Cli::try_parse_from(["vulcan", "dataview", "query", "TABLE status FROM #tag"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Dataview {
                command: DataviewCommand::Query {
                    dql: "TABLE status FROM #tag".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_dataview_query_js_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "dataview",
            "query-js",
            "dv.current()",
            "--file",
            "Dashboard",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Dataview {
                command: DataviewCommand::QueryJs {
                    js: "dv.current()".to_string(),
                    file: Some("Dashboard".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_dataview_eval_command() {
        let cli = Cli::try_parse_from(["vulcan", "dataview", "eval", "Dashboard", "--block", "1"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Dataview {
                command: DataviewCommand::Eval {
                    file: "Dashboard".to_string(),
                    block: Some(1),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_query_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "query", "not done"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Query {
                    query: "not done".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_add_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "add",
            "Buy groceries tomorrow @home",
            "--status",
            "open",
            "--priority",
            "high",
            "--due",
            "2026-04-10",
            "--scheduled",
            "2026-04-09",
            "--context",
            "@errands",
            "--project",
            "Website",
            "--tag",
            "shopping",
            "--template",
            "task",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Add {
                    text: "Buy groceries tomorrow @home".to_string(),
                    no_nlp: false,
                    status: Some("open".to_string()),
                    priority: Some("high".to_string()),
                    due: Some("2026-04-10".to_string()),
                    scheduled: Some("2026-04-09".to_string()),
                    contexts: vec!["@errands".to_string()],
                    projects: vec!["Website".to_string()],
                    tags: vec!["shopping".to_string()],
                    template: Some("task".to_string()),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_show_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "show", "Write Docs"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Show {
                    task: "Write Docs".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_edit_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "edit", "Write Docs", "--no-commit"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Edit {
                    task: "Write Docs".to_string(),
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_set_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "set",
            "Write Docs",
            "due",
            "2026-04-12",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Set {
                    task: "Write Docs".to_string(),
                    property: "due".to_string(),
                    value: "2026-04-12".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_complete_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "complete",
            "Write Docs",
            "--date",
            "2026-04-10",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Complete {
                    task: "Write Docs".to_string(),
                    date: Some("2026-04-10".to_string()),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_archive_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "archive",
            "Prep Outline",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Archive {
                    task: "Prep Outline".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_convert_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "convert",
            "Notes/Idea.md",
            "--line",
            "12",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Convert {
                    file: "Notes/Idea.md".to_string(),
                    line: Some(12),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_create_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "create",
            "Call Alice tomorrow @desk",
            "--in",
            "Inbox",
            "--due",
            "2026-04-12",
            "--priority",
            "high",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Create {
                    text: "Call Alice tomorrow @desk".to_string(),
                    note: Some("Inbox".to_string()),
                    due: Some("2026-04-12".to_string()),
                    priority: Some("high".to_string()),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_reschedule_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "reschedule",
            "Inbox:3",
            "--due",
            "2026-04-12",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Reschedule {
                    task: "Inbox:3".to_string(),
                    due: "2026-04-12".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_eval_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "eval", "Dashboard", "--block", "1"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Eval {
                    file: "Dashboard".to_string(),
                    block: Some(1),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_list_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "list",
            "--filter",
            "completed",
            "--source",
            "file",
            "--status",
            "in-progress",
            "--priority",
            "high",
            "--due-before",
            "2026-04-11",
            "--due-after",
            "2026-04-01",
            "--project",
            "[[Projects/Website]]",
            "--context",
            "@desk",
            "--group-by",
            "source",
            "--sort-by",
            "due",
            "--include-archived",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::List {
                    filter: Some("completed".to_string()),
                    source: Some(TasksListSourceArg::Tasknotes),
                    status: Some("in-progress".to_string()),
                    priority: Some("high".to_string()),
                    due_before: Some("2026-04-11".to_string()),
                    due_after: Some("2026-04-01".to_string()),
                    project: Some("[[Projects/Website]]".to_string()),
                    context: Some("@desk".to_string()),
                    group_by: Some("source".to_string()),
                    sort_by: Some("due".to_string()),
                    include_archived: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_next_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "next", "5", "--from", "2026-03-29"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Next {
                    count: 5,
                    from: Some("2026-03-29".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_blocked_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "blocked"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Blocked,
            }
        );
    }

    #[test]
    fn parses_tasks_graph_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "graph"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Graph,
            }
        );
    }

    #[test]
    fn parses_tasks_track_start_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "track",
            "start",
            "Write Docs",
            "--description",
            "Deep work",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Track {
                    command: TasksTrackCommand::Start {
                        task: "Write Docs".to_string(),
                        description: Some("Deep work".to_string()),
                        dry_run: true,
                        no_commit: true,
                    },
                },
            }
        );
    }

    #[test]
    fn parses_tasks_track_summary_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "track", "summary", "--period", "month"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Track {
                    command: TasksTrackCommand::Summary {
                        period: TasksTrackSummaryPeriodArg::Month,
                    },
                },
            }
        );
    }

    #[test]
    fn parses_tasks_pomodoro_start_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "pomodoro",
            "start",
            "Write Docs",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Pomodoro {
                    command: TasksPomodoroCommand::Start {
                        task: "Write Docs".to_string(),
                        dry_run: true,
                        no_commit: true,
                    },
                },
            }
        );
    }

    #[test]
    fn parses_tasks_pomodoro_status_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "pomodoro", "status"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Pomodoro {
                    command: TasksPomodoroCommand::Status,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_due_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "due", "--within", "30d"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Due {
                    within: "30d".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_reminders_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "reminders", "--upcoming", "12h"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Reminders {
                    upcoming: "12h".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_view_show_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "view", "show", "Tasks"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::View {
                    command: TasksViewCommand::Show {
                        name: "Tasks".to_string(),
                        export: ExportArgs::default(),
                    },
                },
            }
        );
    }

    #[test]
    fn parses_tasks_view_list_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "tasks", "view", "list"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::View {
                    command: TasksViewCommand::List,
                },
            }
        );
    }

    #[test]
    fn parses_kanban_list_command() {
        let cli = Cli::try_parse_from(["vulcan", "kanban", "list"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::List,
            }
        );
    }

    #[test]
    fn parses_kanban_show_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "kanban",
            "show",
            "Board",
            "--verbose",
            "--include-archive",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::Show {
                    board: "Board".to_string(),
                    verbose: true,
                    include_archive: true,
                },
            }
        );
    }

    #[test]
    fn parses_kanban_cards_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "kanban",
            "cards",
            "Board",
            "--column",
            "Todo",
            "--status",
            "IN_PROGRESS",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::Cards {
                    board: "Board".to_string(),
                    column: Some("Todo".to_string()),
                    status: Some("IN_PROGRESS".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_kanban_archive_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "kanban",
            "archive",
            "Board",
            "build-release",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::Archive {
                    board: "Board".to_string(),
                    card: "build-release".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_kanban_move_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "kanban",
            "move",
            "Board",
            "build-release",
            "Done",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::Move {
                    board: "Board".to_string(),
                    card: "build-release".to_string(),
                    target_column: "Done".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_kanban_add_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "kanban",
            "add",
            "Board",
            "Todo",
            "Build release",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::Add {
                    board: "Board".to_string(),
                    column: "Todo".to_string(),
                    text: "Build release".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_daily_append_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "daily",
            "append",
            "Called Alice",
            "--heading",
            "## Log",
            "--date",
            "2026-04-03",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Daily {
                command: DailyCommand::Append {
                    text: "Called Alice".to_string(),
                    heading: Some("## Log".to_string()),
                    date: Some("2026-04-03".to_string()),
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_today_command() {
        let cli = Cli::try_parse_from(["vulcan", "today", "--no-edit", "--no-commit"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Today {
                no_edit: true,
                no_commit: true,
            }
        );
    }

    #[test]
    fn parses_note_get_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "note",
            "get",
            "Dashboard",
            "--heading",
            "Tasks",
            "--match",
            "TODO",
            "--context",
            "1",
            "--raw",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Get {
                    note: "Dashboard".to_string(),
                    mode: NoteGetMode::Markdown,
                    section_id: None,
                    heading: Some("Tasks".to_string()),
                    block_ref: None,
                    lines: None,
                    match_pattern: Some("TODO".to_string()),
                    context: 1,
                    no_frontmatter: false,
                    raw: true,
                },
            }
        );
    }

    #[test]
    fn parses_note_get_html_mode_command() {
        let cli = Cli::try_parse_from(["vulcan", "note", "get", "Dashboard", "--mode", "html"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Get {
                    note: "Dashboard".to_string(),
                    mode: NoteGetMode::Html,
                    section_id: None,
                    heading: None,
                    block_ref: None,
                    lines: None,
                    match_pattern: None,
                    context: 0,
                    no_frontmatter: false,
                    raw: false,
                },
            }
        );
    }

    #[test]
    fn parses_note_outline_command() {
        let cli = Cli::try_parse_from(["vulcan", "note", "outline", "Dashboard"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Outline {
                    note: "Dashboard".to_string(),
                    section_id: None,
                    depth: None,
                },
            }
        );
    }

    #[test]
    fn parses_note_checkbox_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "note",
            "checkbox",
            "Dashboard",
            "--section",
            "dashboard/tasks@9",
            "--index",
            "2",
            "--state",
            "unchecked",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Checkbox {
                    note: "Dashboard".to_string(),
                    section_id: Some("dashboard/tasks@9".to_string()),
                    heading: None,
                    block_ref: None,
                    lines: None,
                    line: None,
                    index: Some(2),
                    state: NoteCheckboxState::Unchecked,
                    check: false,
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_note_append_periodic_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "note",
            "append",
            "- {{VALUE:title|case:slug}}",
            "--periodic",
            "daily",
            "--date",
            "2026-04-03",
            "--prepend",
            "--var",
            "title=Release Planning",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Append {
                    note_or_text: "- {{VALUE:title|case:slug}}".to_string(),
                    text: None,
                    heading: None,
                    prepend: true,
                    append: false,
                    periodic: Some(NoteAppendPeriodicArg::Daily),
                    date: Some("2026-04-03".to_string()),
                    vars: vec!["title=Release Planning".to_string()],
                    check: false,
                    no_commit: false,
                },
            }
        );
    }

    #[test]
    fn parses_note_patch_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "note",
            "patch",
            "Dashboard",
            "--heading",
            "Tasks",
            "--lines",
            "2-4",
            "--find",
            "/TODO \\d+/",
            "--replace",
            "DONE",
            "--all",
            "--check",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Patch {
                    note: "Dashboard".to_string(),
                    section_id: None,
                    heading: Some("Tasks".to_string()),
                    block_ref: None,
                    lines: Some("2-4".to_string()),
                    find: "/TODO \\d+/".to_string(),
                    replace: "DONE".to_string(),
                    all: true,
                    check: true,
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_note_delete_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "note",
            "delete",
            "Dashboard",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Delete {
                    note: "Dashboard".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_note_rename_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "note",
            "rename",
            "Dashboard",
            "Archive/Dashboard",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Rename {
                    note: "Dashboard".to_string(),
                    new_name: "Archive/Dashboard".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_note_info_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "note", "info", "Dashboard"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Info {
                    note: "Dashboard".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_note_history_command() {
        let cli = Cli::try_parse_from(["vulcan", "note", "history", "Dashboard", "--limit", "5"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::History {
                    note: "Dashboard".to_string(),
                    limit: 5,
                },
            }
        );
    }

    #[test]
    fn parses_daily_export_ics_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "daily",
            "export-ics",
            "--month",
            "--path",
            "journal.ics",
            "--calendar-name",
            "Journal",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Daily {
                command: DailyCommand::ExportIcs {
                    from: None,
                    to: None,
                    week: false,
                    month: true,
                    path: Some(PathBuf::from("journal.ics")),
                    calendar_name: Some("Journal".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_git_status_command() {
        let cli = Cli::try_parse_from(["vulcan", "git", "status"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Git {
                command: GitCommand::Status,
            }
        );
    }

    #[test]
    fn parses_git_log_command() {
        let cli = Cli::try_parse_from(["vulcan", "git", "log", "--limit", "5"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Git {
                command: GitCommand::Log { limit: 5 },
            }
        );
    }

    #[test]
    fn parses_git_diff_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "git", "diff", "Home.md"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Git {
                command: GitCommand::Diff {
                    path: Some("Home.md".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_git_commit_command() {
        let cli = Cli::try_parse_from(["vulcan", "git", "commit", "-m", "Update notes"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Git {
                command: GitCommand::Commit {
                    message: "Update notes".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_git_blame_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "git", "blame", "Home.md"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Git {
                command: GitCommand::Blame {
                    path: "Home.md".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_web_search_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "web",
            "search",
            "release notes",
            "--backend",
            "ollama",
            "--limit",
            "5",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Web {
                command: WebCommand::Search {
                    query: "release notes".to_string(),
                    backend: Some(SearchBackendArg::Ollama),
                    limit: 5,
                },
            }
        );
    }

    #[test]
    fn parses_web_fetch_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "web",
            "fetch",
            "https://example.com",
            "--mode",
            "raw",
            "--save",
            "page.bin",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Web {
                command: WebCommand::Fetch {
                    url: "https://example.com".to_string(),
                    mode: WebFetchMode::Raw,
                    save: Some(PathBuf::from("page.bin")),
                },
            }
        );
    }

    #[test]
    fn parses_weekly_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "periodic",
            "weekly",
            "2026-04-03",
            "--no-edit",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Periodic {
                command: Some(PeriodicSubcommand::Weekly {
                    args: PeriodicOpenArgs {
                        date: Some("2026-04-03".to_string()),
                        no_edit: true,
                        no_commit: true,
                    },
                }),
                period_type: None,
                date: None,
                no_edit: false,
                no_commit: false,
            }
        );
    }

    #[test]
    fn removed_top_level_aliases_no_longer_parse() {
        for command in ["weekly", "monthly", "batch", "cluster", "related", "notes"] {
            assert!(
                Cli::try_parse_from(["vulcan", command]).is_err(),
                "{command} should not parse as a top-level compatibility alias"
            );
        }
        assert!(Cli::try_parse_from(["vulcan", "saved", "search", "weekly", "dashboard"]).is_err());
        assert!(Cli::try_parse_from(["vulcan", "saved", "notes", "active"]).is_err());
        assert!(Cli::try_parse_from(["vulcan", "saved", "bases", "base", "Dash.base"]).is_err());
    }

    #[test]
    fn parses_periodic_gaps_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "periodic",
            "gaps",
            "--type",
            "daily",
            "--from",
            "2026-04-01",
            "--to",
            "2026-04-07",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Periodic {
                command: Some(PeriodicSubcommand::Gaps {
                    period_type: Some("daily".to_string()),
                    from: Some("2026-04-01".to_string()),
                    to: Some("2026-04-07".to_string()),
                }),
                period_type: None,
                date: None,
                no_edit: false,
                no_commit: false,
            }
        );
    }

    #[test]
    fn parses_config_import_tasks_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "config", "import", "tasks"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Tasks),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        apply: false,
                        target: ConfigTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_show_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "show", "periodic.daily"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Show {
                    section: Some("periodic.daily".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_config_get_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "get", "periodic.daily.template"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Get {
                    key: "periodic.daily.template".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_config_edit_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "edit", "--no-commit"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Edit { no_commit: true },
            }
        );
    }

    #[test]
    fn parses_config_set_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "config",
            "set",
            "periodic.daily.template",
            "Templates/Daily",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Set {
                    key: "periodic.daily.template".to_string(),
                    value: "Templates/Daily".to_string(),
                    target: ConfigTargetArg::Shared,
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tags_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tags",
            "--count",
            "--sort",
            "name",
            "--where",
            "status = active",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tags {
                count: true,
                sort: TagSortArg::Name,
                filters: vec!["status = active".to_string()],
            }
        );
    }

    #[test]
    fn parses_properties_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "properties",
            "--count",
            "--type",
            "--sort",
            "name",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Properties {
                count: true,
                r#type: true,
                sort: PropertySortArg::Name,
            }
        );
    }

    #[test]
    fn parses_config_import_periodic_notes_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "periodic-notes"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::PeriodicNotes),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        apply: false,
                        target: ConfigTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_tasknotes_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "tasknotes"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::TaskNotes),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        apply: false,
                        target: ConfigTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_templater_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "templater"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Templater),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        apply: false,
                        target: ConfigTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_quickadd_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "quickadd"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Quickadd),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        apply: false,
                        target: ConfigTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_templater_template_preview_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "template",
            "preview",
            "daily",
            "--path",
            "Journal/Today",
            "--engine",
            "templater",
            "--var",
            "project=Vulcan",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Template {
                command: Some(TemplateSubcommand::Preview {
                    template: "daily".to_string(),
                    path: Some("Journal/Today".to_string()),
                    render: TemplateRenderArgs {
                        engine: TemplateEngineArg::Templater,
                        vars: vec!["project=Vulcan".to_string()],
                    },
                }),
                name: None,
                list: false,
                path: None,
                render: TemplateRenderArgs {
                    engine: TemplateEngineArg::Auto,
                    vars: Vec::new(),
                },
                no_commit: false,
            }
        );
    }

    #[test]
    fn parses_config_import_kanban_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "kanban"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Kanban),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        apply: false,
                        target: ConfigTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_core_command_with_shared_flags() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "config",
            "import",
            "core",
            "--preview",
            "--target",
            "local",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Core),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: true,
                        apply: false,
                        target: ConfigTargetArg::Local,
                        no_commit: true,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_apply_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "tasks", "--apply"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Tasks),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        apply: true,
                        target: ConfigTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_dataview_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "dataview"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Dataview),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        apply: false,
                        target: ConfigTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_all_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "config",
            "import",
            "--all",
            "--dry-run",
            "--target",
            "local",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: None,
                    all: true,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: true,
                        apply: false,
                        target: ConfigTargetArg::Local,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_init_import_flags() {
        let cli = Cli::try_parse_from(["vulcan", "init", "--import"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Init(InitArgs {
                import: true,
                no_import: false,
                agent_files: false,
                example_tool: false,
            })
        );
    }

    #[test]
    fn parses_init_agent_files_flag() {
        let cli =
            Cli::try_parse_from(["vulcan", "init", "--agent-files"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Init(InitArgs {
                import: false,
                no_import: false,
                agent_files: true,
                example_tool: false,
            })
        );
    }

    #[test]
    fn parses_init_agent_files_with_example_tool_flag() {
        let cli = Cli::try_parse_from(["vulcan", "init", "--agent-files", "--example-tool"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Init(InitArgs {
                import: false,
                no_import: false,
                agent_files: true,
                example_tool: true,
            })
        );
    }

    #[test]
    fn parses_agent_install_overwrite_flag() {
        let cli = Cli::try_parse_from(["vulcan", "agent", "install", "--overwrite"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Agent {
                command: AgentCommand::Install(AgentInstallArgs {
                    overwrite: true,
                    example_tool: false,
                })
            }
        );
    }

    #[test]
    fn parses_agent_install_example_tool_flag() {
        let cli = Cli::try_parse_from(["vulcan", "agent", "install", "--example-tool"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Agent {
                command: AgentCommand::Install(AgentInstallArgs {
                    overwrite: false,
                    example_tool: true,
                })
            }
        );
    }

    #[test]
    fn parses_agent_print_config_runtime_flag() {
        let cli = Cli::try_parse_from(["vulcan", "agent", "print-config", "--runtime", "codex"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Agent {
                command: AgentCommand::PrintConfig(AgentPrintConfigArgs {
                    runtime: AgentRuntimeArg::Codex,
                })
            }
        );
    }

    #[test]
    fn parses_agent_import_flags() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "agent",
            "import",
            "--apply",
            "--symlink",
            "--overwrite",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Agent {
                command: AgentCommand::Import(AgentImportArgs {
                    apply: true,
                    symlink: true,
                    overwrite: true,
                    no_commit: true,
                })
            }
        );
    }

    #[test]
    fn parses_skill_list_command() {
        let cli = Cli::try_parse_from(["vulcan", "skill", "list"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Skill {
                command: SkillCommand::List,
            }
        );
    }

    #[test]
    fn parses_skill_get_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "skill", "get", "note-operations"]).expect("parse");

        assert_eq!(
            cli.command,
            Command::Skill {
                command: SkillCommand::Get {
                    name: "note-operations".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_skill_run_and_init_commands() {
        let run = Cli::try_parse_from([
            "vulcan",
            "skill",
            "run",
            "daily-review",
            "prepare-day",
            "--input-json",
            "{\"date\":\"2026-05-08\"}",
        ])
        .expect("skill run should parse");
        let init = Cli::try_parse_from([
            "vulcan",
            "skill",
            "init",
            "daily-review",
            "--starter-command",
            "prepare-day",
            "--dry-run",
        ])
        .expect("skill init should parse");

        assert_eq!(
            run.command,
            Command::Skill {
                command: SkillCommand::Run {
                    skill: "daily-review".to_string(),
                    command: "prepare-day".to_string(),
                    input_json: Some("{\"date\":\"2026-05-08\"}".to_string()),
                    input_file: None,
                    input_args: Vec::new(),
                    input_json_args: Vec::new(),
                    input_file_args: Vec::new(),
                    input_json_file_args: Vec::new(),
                },
            }
        );
        let exec = Cli::try_parse_from([
            "vulcan",
            "skill",
            "exec",
            ".agents/skills/daily-review/scripts/prepare-day.js",
            "--input-json",
            "{\"date\":\"2026-05-08\"}",
        ])
        .expect("skill exec should parse");
        assert_eq!(
            exec.command,
            Command::Skill {
                command: SkillCommand::Exec {
                    script: PathBuf::from(".agents/skills/daily-review/scripts/prepare-day.js"),
                    input_json: Some("{\"date\":\"2026-05-08\"}".to_string()),
                    input_file: None,
                    input_args: Vec::new(),
                    input_json_args: Vec::new(),
                    input_file_args: Vec::new(),
                    input_json_file_args: Vec::new(),
                },
            }
        );
        assert_eq!(
            init.command,
            Command::Skill {
                command: SkillCommand::Init {
                    name: "daily-review".to_string(),
                    description: None,
                    starter_command: Some("prepare-day".to_string()),
                    dry_run: true,
                    overwrite: false,
                },
            }
        );
    }

    #[test]
    fn config_import_dry_run_does_not_write_target_file() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let plugin_dir = temp_dir
            .path()
            .join(".obsidian/plugins/obsidian-tasks-plugin");
        fs::create_dir_all(&plugin_dir).expect("tasks plugin dir should be created");
        fs::write(
            plugin_dir.join("data.json"),
            r##"{
              "globalFilter": "#task",
              "globalQuery": "not done",
              "removeGlobalFilter": true,
              "setCreatedDate": false
            }"##,
        )
        .expect("tasks config should be written");

        run_from([
            "vulcan",
            "--vault",
            temp_dir.path().to_str().expect("vault path should be utf8"),
            "config",
            "import",
            "tasks",
            "--dry-run",
        ])
        .expect("config import dry run should succeed");

        assert!(!temp_dir.path().join(".vulcan/config.toml").exists());
    }

    #[test]
    fn config_import_target_local_writes_local_config_file() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        fs::create_dir_all(temp_dir.path().join(".vulcan")).expect(".vulcan dir should exist");
        let obsidian_dir = temp_dir.path().join(".obsidian");
        fs::create_dir_all(&obsidian_dir).expect("obsidian dir should be created");
        fs::write(
            obsidian_dir.join("app.json"),
            r#"{
              "useMarkdownLinks": true,
              "strictLineBreaks": true
            }"#,
        )
        .expect("core app config should be written");

        run_from([
            "vulcan",
            "--vault",
            temp_dir.path().to_str().expect("vault path should be utf8"),
            "config",
            "import",
            "core",
            "--target",
            "local",
        ])
        .expect("core import should succeed");

        let local_config = fs::read_to_string(temp_dir.path().join(".vulcan/config.local.toml"))
            .expect("local config should exist");
        assert!(local_config.contains("[links]"));
        assert!(local_config.contains("style = \"markdown\""));
        assert!(local_config.contains("strict_line_breaks = true"));
        assert!(!temp_dir.path().join(".vulcan/config.toml").exists());
    }

    #[test]
    fn config_import_dry_run_target_local_does_not_write_local_file() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let obsidian_dir = temp_dir.path().join(".obsidian");
        fs::create_dir_all(&obsidian_dir).expect("obsidian dir should be created");
        fs::write(
            obsidian_dir.join("templates.json"),
            r#"{
              "dateFormat": "DD/MM/YYYY",
              "timeFormat": "HH:mm"
            }"#,
        )
        .expect("templates config should be written");

        run_from([
            "vulcan",
            "--vault",
            temp_dir.path().to_str().expect("vault path should be utf8"),
            "config",
            "import",
            "core",
            "--dry-run",
            "--target",
            "local",
        ])
        .expect("core dry run should succeed");

        assert!(!temp_dir.path().join(".vulcan/config.local.toml").exists());
    }

    #[test]
    fn edit_new_auto_commit_creates_git_commit() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::create_dir_all(temp_dir.path().join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            temp_dir.path().join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("config should be written");

        let original_editor = std::env::var_os("EDITOR");
        std::env::set_var("EDITOR", "true");

        let result = run_from([
            "vulcan",
            "--vault",
            temp_dir.path().to_str().expect("temp dir should be utf8"),
            "edit",
            "--new",
            "Notes/Idea.md",
        ]);

        match original_editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }

        result.expect("edit should succeed");
        assert!(temp_dir.path().join("Notes/Idea.md").exists());
        assert_eq!(
            git_head_summary(temp_dir.path()),
            "vulcan edit: Notes/Idea.md"
        );
    }

    fn write_bases_create_fixture(vault_root: &Path, with_template: bool) {
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        if with_template {
            fs::create_dir_all(vault_root.join(".vulcan/templates"))
                .expect("template dir should exist");
            fs::write(
                vault_root.join(".vulcan/templates/Project.md"),
                concat!(
                    "---\n",
                    "owner: Template Owner\n",
                    "tags:\n",
                    "  - base\n",
                    "---\n",
                    "# {{title}}\n\n",
                    "Template body.\n",
                ),
            )
            .expect("template should be written");
        }
        fs::write(
            vault_root.join("release.base"),
            if with_template {
                concat!(
                    "create_template: Project\n",
                    "filters:\n",
                    "  - 'file.folder = \"Projects\"'\n",
                    "views:\n",
                    "  - name: Inbox\n",
                    "    type: table\n",
                    "    filters:\n",
                    "      - 'status = todo'\n",
                    "      - 'priority = 2'\n",
                )
            } else {
                concat!(
                    "filters:\n",
                    "  - 'file.folder = \"Projects\"'\n",
                    "views:\n",
                    "  - name: Inbox\n",
                    "    type: table\n",
                    "    filters:\n",
                    "      - 'status = todo'\n",
                )
            },
        )
        .expect("base file should be written");
    }

    #[test]
    fn bases_create_dry_run_does_not_write_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        write_bases_create_fixture(temp_dir.path(), false);
        let paths = VaultPaths::new(temp_dir.path());

        let report = create_note_from_bases_view(&paths, "release.base", 0, None, true)
            .expect("bases create should succeed");

        assert_eq!(report.path, "Projects/Untitled.md");
        assert!(!temp_dir.path().join("Projects/Untitled.md").exists());
    }

    #[test]
    fn bases_create_writes_template_with_derived_frontmatter() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        write_bases_create_fixture(temp_dir.path(), true);
        let paths = VaultPaths::new(temp_dir.path());

        let report =
            create_note_from_bases_view(&paths, "release.base", 0, Some("Launch Plan"), false)
                .expect("bases create should succeed");

        assert_eq!(report.path, "Projects/Launch Plan.md");
        let source = fs::read_to_string(temp_dir.path().join(&report.path))
            .expect("created note should be readable");
        let (frontmatter, body) =
            parse_frontmatter_document(&source, false).expect("created note should parse");
        let frontmatter = YamlValue::Mapping(frontmatter.expect("frontmatter should exist"));

        assert_eq!(frontmatter["status"], YamlValue::String("todo".to_string()));
        assert_eq!(frontmatter["priority"], YamlValue::Number(2_i64.into()));
        assert_eq!(
            frontmatter["owner"],
            YamlValue::String("Template Owner".to_string())
        );
        assert_eq!(
            frontmatter["tags"],
            serde_yaml::from_str::<YamlValue>("- base\n").expect("tag yaml should parse")
        );
        assert!(body.contains("# Launch Plan"));
        assert!(body.contains("Template body."));
    }

    #[test]
    fn bases_create_auto_commit_creates_git_commit() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        write_bases_create_fixture(temp_dir.path(), false);
        fs::write(
            temp_dir.path().join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("config should be written");

        run_from([
            "vulcan",
            "--vault",
            temp_dir.path().to_str().expect("temp dir should be utf8"),
            "bases",
            "create",
            "release.base",
            "--title",
            "Launch Plan",
        ])
        .expect("bases create should succeed");

        assert!(temp_dir.path().join("Projects/Launch Plan.md").exists());
        assert_eq!(
            git_head_summary(temp_dir.path()),
            "vulcan bases-create: Projects/Launch Plan.md"
        );
    }

    #[test]
    fn diff_command_uses_git_head_for_modified_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        fs::write(temp_dir.path().join("Home.md"), "# Home\n").expect("note should be written");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir()).expect(".vulcan dir should exist");
        vulcan_core::scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");
        init_git_repo(temp_dir.path());
        run_git(temp_dir.path(), &["add", "Home.md"]);
        run_git(temp_dir.path(), &["commit", "-m", "Initial"]);

        fs::write(temp_dir.path().join("Home.md"), "# Home\nUpdated\n")
            .expect("note should be updated");

        let report =
            run_diff_command(&paths, Some("Home"), None, false).expect("diff should succeed");

        assert_eq!(report.path, "Home.md");
        assert_eq!(report.source, "git_head");
        assert_eq!(report.status, "changed");
        assert!(report.changed);
        assert!(report
            .diff
            .as_deref()
            .is_some_and(|diff| diff.contains("+Updated")));
    }

    #[test]
    fn append_under_heading_inserts_before_next_peer_heading() {
        let rendered = append_under_heading(
            "# Notes\n\n## Inbox\n\n- earlier\n\n## Later\n\ncontent\n",
            "## Inbox",
            "- new item",
        );

        assert!(rendered.contains("## Inbox\n\n- earlier\n\n- new item\n\n## Later"));
    }

    #[test]
    fn render_template_contents_supports_obsidian_format_strings() {
        let timestamp = test_template_timestamp(2026, 3, 26, 17, 4, 5);
        let variables = template_variables_for_path("Journal/Today.md", timestamp);
        let config = TemplatesConfig {
            date_format: "dddd, MMMM Do YYYY".to_string(),
            time_format: "hh:mm A".to_string(),
            ..TemplatesConfig::default()
        };

        let rendered = render_template_contents(
            "Date {{date}}\nTime {{time}}\nAlt {{time:YYYY-MM-DD}}\nWeekday {{date:dd}} {{date:ddd}} {{date:dddd}}\nStamp {{datetime}}\n",
            &variables,
            &config,
        );

        assert!(rendered.contains("Date Thursday, March 26th 2026"));
        assert!(rendered.contains("Time 05:04 PM"));
        assert!(rendered.contains("Alt 2026-03-26"));
        assert!(rendered.contains("Weekday Th Thu Thursday"));
        assert!(rendered.contains(&format!("Stamp {}", variables.datetime)));
    }

    #[test]
    fn render_template_contents_preserves_datetime_and_uuid_variables() {
        let timestamp = test_template_timestamp(2026, 3, 26, 17, 4, 5);
        let mut variables = template_variables_for_path("Journal/Today.md", timestamp);
        variables.uuid = "00000000-0000-0000-0000-000000000000".to_string();
        let config = TemplatesConfig::default();

        let rendered = render_template_contents(
            "{{datetime}}\n{{uuid}}\n{{date}}\n{{time}}\n",
            &variables,
            &config,
        );

        assert!(rendered.contains("2026-03-26T17:04:05Z"));
        assert!(rendered.contains("00000000-0000-0000-0000-000000000000"));
        assert!(rendered.contains("2026-03-26"));
        assert!(rendered.contains("17:04"));
    }

    #[test]
    fn template_command_lists_obsidian_templates_with_sources_and_conflicts() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::create_dir_all(temp_dir.path().join(".obsidian")).expect("obsidian dir");
        fs::create_dir_all(temp_dir.path().join("Shared Templates")).expect("shared templates dir");
        fs::write(
            temp_dir.path().join(".obsidian/templates.json"),
            r#"{"folder":"Shared Templates"}"#,
        )
        .expect("templates config should be written");
        fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "# Vulcan\n",
        )
        .expect("vulcan template should be written");
        fs::write(
            temp_dir.path().join("Shared Templates").join("daily.md"),
            "# Obsidian\n",
        )
        .expect("obsidian daily template should be written");
        fs::write(
            temp_dir.path().join("Shared Templates").join("meeting.md"),
            "# Meeting\n",
        )
        .expect("obsidian meeting template should be written");

        let result = run_template_command(
            &paths,
            None,
            true,
            None,
            TemplateEngineArg::Auto,
            &[],
            false,
            false,
            false,
        )
        .expect("template list should succeed");
        let TemplateCommandResult::List(report) = result else {
            panic!("template command should list templates");
        };

        assert_eq!(
            report.templates,
            vec![
                TemplateSummary {
                    name: "daily.md".to_string(),
                    source: "vulcan".to_string(),
                    path: ".vulcan/templates/daily.md".to_string(),
                },
                TemplateSummary {
                    name: "meeting.md".to_string(),
                    source: "obsidian".to_string(),
                    path: "Shared Templates/meeting.md".to_string(),
                },
            ]
        );
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("daily.md"));
        assert!(report.warnings[0].contains(".vulcan/templates/daily.md"));
    }

    #[test]
    fn template_command_lists_templater_templates_with_sources() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::create_dir_all(temp_dir.path().join(".obsidian/plugins/templater-obsidian"))
            .expect("templater dir");
        fs::create_dir_all(temp_dir.path().join("Templater")).expect("templater templates dir");
        fs::write(
            temp_dir
                .path()
                .join(".obsidian/plugins/templater-obsidian/data.json"),
            r#"{"templates_folder":"Templater"}"#,
        )
        .expect("templater config should be written");
        fs::write(
            temp_dir.path().join("Templater").join("daily.md"),
            "# Templater\n",
        )
        .expect("templater template should be written");

        let result = run_template_command(
            &paths,
            None,
            true,
            None,
            TemplateEngineArg::Auto,
            &[],
            false,
            false,
            false,
        )
        .expect("template list should succeed");
        let TemplateCommandResult::List(report) = result else {
            panic!("template command should list templates");
        };

        assert_eq!(
            report.templates,
            vec![TemplateSummary {
                name: "daily.md".to_string(),
                source: "templater".to_string(),
                path: "Templater/daily.md".to_string(),
            }]
        );
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn template_command_prefers_vulcan_template_over_obsidian_conflict() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::create_dir_all(temp_dir.path().join(".obsidian")).expect("obsidian dir");
        fs::create_dir_all(temp_dir.path().join("Shared Templates")).expect("shared templates dir");
        fs::write(
            temp_dir.path().join(".obsidian/templates.json"),
            r#"{"folder":"Shared Templates"}"#,
        )
        .expect("templates config should be written");
        fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "# Vulcan {{title}}\n",
        )
        .expect("vulcan template should be written");
        fs::write(
            temp_dir.path().join("Shared Templates").join("daily.md"),
            "# Obsidian {{title}}\n",
        )
        .expect("obsidian template should be written");

        let result = run_template_command(
            &paths,
            Some("daily"),
            false,
            Some("Journal/Today"),
            TemplateEngineArg::Auto,
            &[],
            false,
            false,
            false,
        )
        .expect("template command should succeed");

        let TemplateCommandResult::Create(report) = result else {
            panic!("template command should create a note");
        };
        assert_eq!(report.template, "daily.md");
        assert_eq!(report.template_source, "vulcan");
        assert_eq!(report.warnings.len(), 1);

        let contents = fs::read_to_string(temp_dir.path().join("Journal/Today.md"))
            .expect("created note should be readable");
        assert!(contents.contains("# Vulcan Today"));
        assert!(!contents.contains("# Obsidian Today"));
    }

    #[test]
    fn prepare_template_insertion_merges_frontmatter_without_overwriting_existing_values() {
        let timestamp = test_template_timestamp(2026, 3, 26, 17, 4, 5);
        let variables = template_variables_for_path("Projects/Alpha.md", timestamp);
        let rendered_template = render_template_contents(
            "---\nstatus: backlog\ncreated: \"{{date}}\"\ntags:\n- team\n- release\n---\n\n## Template Section\n",
            &variables,
            &TemplatesConfig::default(),
        );

        let prepared = prepare_template_insertion(
            "---\nstatus: done\ntags:\n- team\n- shipped\nowner: Alice\n---\n# Existing\n",
            &rendered_template,
        )
        .expect("template insertion should be prepared");

        let merged_frontmatter = prepared
            .merged_frontmatter
            .expect("merged frontmatter should be present");
        let merged = YamlValue::Mapping(merged_frontmatter);
        assert_eq!(merged["status"], YamlValue::String("done".to_string()));
        assert_eq!(merged["owner"], YamlValue::String("Alice".to_string()));
        assert_eq!(
            merged["created"],
            YamlValue::String("2026-03-26".to_string())
        );
        assert_eq!(
            merged["tags"],
            serde_yaml::from_str::<YamlValue>("- team\n- shipped\n- release\n")
                .expect("tags should parse")
        );
        assert_eq!(prepared.target_body, "# Existing\n");
        assert_eq!(prepared.template_body, "\n## Template Section\n");
    }

    #[test]
    fn prepare_template_insertion_uses_template_frontmatter_when_target_has_none() {
        let prepared = prepare_template_insertion(
            "# Existing\n",
            "---\nstatus: backlog\n---\nTemplate body\n",
        )
        .expect("template insertion should be prepared");

        let rendered =
            render_note_from_parts(prepared.merged_frontmatter.as_ref(), &prepared.target_body)
                .expect("note should render");
        assert!(rendered.starts_with("---\nstatus: backlog\n---\n# Existing\n"));
        assert_eq!(prepared.template_body, "Template body\n");
    }

    #[test]
    fn template_command_creates_note_and_renders_variables() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "# {{title}}\n\nCreated {{date}} {{time}}\nID {{uuid}}\n",
        )
        .expect("template should be written");

        let result = run_template_command(
            &paths,
            Some("daily"),
            false,
            Some("Journal/Today"),
            TemplateEngineArg::Auto,
            &[],
            false,
            false,
            false,
        )
        .expect("template command should succeed");

        let TemplateCommandResult::Create(report) = result else {
            panic!("template command should create a note");
        };
        assert_eq!(report.path, "Journal/Today.md");
        let contents = fs::read_to_string(temp_dir.path().join("Journal/Today.md"))
            .expect("created note should be readable");
        assert!(contents.contains("# Today"));
        assert!(contents.contains("Created "));
        assert!(contents.contains("ID "));
    }

    #[test]
    fn template_insert_command_prepends_and_merges_frontmatter() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::write(
            temp_dir.path().join("Home.md"),
            "---\nstatus: done\ntags:\n- team\n- shipped\nowner: Alice\n---\n# Existing\n",
        )
        .expect("target note should be written");
        fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "---\nstatus: backlog\ncreated: \"{{date}}\"\ntags:\n- team\n- release\n---\n\n## Inserted\n",
        )
        .expect("template should be written");
        vulcan_core::scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");

        let report = run_template_insert_command(
            &paths,
            "daily",
            Some("Home"),
            TemplateInsertMode::Prepend,
            TemplateEngineArg::Auto,
            &[],
            false,
            false,
            false,
        )
        .expect("template insert should succeed");

        assert_eq!(report.note, "Home.md");
        assert_eq!(report.mode, "prepend");
        let updated =
            fs::read_to_string(temp_dir.path().join("Home.md")).expect("updated note should exist");
        let (frontmatter, body) =
            parse_frontmatter_document(&updated, false).expect("updated note should parse");
        let frontmatter = YamlValue::Mapping(frontmatter.expect("frontmatter should exist"));
        assert_eq!(frontmatter["status"], YamlValue::String("done".to_string()));
        assert_eq!(frontmatter["owner"], YamlValue::String("Alice".to_string()));
        assert_eq!(
            frontmatter["created"],
            YamlValue::String(TemplateTimestamp::current().default_date_string())
        );
        assert_eq!(
            frontmatter["tags"],
            serde_yaml::from_str::<YamlValue>("- team\n- shipped\n- release\n")
                .expect("tags should parse"),
        );
        assert_eq!(body, "\n## Inserted\n\n# Existing\n");
    }

    #[test]
    fn template_insert_command_appends_and_auto_commits() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::write(temp_dir.path().join("Home.md"), "# Existing\n")
            .expect("target note should be written");
        fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "## Inserted\n",
        )
        .expect("template should be written");
        fs::write(paths.config_file(), "[git]\nauto_commit = true\n")
            .expect("config should be written");
        vulcan_core::scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");
        init_git_repo(temp_dir.path());
        run_git(temp_dir.path(), &["add", "Home.md", ".vulcan/config.toml"]);
        run_git(temp_dir.path(), &["commit", "-m", "Initial"]);

        let report = run_template_insert_command(
            &paths,
            "daily",
            Some("Home"),
            TemplateInsertMode::Append,
            TemplateEngineArg::Auto,
            &[],
            false,
            false,
            false,
        )
        .expect("template insert should succeed");

        assert_eq!(report.note, "Home.md");
        assert_eq!(report.mode, "append");
        assert_eq!(
            fs::read_to_string(temp_dir.path().join("Home.md")).expect("updated note should exist"),
            "# Existing\n\n## Inserted\n",
        );
        assert_eq!(
            git_head_summary(temp_dir.path()),
            "vulcan template insert: Home.md"
        );
    }

    fn test_template_timestamp(
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        minute: i64,
        second: i64,
    ) -> TemplateTimestamp {
        let timestamp = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z");
        TemplateTimestamp::from_millis(
            parse_date_like_string(&timestamp).expect("fixed template timestamp should parse"),
        )
    }

    #[test]
    fn build_obsidian_uri_uses_vault_name_and_percent_encoding() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("My Vault");
        fs::create_dir_all(&vault_root).expect("vault root should be created");
        let paths = VaultPaths::new(&vault_root);

        let uri = build_obsidian_uri(&paths, "Notes/Hello World.md");

        assert_eq!(
            uri,
            "obsidian://open?vault=My%20Vault&file=Notes%2FHello%20World.md"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn parses_links_and_backlinks_commands() {
        let rebuild =
            Cli::try_parse_from(["vulcan", "rebuild", "--dry-run"]).expect("cli should parse");
        let repair = Cli::try_parse_from(["vulcan", "repair", "fts", "--dry-run"])
            .expect("cli should parse");
        let watch = Cli::try_parse_from(["vulcan", "watch", "--debounce-ms", "125"])
            .expect("cli should parse");
        let serve = Cli::try_parse_from([
            "vulcan",
            "serve",
            "--bind",
            "127.0.0.1:4000",
            "--no-watch",
            "--debounce-ms",
            "100",
            "--auth-token",
            "secret",
        ])
        .expect("cli should parse");
        let doctor = Cli::try_parse_from(["vulcan", "doctor", "--fix", "--dry-run"])
            .expect("cli should parse");
        let doctor_fail = Cli::try_parse_from(["vulcan", "doctor", "--fail-on-issues"])
            .expect("cli should parse");
        let graph_path = Cli::try_parse_from(["vulcan", "graph", "path", "Home", "Bob"])
            .expect("cli should parse");
        let graph_moc = Cli::try_parse_from(["vulcan", "graph", "moc"]).expect("cli should parse");
        let graph_trends = Cli::try_parse_from(["vulcan", "graph", "trends", "--limit", "7"])
            .expect("cli should parse");
        let cache_verify = Cli::try_parse_from(["vulcan", "cache", "verify", "--fail-on-errors"])
            .expect("cli should parse");
        let cache_vacuum = Cli::try_parse_from(["vulcan", "cache", "vacuum", "--dry-run"])
            .expect("cli should parse");
        let export_search_index =
            Cli::try_parse_from(["vulcan", "export", "search-index", "--pretty"])
                .expect("cli should parse");
        let export_profiles =
            Cli::try_parse_from(["vulcan", "export", "profile", "list"]).expect("cli should parse");
        let export_profile_run =
            Cli::try_parse_from(["vulcan", "export", "profile", "run", "team-book"])
                .expect("cli should parse");
        let export_profile_serve = Cli::try_parse_from([
            "vulcan",
            "export",
            "profile",
            "serve",
            "public-bundle",
            "--port",
            "4174",
        ])
        .expect("cli should parse");
        let export_profile_show =
            Cli::try_parse_from(["vulcan", "export", "profile", "show", "team-book"])
                .expect("cli should parse");
        let export_profile_create = Cli::try_parse_from([
            "vulcan",
            "export",
            "profile",
            "create",
            "team-book",
            "--format",
            "epub",
            "from notes",
            "-o",
            "exports/team.epub",
            "--title",
            "Team Notes",
            "--backlinks",
        ])
        .expect("cli should parse");
        let export_profile_set = Cli::try_parse_from([
            "vulcan",
            "export",
            "profile",
            "set",
            "team-book",
            "--backlinks",
            "--frontmatter",
        ])
        .expect("cli should parse");
        let export_profile_rule_add = Cli::try_parse_from([
            "vulcan",
            "export",
            "profile",
            "rule",
            "add",
            "team-book",
            "from notes where file.path starts_with \"People/\"",
            "--exclude-callout",
            "secret gm",
        ])
        .expect("cli should parse");
        let export_profile_rule_move = Cli::try_parse_from([
            "vulcan",
            "export",
            "profile",
            "rule",
            "move",
            "team-book",
            "2",
            "--before",
            "1",
        ])
        .expect("cli should parse");
        let export_profile_delete = Cli::try_parse_from([
            "vulcan",
            "export",
            "profile",
            "delete",
            "team-book",
            "--dry-run",
        ])
        .expect("cli should parse");
        let export_epub = Cli::try_parse_from([
            "vulcan",
            "export",
            "epub",
            "from notes",
            "-o",
            "exports/book.epub",
            "--title",
            "Team Notes",
            "--author",
            "Vulcan",
            "--backlinks",
            "--exclude-callout",
            "secret gm",
        ])
        .expect("cli should parse");
        let links = Cli::try_parse_from(["vulcan", "links", "Home"]).expect("cli should parse");
        let links_picker = Cli::try_parse_from(["vulcan", "links"]).expect("cli should parse");
        let backlinks = Cli::try_parse_from(["vulcan", "backlinks", "Projects/Alpha"])
            .expect("cli should parse");
        let related_picker =
            Cli::try_parse_from(["vulcan", "vectors", "related"]).expect("cli should parse");
        let search = Cli::try_parse_from([
            "vulcan",
            "search",
            "dashboard",
            "--where",
            "reviewed = true",
            "--tag",
            "index",
            "--path-prefix",
            "People/",
            "--has-property",
            "status",
            "--context-size",
            "24",
            "--fuzzy",
            "--explain",
        ])
        .expect("cli should parse");
        let notes = Cli::try_parse_from([
            "vulcan",
            "query",
            "--where",
            "status = done",
            "--where",
            "estimate > 2",
            "--sort",
            "due",
            "--desc",
        ])
        .expect("cli should parse");
        let bases = Cli::try_parse_from(["vulcan", "bases", "eval", "release.base"])
            .expect("cli should parse");
        let bases_create = Cli::try_parse_from([
            "vulcan",
            "bases",
            "create",
            "release.base",
            "--title",
            "Launch Plan",
            "--dry-run",
        ])
        .expect("cli should parse");
        let bases_tui = Cli::try_parse_from(["vulcan", "bases", "tui", "release.base"])
            .expect("cli should parse");
        let suggest_mentions = Cli::try_parse_from(["vulcan", "suggest", "mentions", "Home"])
            .expect("cli should parse");
        let suggest_duplicates =
            Cli::try_parse_from(["vulcan", "suggest", "duplicates"]).expect("cli should parse");
        let diff =
            Cli::try_parse_from(["vulcan", "note", "diff", "Home"]).expect("cli should parse");
        let inbox = Cli::try_parse_from(["vulcan", "inbox", "idea"]).expect("cli should parse");
        let template = Cli::try_parse_from(["vulcan", "template", "daily", "--path", "Notes/Day"])
            .expect("cli should parse");
        let template_insert =
            Cli::try_parse_from(["vulcan", "template", "insert", "daily", "Home", "--prepend"])
                .expect("cli should parse");
        let open = Cli::try_parse_from(["vulcan", "open", "Home"]).expect("cli should parse");
        let link_mentions = Cli::try_parse_from(["vulcan", "link-mentions", "Home", "--dry-run"])
            .expect("cli should parse");
        let rewrite = Cli::try_parse_from([
            "vulcan",
            "rewrite",
            "--where",
            "reviewed = true",
            "--find",
            "release",
            "--replace",
            "launch",
            "--dry-run",
        ])
        .expect("cli should parse");
        let vectors = Cli::try_parse_from(["vulcan", "vectors", "index", "--dry-run"])
            .expect("cli should parse");
        let vector_repair = Cli::try_parse_from(["vulcan", "vectors", "repair", "--dry-run"])
            .expect("cli should parse");
        let vector_rebuild = Cli::try_parse_from(["vulcan", "vectors", "rebuild", "--dry-run"])
            .expect("cli should parse");
        let vector_queue = Cli::try_parse_from(["vulcan", "vectors", "queue", "status"])
            .expect("cli should parse");
        let vector_related = Cli::try_parse_from(["vulcan", "vectors", "related", "Home"])
            .expect("cli should parse");
        let duplicates =
            Cli::try_parse_from(["vulcan", "vectors", "duplicates"]).expect("cli should parse");
        let cluster = Cli::try_parse_from([
            "vulcan",
            "vectors",
            "cluster",
            "--clusters",
            "3",
            "--dry-run",
        ])
        .expect("cli should parse");
        let related = Cli::try_parse_from(["vulcan", "vectors", "related", "Home"])
            .expect("cli should parse");
        let browse = Cli::try_parse_from(["vulcan", "browse"]).expect("cli should parse");
        let refreshed_browse = Cli::try_parse_from(["vulcan", "--refresh", "background", "browse"])
            .expect("cli should parse");
        let edit = Cli::try_parse_from(["vulcan", "edit", "Home"]).expect("cli should parse");
        let edit_new = Cli::try_parse_from(["vulcan", "edit", "--new", "Notes/Idea"])
            .expect("cli should parse");
        let move_command = Cli::try_parse_from([
            "vulcan",
            "move",
            "Projects/Alpha.md",
            "Archive/Alpha.md",
            "--dry-run",
        ])
        .expect("cli should parse");
        let completions =
            Cli::try_parse_from(["vulcan", "completions", "bash"]).expect("cli should parse");
        let saved_search = Cli::try_parse_from([
            "vulcan",
            "--fields",
            "document_path,rank",
            "--limit",
            "5",
            "saved",
            "create",
            "search",
            "weekly",
            "dashboard",
            "--where",
            "reviewed = true",
            "--raw-query",
            "--fuzzy",
            "--description",
            "weekly dashboard",
            "--export",
            "csv",
            "--export-path",
            "exports/weekly.csv",
        ])
        .expect("cli should parse");
        let saved_run = Cli::try_parse_from([
            "vulcan",
            "saved",
            "run",
            "weekly",
            "--export",
            "jsonl",
            "--export-path",
            "exports/weekly.jsonl",
        ])
        .expect("cli should parse");
        let checkpoint_create = Cli::try_parse_from(["vulcan", "checkpoint", "create", "weekly"])
            .expect("cli should parse");
        let checkpoint_list =
            Cli::try_parse_from(["vulcan", "checkpoint", "list"]).expect("cli should parse");
        let changes = Cli::try_parse_from(["vulcan", "changes", "--checkpoint", "weekly"])
            .expect("cli should parse");
        let today =
            Cli::try_parse_from(["vulcan", "today", "--no-edit"]).expect("cli should parse");
        let automation_list =
            Cli::try_parse_from(["vulcan", "automation", "list"]).expect("cli should parse");
        let automation = Cli::try_parse_from([
            "vulcan",
            "automation",
            "run",
            "--scan",
            "--doctor",
            "--verify-cache",
            "--repair-fts",
            "--all-reports",
            "--fail-on-issues",
        ])
        .expect("cli should parse");

        assert_eq!(rebuild.command, Command::Rebuild { dry_run: true });
        assert_eq!(
            repair.command,
            Command::Repair {
                command: RepairCommand::Fts { dry_run: true }
            }
        );
        assert_eq!(
            watch.command,
            Command::Watch {
                debounce_ms: 125,
                no_commit: false,
            }
        );
        assert_eq!(
            serve.command,
            Command::Serve {
                bind: "127.0.0.1:4000".to_string(),
                no_watch: true,
                debounce_ms: 100,
                auth_token: Some("secret".to_string()),
            }
        );
        assert_eq!(
            doctor.command,
            Command::Doctor {
                fix: true,
                dry_run: true,
                fail_on_issues: false,
            }
        );
        assert_eq!(
            doctor_fail.command,
            Command::Doctor {
                fix: false,
                dry_run: false,
                fail_on_issues: true,
            }
        );
        assert_eq!(
            graph_path.command,
            Command::Graph {
                command: GraphCommand::Path {
                    from: Some("Home".to_string()),
                    to: Some("Bob".to_string()),
                }
            }
        );
        assert_eq!(
            graph_moc.command,
            Command::Graph {
                command: GraphCommand::Moc {
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            graph_trends.command,
            Command::Graph {
                command: GraphCommand::Trends {
                    limit: 7,
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            cache_verify.command,
            Command::Cache {
                command: CacheCommand::Verify {
                    fail_on_errors: true,
                }
            }
        );
        assert_eq!(
            cache_vacuum.command,
            Command::Cache {
                command: CacheCommand::Vacuum { dry_run: true }
            }
        );
        assert_eq!(
            export_search_index.command,
            Command::Export {
                command: ExportCommand::SearchIndex {
                    path: None,
                    pretty: true,
                },
            }
        );
        assert_eq!(
            export_profiles.command,
            Command::Export {
                command: ExportCommand::Profile {
                    command: ExportProfileCommand::List,
                },
            }
        );
        assert_eq!(
            export_profile_run.command,
            Command::Export {
                command: ExportCommand::Profile {
                    command: ExportProfileCommand::Run {
                        name: "team-book".to_string(),
                    },
                },
            }
        );
        assert_eq!(
            export_profile_serve.command,
            Command::Export {
                command: ExportCommand::Profile {
                    command: ExportProfileCommand::Serve {
                        name: "public-bundle".to_string(),
                        port: 4174,
                        debounce_ms: 100,
                    },
                },
            }
        );
        assert_eq!(
            export_profile_show.command,
            Command::Export {
                command: ExportCommand::Profile {
                    command: ExportProfileCommand::Show {
                        name: "team-book".to_string(),
                    },
                },
            }
        );
        assert_eq!(
            export_profile_create.command,
            Command::Export {
                command: ExportCommand::Profile {
                    command: ExportProfileCommand::Create {
                        name: "team-book".to_string(),
                        format: ExportProfileFormatArg::Epub,
                        query: Some("from notes".to_string()),
                        query_json: None,
                        path: PathBuf::from("exports/team.epub"),
                        site_profile: None,
                        title: Some("Team Notes".to_string()),
                        author: None,
                        toc: None,
                        backlinks: true,
                        frontmatter: false,
                        pretty: false,
                        graph_format: None,
                        replace: false,
                        dry_run: false,
                        no_commit: false,
                    },
                },
            }
        );
        assert_eq!(
            export_profile_set.command,
            Command::Export {
                command: ExportCommand::Profile {
                    command: ExportProfileCommand::Set {
                        name: "team-book".to_string(),
                        format: None,
                        query: None,
                        query_json: None,
                        clear_query: false,
                        path: None,
                        clear_path: false,
                        site_profile: None,
                        clear_site_profile: false,
                        title: None,
                        clear_title: false,
                        author: None,
                        clear_author: false,
                        toc: None,
                        clear_toc: false,
                        backlinks: true,
                        no_backlinks: false,
                        frontmatter: true,
                        no_frontmatter: false,
                        pretty: false,
                        no_pretty: false,
                        graph_format: None,
                        clear_graph_format: false,
                        dry_run: false,
                        no_commit: false,
                    },
                },
            }
        );
        assert_eq!(
            export_profile_rule_add.command,
            Command::Export {
                command: ExportCommand::Profile {
                    command: ExportProfileCommand::Rule {
                        command: ExportProfileRuleCommand::Add {
                            profile: "team-book".to_string(),
                            before: None,
                            query: Some(
                                "from notes where file.path starts_with \"People/\"".to_string()
                            ),
                            query_json: None,
                            transforms: Box::new(ExportTransformArgs {
                                exclude_callouts: vec!["secret gm".to_string()],
                                exclude_headings: vec![],
                                exclude_frontmatter_keys: vec![],
                                exclude_inline_fields: vec![],
                                replace_rules: vec![],
                            }),
                            dry_run: false,
                            no_commit: false,
                        },
                    },
                },
            }
        );
        assert_eq!(
            export_profile_rule_move.command,
            Command::Export {
                command: ExportCommand::Profile {
                    command: ExportProfileCommand::Rule {
                        command: ExportProfileRuleCommand::Move {
                            profile: "team-book".to_string(),
                            index: 2,
                            before: Some(1),
                            after: None,
                            last: false,
                            dry_run: false,
                            no_commit: false,
                        },
                    },
                },
            }
        );
        assert_eq!(
            export_profile_delete.command,
            Command::Export {
                command: ExportCommand::Profile {
                    command: ExportProfileCommand::Delete {
                        name: "team-book".to_string(),
                        dry_run: true,
                        no_commit: false,
                    },
                },
            }
        );
        assert_eq!(
            export_epub.command,
            Command::Export {
                command: ExportCommand::Epub {
                    query: ExportQueryArgs {
                        query: Some("from notes".to_string()),
                        query_json: None,
                    },
                    path: PathBuf::from("exports/book.epub"),
                    title: Some("Team Notes".to_string()),
                    author: Some("Vulcan".to_string()),
                    toc: EpubTocStyle::Tree,
                    backlinks: true,
                    frontmatter: false,
                    transforms: ExportTransformArgs {
                        exclude_callouts: vec!["secret gm".to_string()],
                        exclude_headings: vec![],
                        exclude_frontmatter_keys: vec![],
                        exclude_inline_fields: vec![],
                        replace_rules: vec![],
                    },
                },
            }
        );

        assert_eq!(
            links.command,
            Command::Links {
                note: Some("Home".to_string()),
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            links_picker.command,
            Command::Links {
                note: None,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            backlinks.command,
            Command::Backlinks {
                note: Some("Projects/Alpha".to_string()),
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            search.command,
            Command::Search {
                query: Some("dashboard".to_string()),
                regex: None,
                filters: vec!["reviewed = true".to_string()],
                mode: SearchMode::Keyword,
                tag: Some("index".to_string()),
                path_prefix: Some("People/".to_string()),
                has_property: Some("status".to_string()),
                sort: None,
                match_case: false,
                context_size: 24,
                raw_query: false,
                fuzzy: true,
                explain: true,
                exit_code: false,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            notes.command,
            Command::Query {
                dsl: None,
                json: None,
                filters: vec!["status = done".to_string(), "estimate > 2".to_string()],
                sort: Some("due".to_string()),
                desc: true,
                list_fields: false,
                engine: QueryEngineArg::Auto,
                format: QueryFormatArg::Table,
                glob: None,
                explain: false,
                exit_code: false,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            bases.command,
            Command::Bases {
                command: BasesCommand::Eval {
                    file: "release.base".to_string(),
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            bases_create.command,
            Command::Bases {
                command: BasesCommand::Create {
                    file: "release.base".to_string(),
                    title: Some("Launch Plan".to_string()),
                    dry_run: true,
                    no_commit: false,
                },
            }
        );
        assert_eq!(
            bases_tui.command,
            Command::Bases {
                command: BasesCommand::Tui {
                    file: "release.base".to_string(),
                },
            }
        );
        assert_eq!(
            suggest_mentions.command,
            Command::Suggest {
                command: SuggestCommand::Mentions {
                    note: Some("Home".to_string()),
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            suggest_duplicates.command,
            Command::Suggest {
                command: SuggestCommand::Duplicates {
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            diff.command,
            Command::Note {
                command: NoteCommand::Diff {
                    note: "Home".to_string(),
                    since: None,
                },
            }
        );
        assert_eq!(
            inbox.command,
            Command::Inbox {
                text: Some("idea".to_string()),
                file: None,
                no_commit: false,
            }
        );
        assert_eq!(
            template.command,
            Command::Template {
                command: None,
                name: Some("daily".to_string()),
                list: false,
                path: Some("Notes/Day".to_string()),
                render: TemplateRenderArgs {
                    engine: TemplateEngineArg::Auto,
                    vars: Vec::new(),
                },
                no_commit: false,
            }
        );
        assert_eq!(
            template_insert.command,
            Command::Template {
                command: Some(TemplateSubcommand::Insert {
                    template: "daily".to_string(),
                    note: Some("Home".to_string()),
                    prepend: true,
                    append: false,
                    render: TemplateRenderArgs {
                        engine: TemplateEngineArg::Auto,
                        vars: Vec::new(),
                    },
                    no_commit: false,
                }),
                name: None,
                list: false,
                path: None,
                render: TemplateRenderArgs {
                    engine: TemplateEngineArg::Auto,
                    vars: Vec::new(),
                },
                no_commit: false,
            }
        );
        assert_eq!(
            open.command,
            Command::Open {
                note: Some("Home".to_string())
            }
        );
        assert_eq!(
            link_mentions.command,
            Command::LinkMentions {
                note: Some("Home".to_string()),
                dry_run: true,
                no_commit: false,
            }
        );
        assert_eq!(
            rewrite.command,
            Command::Rewrite {
                filters: vec!["reviewed = true".to_string()],
                stdin: false,
                find: "release".to_string(),
                replace: "launch".to_string(),
                dry_run: true,
                no_commit: false,
            }
        );
        assert_eq!(
            vectors.command,
            Command::Vectors {
                command: VectorsCommand::Index { dry_run: true },
            }
        );
        assert_eq!(
            vector_repair.command,
            Command::Vectors {
                command: VectorsCommand::Repair { dry_run: true },
            }
        );
        assert_eq!(
            vector_rebuild.command,
            Command::Vectors {
                command: VectorsCommand::Rebuild { dry_run: true },
            }
        );
        assert_eq!(
            vector_queue.command,
            Command::Vectors {
                command: VectorsCommand::Queue {
                    command: VectorQueueCommand::Status,
                },
            }
        );
        assert_eq!(
            vector_related.command,
            Command::Vectors {
                command: VectorsCommand::Related {
                    note: Some("Home".to_string()),
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            duplicates.command,
            Command::Vectors {
                command: VectorsCommand::Duplicates {
                    threshold: 0.95,
                    limit: 50,
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            cluster.command,
            Command::Vectors {
                command: VectorsCommand::Cluster {
                    clusters: 3,
                    dry_run: true,
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            related.command,
            Command::Vectors {
                command: VectorsCommand::Related {
                    note: Some("Home".to_string()),
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(browse.command, Command::Browse { no_commit: false });
        assert_eq!(browse.refresh, None);
        assert_eq!(refreshed_browse.refresh, Some(RefreshMode::Background));
        assert_eq!(
            refreshed_browse.command,
            Command::Browse { no_commit: false }
        );
        assert_eq!(
            related_picker.command,
            Command::Vectors {
                command: VectorsCommand::Related {
                    note: None,
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            edit.command,
            Command::Edit {
                note: Some("Home".to_string()),
                new: false,
                no_commit: false,
            }
        );
        assert_eq!(
            edit_new.command,
            Command::Edit {
                note: Some("Notes/Idea".to_string()),
                new: true,
                no_commit: false,
            }
        );
        assert_eq!(
            move_command.command,
            Command::Move {
                source: "Projects/Alpha.md".to_string(),
                dest: "Archive/Alpha.md".to_string(),
                dry_run: true,
                no_commit: false,
            }
        );
        assert_eq!(
            Cli::try_parse_from(["vulcan", "rename-property", "status", "phase", "--dry-run"])
                .expect("cli should parse")
                .command,
            Command::RenameProperty {
                old: "status".to_string(),
                new: "phase".to_string(),
                dry_run: true,
                no_commit: false,
            }
        );
        assert_eq!(
            Cli::try_parse_from(["vulcan", "merge-tags", "project", "initiative", "--dry-run"])
                .expect("cli should parse")
                .command,
            Command::MergeTags {
                source: "project".to_string(),
                dest: "initiative".to_string(),
                dry_run: true,
                no_commit: false,
            }
        );
        assert_eq!(
            Cli::try_parse_from([
                "vulcan",
                "rename-alias",
                "Home",
                "Start",
                "Landing",
                "--dry-run"
            ])
            .expect("cli should parse")
            .command,
            Command::RenameAlias {
                note: "Home".to_string(),
                old: "Start".to_string(),
                new: "Landing".to_string(),
                dry_run: true,
                no_commit: false,
            }
        );
        assert_eq!(
            Cli::try_parse_from([
                "vulcan",
                "rename-heading",
                "Projects/Alpha",
                "Status",
                "Progress",
                "--dry-run"
            ])
            .expect("cli should parse")
            .command,
            Command::RenameHeading {
                note: "Projects/Alpha".to_string(),
                old: "Status".to_string(),
                new: "Progress".to_string(),
                dry_run: true,
                no_commit: false,
            }
        );
        assert_eq!(
            Cli::try_parse_from([
                "vulcan",
                "rename-block-ref",
                "Projects/Alpha",
                "alpha-status",
                "alpha-progress",
                "--dry-run"
            ])
            .expect("cli should parse")
            .command,
            Command::RenameBlockRef {
                note: "Projects/Alpha".to_string(),
                old: "alpha-status".to_string(),
                new: "alpha-progress".to_string(),
                dry_run: true,
                no_commit: false,
            }
        );
        assert_eq!(
            completions.command,
            Command::Completions {
                shell: clap_complete::Shell::Bash
            }
        );
        assert_eq!(
            saved_search.command,
            Command::Saved {
                command: SavedCommand::Create {
                    command: SavedCreateCommand::Search {
                        name: "weekly".to_string(),
                        query: "dashboard".to_string(),
                        filters: vec!["reviewed = true".to_string()],
                        mode: SearchMode::Keyword,
                        tag: None,
                        path_prefix: None,
                        has_property: None,
                        sort: None,
                        match_case: false,
                        context_size: 18,
                        raw_query: true,
                        fuzzy: true,
                        description: Some("weekly dashboard".to_string()),
                        export: ExportArgs {
                            export: Some(ExportFormat::Csv),
                            export_path: Some(PathBuf::from("exports/weekly.csv")),
                        },
                    },
                },
            }
        );
        assert_eq!(
            saved_run.command,
            Command::Saved {
                command: SavedCommand::Run {
                    name: "weekly".to_string(),
                    export: ExportArgs {
                        export: Some(ExportFormat::Jsonl),
                        export_path: Some(PathBuf::from("exports/weekly.jsonl")),
                    },
                },
            }
        );
        assert_eq!(
            checkpoint_create.command,
            Command::Checkpoint {
                command: CheckpointCommand::Create {
                    name: "weekly".to_string(),
                },
            }
        );
        assert_eq!(
            checkpoint_list.command,
            Command::Checkpoint {
                command: CheckpointCommand::List {
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            changes.command,
            Command::Changes {
                checkpoint: Some("weekly".to_string()),
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            today.command,
            Command::Today {
                no_edit: true,
                no_commit: false,
            }
        );
        assert_eq!(
            automation_list.command,
            Command::Automation {
                command: AutomationCommand::List,
            }
        );
        assert_eq!(
            automation.command,
            Command::Automation {
                command: AutomationCommand::Run {
                    reports: Vec::new(),
                    all_reports: true,
                    scan: true,
                    doctor: true,
                    doctor_fix: false,
                    verify_cache: true,
                    repair_fts: true,
                    fail_on_issues: true,
                }
            }
        );
    }

    #[test]
    fn parses_site_build_and_serve_commands() {
        let build = Cli::try_parse_from([
            "vulcan",
            "site",
            "build",
            "--profile",
            "public",
            "--output-dir",
            "dist",
            "--clean",
            "--watch",
            "--strict",
            "--fail-on-warning",
            "--debounce-ms",
            "75",
        ])
        .expect("cli should parse");
        let serve = Cli::try_parse_from([
            "vulcan",
            "site",
            "serve",
            "--profile",
            "public",
            "--output-dir",
            "dist",
            "--port",
            "43110",
            "--watch",
            "--strict",
            "--fail-on-warning",
            "--debounce-ms",
            "60",
        ])
        .expect("cli should parse");

        assert_eq!(
            build.command,
            Command::Site {
                command: SiteCommand::Build {
                    profile: Some("public".to_string()),
                    output_dir: Some(PathBuf::from("dist")),
                    clean: true,
                    dry_run: false,
                    watch: true,
                    strict: true,
                    fail_on_warning: true,
                    debounce_ms: 75,
                },
            }
        );
        assert_eq!(
            serve.command,
            Command::Site {
                command: SiteCommand::Serve {
                    profile: Some("public".to_string()),
                    output_dir: Some(PathBuf::from("dist")),
                    port: 43110,
                    watch: true,
                    strict: true,
                    fail_on_warning: true,
                    debounce_ms: 60,
                },
            }
        );
    }

    #[test]
    fn parses_global_flags_and_scan_options() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "--vault",
            "/tmp/vault",
            "--output",
            "json",
            "--fields",
            "source_path,raw_text",
            "--limit",
            "10",
            "--offset",
            "2",
            "--verbose",
            "scan",
            "--full",
        ])
        .expect("cli should parse");

        assert_eq!(cli.vault, PathBuf::from("/tmp/vault"));
        assert_eq!(cli.output, OutputFormat::Json);
        assert_eq!(
            cli.fields,
            Some(vec!["source_path".to_string(), "raw_text".to_string()])
        );
        assert_eq!(cli.limit, Some(10));
        assert_eq!(cli.offset, 2);
        assert!(cli.verbose);
        assert_eq!(
            cli.command,
            Command::Scan {
                full: true,
                no_commit: false,
            }
        );
    }

    #[test]
    fn parses_query_format_and_glob_flags() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "query",
            "--format",
            "paths",
            "--glob",
            "Projects/**",
            "from notes where file.name matches \"^2026-\"",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Query {
                dsl: Some("from notes where file.name matches \"^2026-\"".to_string()),
                json: None,
                filters: Vec::new(),
                sort: None,
                desc: false,
                list_fields: false,
                engine: QueryEngineArg::Auto,
                format: QueryFormatArg::Paths,
                glob: Some("Projects/**".to_string()),
                explain: false,
                exit_code: false,
                export: ExportArgs::default(),
            }
        );
    }

    #[test]
    fn legacy_notes_confusion_hint_points_note_where_to_query() {
        let hint = detect_command_confusion(&[
            OsString::from("vulcan"),
            OsString::from("note"),
            OsString::from("--where"),
            OsString::from("status = done"),
        ])
        .expect("note --where should produce a hint");

        assert!(hint.contains("vulcan query --where"));
        assert!(!hint.contains("vulcan notes --where"));
    }

    #[test]
    fn parses_ls_and_refactor_group_commands() {
        let ls = Cli::try_parse_from([
            "vulcan",
            "ls",
            "--where",
            "status = done",
            "--tag",
            "project",
            "--format",
            "detail",
        ])
        .expect("ls should parse");
        let refactor = Cli::try_parse_from([
            "vulcan",
            "refactor",
            "rename-property",
            "status",
            "phase",
            "--dry-run",
        ])
        .expect("refactor should parse");

        assert_eq!(
            ls.command,
            Command::Ls {
                filters: vec!["status = done".to_string()],
                glob: None,
                tag: Some("project".to_string()),
                format: QueryFormatArg::Detail,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            refactor.command,
            Command::Refactor {
                command: RefactorCommand::RenameProperty {
                    old: "status".to_string(),
                    new: "phase".to_string(),
                    dry_run: true,
                    no_commit: false,
                },
            }
        );
    }

    #[test]
    fn parses_help_and_describe_format_commands() {
        let help = Cli::try_parse_from(["vulcan", "help", "note", "get", "--output", "json"])
            .expect("help should parse");
        let describe = Cli::try_parse_from([
            "vulcan",
            "describe",
            "--format",
            "openai-tools",
            "--tool-pack",
            "notes-read,search",
            "--tool-pack",
            "web",
            "--tool-pack-mode",
            "adaptive",
        ])
        .expect("describe should parse");
        let mcp = Cli::try_parse_from([
            "vulcan",
            "--permissions",
            "readonly",
            "mcp",
            "--tool-pack",
            "notes-read,notes-manage",
            "--tool-pack",
            "index",
            "--tool-pack-mode",
            "adaptive",
            "--transport",
            "http",
            "--bind",
            "127.0.0.1:9123",
            "--endpoint",
            "/custom-mcp",
            "--auth-token",
            "secret-token",
        ])
        .expect("mcp should parse");

        assert_eq!(
            help.command,
            Command::Help {
                search: None,
                topic: vec!["note".to_string(), "get".to_string()],
            }
        );
        assert_eq!(
            describe.command,
            Command::Describe {
                format: DescribeFormatArg::OpenaiTools,
                tool_pack: vec![
                    McpToolPackArg::NotesRead,
                    McpToolPackArg::Search,
                    McpToolPackArg::Web,
                ],
                tool_pack_mode: McpToolPackModeArg::Adaptive,
            }
        );
        assert_eq!(
            mcp.command,
            Command::Mcp {
                tool_pack: vec![
                    McpToolPackArg::NotesRead,
                    McpToolPackArg::NotesManage,
                    McpToolPackArg::Index,
                ],
                tool_pack_mode: McpToolPackModeArg::Adaptive,
                transport: McpTransportArg::Http,
                request_timeout: "120s".to_string(),
                bind: "127.0.0.1:9123".to_string(),
                endpoint: "/custom-mcp".to_string(),
                auth_token: Some("secret-token".to_string()),
                public_url: None,
                oauth_issuer: None,
                oauth_audience: Vec::new(),
                oauth_jwks_url: None,
                oauth_allowed_sub: Vec::new(),
                oauth_allowed_email: Vec::new(),
                oauth_local_client_id: None,
                oauth_local_client_secret: None,
                oauth_local_approval_token: None,
                oauth_local_subject: Some("local-user".to_string()),
                oauth_local_email: None,
                oauth_dcr: false,
                oauth_dcr_allowed_redirect_host: Vec::new(),
                oauth_indieauth_authorization_endpoint: None,
                oauth_indieauth_token_endpoint: None,
                oauth_indieauth_client_id: None,
                oauth_indieauth_redirect_uri: None,
                oauth_indieauth_me: None,
                oauth_local_user: Vec::new(),
            }
        );
        assert_eq!(mcp.permissions.as_deref(), Some("readonly"));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn parses_tool_commands() {
        let list = Cli::try_parse_from(["vulcan", "tool", "list"]).expect("tool list should parse");
        let show = Cli::try_parse_from(["vulcan", "tool", "show", "skill_meeting_summarize"])
            .expect("tool show should parse");
        let help = Cli::try_parse_from(["vulcan", "tool", "help", "meeting-summary"])
            .expect("tool help should parse");
        let test = Cli::try_parse_from([
            "vulcan",
            "tool",
            "test",
            "meeting-summary",
            "--example",
            "smoke",
            "--update-expected",
            "--profile",
            "daily-wiki-agent",
        ])
        .expect("tool test should parse");
        let compat = Cli::try_parse_from([
            "vulcan",
            "tool",
            "compat",
            "meeting-summary",
            "--surface",
            "cli,mcp",
        ])
        .expect("tool compat should parse");
        let types = Cli::try_parse_from(["vulcan", "tool", "types", "meeting-summary"])
            .expect("tool types should parse");
        let ci = Cli::try_parse_from([
            "vulcan",
            "tool",
            "ci",
            "--profile",
            "daily-wiki-agent",
            "--surface",
            "cli,mcp",
        ])
        .expect("tool ci should parse");
        let run = Cli::try_parse_from([
            "vulcan",
            "tool",
            "run",
            "skill_meeting_summarize",
            "--input-json",
            "{\"note\":\"Meetings/Weekly.md\"}",
        ])
        .expect("tool run should parse");

        assert_eq!(
            list.command,
            Command::Tool {
                command: ToolCommand::List,
            }
        );
        assert_eq!(
            show.command,
            Command::Tool {
                command: ToolCommand::Show {
                    name: "skill_meeting_summarize".to_string(),
                },
            }
        );
        assert_eq!(
            help.command,
            Command::Tool {
                command: ToolCommand::Help {
                    name: "meeting-summary".to_string(),
                },
            }
        );
        assert_eq!(
            test.command,
            Command::Tool {
                command: ToolCommand::Test {
                    name: Some("meeting-summary".to_string()),
                    all: false,
                    example: Some("smoke".to_string()),
                    update_expected: true,
                    profile: Some("daily-wiki-agent".to_string()),
                },
            }
        );
        assert_eq!(
            compat.command,
            Command::Tool {
                command: ToolCommand::Compat {
                    name: "meeting-summary".to_string(),
                    surface: vec!["cli".to_string(), "mcp".to_string()],
                },
            }
        );
        assert_eq!(
            types.command,
            Command::Tool {
                command: ToolCommand::Types {
                    name: Some("meeting-summary".to_string()),
                    all: false,
                },
            }
        );
        assert_eq!(
            ci.command,
            Command::Tool {
                command: ToolCommand::Ci {
                    profile: Some("daily-wiki-agent".to_string()),
                    surface: vec!["cli".to_string(), "mcp".to_string()],
                },
            }
        );
        assert_eq!(
            run.command,
            Command::Tool {
                command: ToolCommand::Run {
                    name: "skill_meeting_summarize".to_string(),
                    input_json: Some("{\"note\":\"Meetings/Weekly.md\"}".to_string()),
                    input_file: None,
                    args: Vec::new(),
                },
            }
        );
    }

    #[test]
    fn parses_tool_test_all_command() {
        let test_all = Cli::try_parse_from(["vulcan", "tool", "test", "--all"])
            .expect("tool test --all should parse");
        let types_all = Cli::try_parse_from(["vulcan", "tool", "types", "--all"])
            .expect("tool types --all should parse");

        assert_eq!(
            test_all.command,
            Command::Tool {
                command: ToolCommand::Test {
                    name: None,
                    all: true,
                    example: None,
                    update_expected: false,
                    profile: None,
                },
            }
        );
        assert_eq!(
            types_all.command,
            Command::Tool {
                command: ToolCommand::Types {
                    name: None,
                    all: true,
                },
            }
        );
    }

    #[test]
    fn parses_tool_authoring_commands() {
        let init = Cli::try_parse_from([
            "vulcan",
            "tool",
            "init",
            "meeting-summary",
            "--description",
            "Summarize meetings",
            "--command",
            "summarize",
            "--template",
            "reader",
            "--dry-run",
        ])
        .expect("tool init should parse");
        let lint = Cli::try_parse_from([
            "vulcan",
            "tool",
            "lint",
            "meeting-summary",
            "--strict",
            "--fix",
        ])
        .expect("tool lint should parse");

        assert_eq!(
            init.command,
            Command::Tool {
                command: ToolCommand::Init {
                    name: "meeting-summary".to_string(),
                    description: Some("Summarize meetings".to_string()),
                    command: "summarize".to_string(),
                    template: ToolInitTemplateArg::Reader,
                    dry_run: true,
                    overwrite: false,
                },
            }
        );
        assert_eq!(
            lint.command,
            Command::Tool {
                command: ToolCommand::Lint {
                    name: Some("meeting-summary".to_string()),
                    strict: true,
                    fix: true,
                },
            }
        );
    }

    #[test]
    fn json_schema_typescript_supports_common_schema_composition() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["mode", "payload"],
            "properties": {
                "mode": { "const": "append" },
                "payload": {
                    "anyOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": ["string", "null"] } }
                    ]
                },
                "labels": {
                    "type": "object",
                    "additionalProperties": { "type": "number" }
                },
                "status": { "enum": ["open", "done", null] }
            },
            "additionalProperties": false
        });

        let typescript = vulcan_app::tools::json_schema_to_typescript(&schema, 0);

        assert!(typescript.contains("mode: \"append\";"));
        assert!(typescript.contains("payload: string | (string | null)[];"));
        assert!(typescript.contains("labels?: Record<string, number>;"));
        assert!(typescript.contains("status?: \"open\" | \"done\" | null;"));
        assert!(!typescript.contains("[key: string]"));
    }

    #[test]
    fn tool_registry_entry_converts_to_openai_and_mcp_shapes() {
        let entry = ToolRegistryEntry {
            name: "demo_tool".to_string(),
            title: "Demo Tool".to_string(),
            description: "Run a demo operation.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "note": { "type": "string" }
                },
                "required": ["note"],
                "additionalProperties": false,
            }),
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" }
                },
                "required": ["ok"],
                "additionalProperties": false,
            })),
            annotations: McpToolAnnotations {
                read_only_hint: true,
                destructive_hint: false,
                idempotent_hint: true,
                open_world_hint: false,
            },
            tool_packs: vec!["custom".to_string()],
            examples: vec!["demo_tool {\"note\":\"Projects/Alpha.md\"}".to_string()],
        };

        let openai = entry.clone().into_openai_definition();
        assert_eq!(openai.function.name, "demo_tool");
        assert_eq!(openai.function.description, "Run a demo operation.");
        assert_eq!(openai.function.parameters["type"], "object");
        assert_eq!(
            openai.function.examples,
            vec!["demo_tool {\"note\":\"Projects/Alpha.md\"}".to_string()]
        );

        let mcp = entry.to_mcp_definition();
        assert_eq!(mcp.name, "demo_tool");
        assert_eq!(mcp.title, "Demo Tool");
        assert!(mcp.annotations.read_only_hint);
        assert_eq!(mcp.tool_packs, vec!["custom".to_string()]);
        assert_eq!(
            mcp.output_schema.as_ref().expect("output schema")["type"],
            "object"
        );

        let item = entry.to_mcp_list_item();
        assert_eq!(item["name"], "demo_tool");
        assert_eq!(item["title"], "Demo Tool");
        assert_eq!(item["annotations"]["readOnlyHint"], true);
        assert_eq!(item["toolPacks"], serde_json::json!(["custom"]));
        assert!(item.get("examples").is_none());
    }

    #[test]
    fn parses_index_note_and_run_commands() {
        let index = Cli::try_parse_from(["vulcan", "index", "scan", "--full"])
            .expect("index scan should parse");
        let note_links = Cli::try_parse_from(["vulcan", "note", "links", "Dashboard"])
            .expect("note links should parse");
        let run = Cli::try_parse_from(["vulcan", "run", "demo", "--script", "--timeout", "45s"])
            .expect("run should parse");

        assert_eq!(
            index.command,
            Command::Index {
                command: IndexCommand::Scan {
                    full: true,
                    no_commit: false,
                },
            }
        );
        assert_eq!(
            note_links.command,
            Command::Note {
                command: NoteCommand::Links {
                    note: Some("Dashboard".to_string()),
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            run.command,
            Command::Run {
                script: Some("demo".to_string()),
                script_mode: true,
                eval: vec![],
                eval_file: None,
                timeout: Some("45s".to_string()),
                sandbox: None,
                no_startup: false,
            }
        );
    }

    #[test]
    fn resolves_relative_vault_path_against_current_directory() {
        let current_dir = std::env::current_dir().expect("cwd should be available");
        let resolved = resolve_vault_root(&PathBuf::from("tests/fixtures/vaults/basic"))
            .expect("path resolution should succeed");

        assert_eq!(resolved, current_dir.join("tests/fixtures/vaults/basic"));
    }

    #[test]
    fn resolves_tilde_prefixed_vault_path_against_home_directory() {
        let Some(home_dir) = current_home_dir() else {
            return;
        };
        let relative = PathBuf::from("vulcan/tests/fixtures/vaults/basic");
        let resolved = resolve_vault_root(&PathBuf::from("~/vulcan/tests/fixtures/vaults/basic"))
            .expect("path resolution should succeed");

        assert_eq!(resolved, home_dir.join(relative));
    }

    #[cfg(windows)]
    fn completion_test_bash_path() -> Option<PathBuf> {
        let mut candidates = Vec::new();
        for key in [
            "ProgramW6432",
            "PROGRAMFILES",
            "ProgramFiles",
            "ProgramFiles(x86)",
        ] {
            let Some(base) = std::env::var_os(key).map(PathBuf::from) else {
                continue;
            };
            candidates.push(base.join("Git").join("bin").join("bash.exe"));
            candidates.push(base.join("Git").join("usr").join("bin").join("bash.exe"));
        }
        if let Ok(output) = ProcessCommand::new("git").arg("--exec-path").output() {
            if output.status.success() {
                let exec_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
                for ancestor in exec_path.ancestors().take(5) {
                    candidates.push(ancestor.join("bin").join("bash.exe"));
                    candidates.push(ancestor.join("usr").join("bin").join("bash.exe"));
                }
            }
        }
        candidates.into_iter().find(|candidate| candidate.is_file())
    }

    #[cfg(not(windows))]
    fn completion_test_bash_path() -> PathBuf {
        PathBuf::from("bash")
    }

    #[test]
    fn bash_dynamic_completions_forward_global_vault_flag() {
        #[cfg(windows)]
        let Some(bash_path) = completion_test_bash_path() else {
            return;
        };
        #[cfg(not(windows))]
        let bash_path = completion_test_bash_path();
        let dynamic = generate_bash_dynamic_completions().replacen(
            &format!("local cmd=\"{}\"", completion_command_path_literal()),
            "local cmd=\"$tmpdir/vulcan\"",
            1,
        );
        let script = format!(
            r#"
set -uo pipefail
tmpdir="$(mktemp -d)"
record_path="$tmpdir/args.txt"
cat > "$tmpdir/vulcan" <<'EOF'
#!/bin/sh
set -eu
: > "$RECORD_PATH"
for arg in "$@"; do
    printf '%s\n' "$arg" >> "$RECORD_PATH"
done
printf 'Home.md\n'
EOF
chmod +x "$tmpdir/vulcan"
export RECORD_PATH="$record_path"
{dynamic}
COMP_WORDS=(vulcan --vault "/tmp/test vault" note get Ho)
COMP_CWORD=5
COMPREPLY=()
__vulcan_dynamic_complete note
for reply in "${{COMPREPLY[@]}}"; do
    printf 'REPLY:%s\n' "$reply"
done
while IFS= read -r recorded_arg; do
    printf 'ARG:%s\n' "$recorded_arg"
done < "$record_path"
"#
        );

        let output = ProcessCommand::new(&bash_path)
            .arg("-lc")
            .arg(script)
            .output()
            .expect("bash should run generated completion helper");
        assert!(
            output.status.success(),
            "bash helper should succeed (shell: {:?}, status: {:?})\nstdout:\n{}\nstderr:\n{}",
            bash_path,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reply_lines: Vec<&str> = stdout
            .lines()
            .filter_map(|line| line.strip_prefix("REPLY:"))
            .collect();
        let recorded_args: Vec<&str> = stdout
            .lines()
            .filter_map(|line| line.strip_prefix("ARG:"))
            .collect();
        assert_eq!(reply_lines, vec!["Home.md"]);
        assert_eq!(
            recorded_args,
            vec!["--vault", "/tmp/test vault", "complete", "note", "Ho"]
        );
    }

    #[test]
    fn bash_dynamic_completions_complete_custom_tool_names_and_flags() {
        #[cfg(windows)]
        let Some(bash_path) = completion_test_bash_path() else {
            return;
        };
        #[cfg(not(windows))]
        let bash_path = completion_test_bash_path();
        let dynamic = generate_bash_dynamic_completions().replace(
            &format!("local cmd=\"{}\"", completion_command_path_literal()),
            "local cmd=\"$tmpdir/vulcan\"",
        );
        let script = format!(
            r#"
set -uo pipefail
tmpdir="$(mktemp -d)"
cat > "$tmpdir/vulcan" <<'EOF'
#!/bin/sh
set -eu
while [ "$#" -gt 0 ]; do
    if [ "$1" = "complete" ]; then
        context="$2"
        case "$context" in
            custom-tool)
                printf 'conversation-export\n'
                ;;
            custom-tool-flag:conversation-export)
                printf -- '--assistant\n'
                ;;
            custom-tool-value:conversation-export:source)
                printf 'codex\n'
                ;;
        esac
        exit 0
    fi
    shift
done
EOF
chmod +x "$tmpdir/vulcan"
_vulcan() {{ COMPREPLY=(); }}
{dynamic}
COMP_WORDS=(vulcan --vault /tmp/vault tool run con)
COMP_CWORD=5
COMPREPLY=()
__vulcan_dynamic_dispatch vulcan con run
for reply in "${{COMPREPLY[@]}}"; do
    printf 'NAME:%s\n' "$reply"
done
COMP_WORDS=(vulcan --vault /tmp/vault tool run conversation-export --ass)
COMP_CWORD=6
COMPREPLY=()
__vulcan_dynamic_dispatch vulcan --ass conversation-export
for reply in "${{COMPREPLY[@]}}"; do
    printf 'FLAG:%s\n' "$reply"
done
COMP_WORDS=(vulcan --vault /tmp/vault tool run conversation-export --source co)
COMP_CWORD=7
COMPREPLY=()
__vulcan_dynamic_dispatch vulcan co --source
for reply in "${{COMPREPLY[@]}}"; do
    printf 'VALUE:%s\n' "$reply"
done
"#
        );

        let output = ProcessCommand::new(&bash_path)
            .arg("-lc")
            .arg(script)
            .output()
            .expect("bash should run generated completion helper");
        assert!(
            output.status.success(),
            "bash helper should succeed (shell: {:?}, status: {:?})\nstdout:\n{}\nstderr:\n{}",
            bash_path,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("NAME:conversation-export"));
        assert!(stdout.contains("FLAG:--assistant"));
        assert!(stdout.contains("VALUE:codex"));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn dynamic_completion_scripts_replay_leading_global_args() {
        let fish = generate_fish_dynamic_completions();
        assert!(
            fish.contains("function __fish_vulcan_completion_prefix_args"),
            "fish completions should define a helper that collects global args"
        );
        assert!(
            fish.contains("set -l prefix (commandline -ct)"),
            "fish completions should capture the current token for prefix-aware completion"
        );
        assert!(
            fish.contains("set -l cmd \""),
            "fish completions should pin the generating vulcan binary path"
        );
        assert!(
            fish.contains("$cmd $args complete note \"$prefix\""),
            "fish note completions should replay collected args and the current token into vulcan complete"
        );
        assert!(
            fish.contains("function __fish_vulcan_dynamic_complete_note"),
            "fish completions should define a dedicated note helper"
        );
        assert!(
            fish.contains("(__fish_vulcan_dynamic_complete_note)"),
            "fish note completion should use the dedicated note helper"
        );
        assert!(
            fish.contains("function __fish_vulcan_complete_vault_path_arg"),
            "fish completions should define a dedicated vault-path helper"
        );
        assert!(
            fish.contains("(__fish_vulcan_complete_vault_path_arg)"),
            "fish path completions should use the dedicated vault-path helper"
        );
        assert!(
            fish.contains("$cmd $args complete custom-tool \"$prefix\""),
            "fish custom tool completion should replay into vulcan complete"
        );
        assert!(
            fish.contains("$cmd $args complete custom-tool-flag:$tool \"$prefix\""),
            "fish custom tool flag completion should include the selected tool"
        );
        assert!(
            fish.contains("$cmd $args complete custom-tool-value:$tool:$flag \"$prefix\""),
            "fish custom tool value completion should include the selected tool and flag"
        );

        let bash = generate_bash_dynamic_completions();
        assert!(
            bash.contains("__vulcan_completion_prefix_args"),
            "bash completions should define a helper that collects global args"
        );
        assert!(
            bash.contains("local cmd=\""),
            "bash completions should pin the generating vulcan binary path"
        );
        assert!(
            bash.contains("\"$cmd\" \"${args[@]}\" complete \"$context\" \"${COMP_WORDS[COMP_CWORD]}\""),
            "bash completions should replay collected args and the current token into vulcan complete"
        );
        assert!(
            bash.contains("while IFS= read -r arg; do"),
            "bash completions should collect forwarded args without relying on mapfile"
        );
        assert!(
            bash.contains("COMPREPLY+=(\"$candidate\")"),
            "bash completions should append exact completion candidates without compgen word-splitting"
        );
        assert!(
            !bash.contains("mapfile"),
            "bash completions should remain compatible with Bash 3.2 on macOS"
        );
        assert!(
            bash.contains("--vault=*"),
            "bash completions should preserve inline --vault assignments"
        );
        assert!(
            bash.contains("complete -F __vulcan_dynamic_dispatch"),
            "bash completions should install a wrapper for dynamic tool run completions"
        );
        assert!(
            bash.contains("__vulcan_dynamic_candidates custom-tool"),
            "bash custom tool completion should call vulcan complete"
        );
        assert!(
            bash.contains("__vulcan_dynamic_candidates \"custom-tool-flag:$tool\""),
            "bash custom tool flag completion should include the selected tool"
        );
        assert!(
            bash.contains("__vulcan_dynamic_candidates \"custom-tool-value:$tool:$flag\""),
            "bash custom tool value completion should include the selected tool and flag"
        );

        let zsh = generate_zsh_dynamic_completions();
        assert!(
            zsh.contains("_vulcan_completion_prefix_args"),
            "zsh completions should define a helper that collects global args"
        );
        assert!(
            zsh.contains("local cmd=\""),
            "zsh completions should pin the generating vulcan binary path"
        );
        assert!(
            zsh.contains("\"$cmd\" \"${args[@]}\" complete note \"${words[CURRENT]}\""),
            "zsh note completion should replay collected args and the current token into vulcan complete"
        );
        assert!(
            zsh.contains("--vault=*"),
            "zsh completions should preserve inline --vault assignments"
        );
        assert!(
            zsh.contains("complete custom-tool \"${words[CURRENT]}\""),
            "zsh custom tool completion should call vulcan complete"
        );
        assert!(
            zsh.contains("complete custom-tool-flag:$tool \"${words[CURRENT]}\""),
            "zsh custom tool flag completion should include the selected tool"
        );
        assert!(
            zsh.contains("custom-tool-value:$tool:$flag"),
            "zsh custom tool value completion should include the selected tool and flag"
        );
        assert!(
            zsh.contains("functions -c _vulcan _vulcan_static"),
            "zsh completions should wrap the generated completer for dynamic tool run completions"
        );
    }

    #[test]
    fn vault_path_completion_lists_entries_relative_to_prefix() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        fs::create_dir_all(temp_dir.path().join("Projects")).expect("projects dir should exist");
        fs::create_dir_all(temp_dir.path().join(".vulcan")).expect("internal dir should exist");
        fs::write(temp_dir.path().join("Home.md"), "# Home\n").expect("note should write");
        fs::write(temp_dir.path().join("Projects/Alpha.md"), "# Alpha\n")
            .expect("nested note should write");
        let paths = VaultPaths::new(temp_dir.path().to_path_buf());

        assert_eq!(
            collect_complete_candidates(&paths, "vault-path", Some("H")),
            vec!["Home.md".to_string()]
        );
        assert_eq!(
            collect_complete_candidates(&paths, "vault-path", Some("Projects/")),
            vec!["Projects/Alpha.md".to_string()]
        );
        assert!(
            !collect_complete_candidates(&paths, "vault-path", Some(""))
                .contains(&".vulcan/".to_string()),
            "internal state directories should be hidden from vault path completions"
        );
    }

    #[test]
    fn describe_report_lists_core_commands() {
        let report = describe_cli();

        assert_eq!(report.name, "vulcan");
        let index = report
            .commands
            .iter()
            .find(|command| command.name == "index")
            .expect("index command should be described");
        assert_eq!(
            index.about.as_deref(),
            Some("Initialize, scan, rebuild, repair, watch, and serve index state")
        );
        let completions = report
            .commands
            .iter()
            .find(|command| command.name == "completions")
            .expect("completions command should be described");
        assert_eq!(
            completions.about.as_deref(),
            Some("Generate shell completion scripts")
        );
        assert!(report.commands.iter().any(|command| command.name == "help"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "index"));
        assert!(report.commands.iter().any(|command| command.name == "note"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "kanban"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "refactor"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "config"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "browse"));
        assert!(report.commands.iter().any(|command| command.name == "edit"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "graph"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "dataview"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "cache"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "saved"));
        assert!(report.commands.iter().any(|command| command.name == "run"));
        assert!(report
            .commands
            .iter()
            .all(|command| command.name != "suggest"));
        assert!(report
            .commands
            .iter()
            .all(|command| command.name != "rewrite"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "checkpoint"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "changes"));
        assert!(report
            .commands
            .iter()
            .all(|command| command.name != "batch"));
        assert!(report
            .commands
            .iter()
            .all(|command| command.name != "related"));
        assert!(report
            .commands
            .iter()
            .all(|command| command.name != "cluster"));
        assert!(report
            .commands
            .iter()
            .all(|command| command.name != "weekly"));
        assert!(report
            .commands
            .iter()
            .all(|command| command.name != "monthly"));
    }

    fn collect_option_help_gaps(
        command_path: &mut Vec<String>,
        command: &CliCommandDescribe,
        gaps: &mut Vec<String>,
    ) {
        command_path.push(command.name.clone());
        for option in &command.options {
            if option_help_is_blank(option) {
                gaps.push(format!("{} --{}", command_path.join(" "), option.id));
            }
        }
        for subcommand in &command.subcommands {
            collect_option_help_gaps(command_path, subcommand, gaps);
        }
        command_path.pop();
    }

    fn option_help_is_blank(option: &CliArgDescribe) -> bool {
        match option.help.as_deref() {
            Some(help) => help.trim().is_empty(),
            None => true,
        }
    }

    #[test]
    fn describe_report_options_have_help_text() {
        let report = describe_cli();
        let mut gaps = Vec::new();
        for option in &report.global_options {
            if option_help_is_blank(option) {
                gaps.push(format!("global --{}", option.id));
            }
        }
        for command in &report.commands {
            collect_option_help_gaps(&mut Vec::new(), command, &mut gaps);
        }

        assert!(
            gaps.is_empty(),
            "every described CLI option should include help text; missing: {gaps:?}"
        );
    }

    #[test]
    fn help_overview_lists_current_top_level_surfaces() {
        let report = help_overview();

        for expected in [
            "`mcp`",
            "`describe`",
            "`completions`",
            "`status`",
            "`plugin`",
            "`today`",
        ] {
            assert!(
                report.body.contains(expected),
                "help overview should mention {expected}"
            );
        }
    }

    #[test]
    fn resolve_template_file_matches_by_bare_name() {
        // Build a minimal set of candidates
        let candidates = vec![
            TemplateCandidate {
                name: "daily.md".to_string(),
                display_path: ".vulcan/templates/daily.md".to_string(),
                source: "vulcan",
                absolute_path: PathBuf::from(".vulcan/templates/daily.md"),
                warning: None,
            },
            TemplateCandidate {
                name: "weekly.md".to_string(),
                display_path: ".vulcan/templates/weekly.md".to_string(),
                source: "vulcan",
                absolute_path: PathBuf::from(".vulcan/templates/weekly.md"),
                warning: None,
            },
        ];

        // Match by bare name (no extension)
        let paths = VaultPaths::new(PathBuf::from("/tmp/fake-vault"));
        let result = resolve_template_file(&paths, &candidates, "daily");
        assert!(result.is_ok(), "should match by bare name");
        assert_eq!(result.unwrap().name, "daily.md");
    }

    #[test]
    fn resolve_template_file_matches_by_display_path_with_directory() {
        // Simulate a template whose display_path includes a directory component, as happens
        // when the Templater/Obsidian folder is a subdirectory like
        // "00-09 Management & Meta/05 Templates".
        let candidates = vec![TemplateCandidate {
            name: "daily.md".to_string(),
            display_path: "00-09 Management & Meta/05 Templates/daily.md".to_string(),
            source: "templater",
            absolute_path: PathBuf::from("00-09 Management & Meta/05 Templates/daily.md"),
            warning: None,
        }];

        let paths = VaultPaths::new(PathBuf::from("/tmp/fake-vault"));

        // Match by full directory path without extension (what periodic config provides)
        let r1 = resolve_template_file(
            &paths,
            &candidates,
            "00-09 Management & Meta/05 Templates/daily",
        );
        assert!(r1.is_ok(), "should match by display_path without .md");

        // Match by full directory path with extension
        let r2 = resolve_template_file(
            &paths,
            &candidates,
            "00-09 Management & Meta/05 Templates/daily.md",
        );
        assert!(r2.is_ok(), "should match by display_path with .md");

        // Match by bare name still works
        let r3 = resolve_template_file(&paths, &candidates, "daily");
        assert!(r3.is_ok(), "bare name should still match");
    }

    #[test]
    fn list_templates_in_directory_scans_subdirectories() {
        use std::fs;

        let tmp = tempfile::tempdir().expect("tempdir should be created");
        let root = tmp.path();

        // Create a nested template
        let sub = root.join("subdir");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("nested.md"), "# Nested").unwrap();
        // Also a top-level one
        fs::write(root.join("top.md"), "# Top").unwrap();
        // Non-markdown file should be ignored
        fs::write(root.join("ignored.txt"), "ignore me").unwrap();

        let templates =
            list_templates_in_directory(root, "Templates", "test").expect("should list templates");

        assert_eq!(templates.len(), 2, "should find both .md files");
        let names: Vec<&str> = templates.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"nested.md"), "nested.md should be found");
        assert!(names.contains(&"top.md"), "top.md should be found");

        let nested = templates.iter().find(|t| t.name == "nested.md").unwrap();
        assert!(
            nested.display_path.contains("subdir"),
            "display_path should include subdir"
        );
    }
}
