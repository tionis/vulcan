use crate::bases::inspect_base_file;
pub use crate::content_transforms::{
    ContentReplacementRuleConfig, ContentTransformConfig, ContentTransformRuleConfig,
};
use crate::paths::{
    ensure_vulcan_dir, normalize_relative_input_path, RelativePathOptions, VaultPaths,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;

const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("default_config.toml");

mod defaults;
mod importers;

use defaults::{
    bytes_to_kilobytes_ceil, bytes_to_megabytes_ceil, default_attachment_extraction_extensions,
    default_cancelled_task_statuses, default_completed_task_statuses,
    default_dataview_default_date_format, default_dataview_default_datetime_format,
    default_dataview_display_result_count, default_dataview_enable_dataview_js,
    default_dataview_enable_inline_dataview_js, default_dataview_group_column_name,
    default_dataview_inline_js_query_prefix, default_dataview_inline_query_prefix,
    default_dataview_js_max_stack_size_bytes, default_dataview_js_memory_limit_bytes,
    default_dataview_js_timeout_seconds, default_dataview_max_recursive_render_depth,
    default_dataview_primary_column_name, default_dataview_recursive_subtask_completion,
    default_dataview_task_completion_text, default_dataview_task_completion_tracking,
    default_dataview_task_completion_use_emoji_shorthand, default_enabled_plugin_registration,
    default_in_progress_task_statuses, default_js_runtime_default_timeout_seconds,
    default_js_runtime_memory_limit_mb, default_js_runtime_scripts_folder,
    default_js_runtime_stack_limit_kb, default_non_task_statuses,
    default_tasknotes_auto_archive_delay, default_tasknotes_nlp_language,
    default_tasknotes_nlp_triggers, default_tasknotes_pomodoro_long_break,
    default_tasknotes_pomodoro_long_break_interval, default_tasknotes_pomodoro_short_break,
    default_tasknotes_pomodoro_work_duration, default_tasknotes_priorities,
    default_tasknotes_statuses, default_todo_task_statuses, default_true,
};

pub use importers::{
    all_importers, annotate_import_conflicts, import_core_plugin_config,
    import_dataview_plugin_config, import_kanban_plugin_config,
    import_periodic_notes_plugin_config, import_quickadd_plugin_config,
    import_tasknotes_plugin_config, import_tasks_plugin_config, import_templater_plugin_config,
    ConfigImportError, ConfigImportMapping, ConfigImportReport, CoreImporter, DataviewImporter,
    ImportConflict, ImportMigratedFile, ImportMigratedFileAction, ImportSkippedSetting,
    ImportTarget, KanbanImporter, PeriodicNotesImporter, PluginImporter, QuickAddImporter,
    TaskNotesImporter, TasksImporter, TemplaterImporter,
};
mod obsidian;

mod partial;

#[cfg(test)]
use importers::{tasknotes_migrate_view_files, tasknotes_skipped_settings};

use partial::{
    PartialPermissionProfile, PartialPermissionsConfig, PartialPluginRegistration,
    PartialTaskNotesFieldMapping, PartialVulcanConfig,
};

use obsidian::{
    ObsidianAppConfig, ObsidianDailyNotesConfig, ObsidianDataviewConfig, ObsidianKanbanConfig,
    ObsidianPeriodicNoteSettings, ObsidianPeriodicNotesConfig, ObsidianQuickAddAiConfig,
    ObsidianQuickAddAiProviderConfig, ObsidianQuickAddChoice, ObsidianQuickAddConfig,
    ObsidianQuickAddFormatConfig, ObsidianTaskNotesConfig, ObsidianTaskNotesCreationDefaults,
    ObsidianTaskNotesDefaultReminder, ObsidianTaskNotesFieldMapping, ObsidianTasksConfig,
    ObsidianTemplaterConfig, ObsidianTemplaterFolderTemplateConfig, ObsidianTemplatesConfig,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkingStrategy {
    #[default]
    Heading,
    Fixed,
    Paragraph,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkResolutionMode {
    #[default]
    Shortest,
    Relative,
    Absolute,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkStylePreference {
    #[default]
    Wikilink,
    Markdown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutoScanMode {
    Off,
    #[default]
    Blocking,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkingConfig {
    pub strategy: ChunkingStrategy,
    pub target_size: usize,
    pub overlap: usize,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            strategy: ChunkingStrategy::Heading,
            target_size: 4_000,
            overlap: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingProviderConfig {
    pub provider: Option<String>,
    pub base_url: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub normalized: Option<bool>,
    pub max_batch_size: Option<usize>,
    pub max_input_tokens: Option<usize>,
    pub max_concurrency: Option<usize>,
    pub cache_key: Option<String>,
}

impl Default for EmbeddingProviderConfig {
    fn default() -> Self {
        Self {
            provider: None,
            base_url: "http://localhost:11434/v1".to_string(),
            model: "text-embedding-3-small".to_string(),
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            normalized: Some(true),
            max_batch_size: Some(32),
            max_input_tokens: Some(8192),
            max_concurrency: Some(4),
            cache_key: None,
        }
    }
}

impl EmbeddingProviderConfig {
    #[must_use]
    pub fn provider_name(&self) -> &str {
        self.provider.as_deref().unwrap_or("openai-compatible")
    }

    #[must_use]
    pub fn effective_cache_key(&self) -> String {
        if let Some(key) = &self.cache_key {
            key.clone()
        } else {
            format!("{}:{}", self.provider_name(), self.model)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentExtractionConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_attachment_extraction_extensions")]
    pub extensions: Vec<String>,
    pub max_output_bytes: Option<usize>,
}

impl Default for AttachmentExtractionConfig {
    fn default() -> Self {
        Self {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "case \"$2\" in pdf) pdftotext \"$1\" - ;; png|jpg|jpeg|webp) tesseract \"$1\" stdout ;; *) exit 0 ;; esac".to_string(),
                "sh".to_string(),
                "{path}".to_string(),
                "{extension}".to_string(),
            ],
            extensions: default_attachment_extraction_extensions(),
            max_output_bytes: Some(262_144),
        }
    }
}

impl AttachmentExtractionConfig {
    #[must_use]
    pub fn supports_extension(&self, extension: &str) -> bool {
        self.extensions
            .iter()
            .any(|configured| configured.eq_ignore_ascii_case(extension))
    }

    #[must_use]
    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes.unwrap_or(256 * 1024).max(1024)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitTrigger {
    #[default]
    Mutation,
    Scan,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitScope {
    #[default]
    VulcanOnly,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitConfig {
    pub auto_commit: bool,
    pub trigger: GitTrigger,
    pub message: String,
    pub scope: GitScope,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            auto_commit: false,
            trigger: GitTrigger::Mutation,
            message: "vulcan {action}: {files}".to_string(),
            scope: GitScope::VulcanOnly,
            exclude: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxConfig {
    pub path: String,
    pub format: String,
    pub timestamp: bool,
    pub heading: Option<String>,
}

impl Default for InboxConfig {
    fn default() -> Self {
        Self {
            path: "Inbox.md".to_string(),
            format: "- {text}".to_string(),
            timestamp: true,
            heading: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplaterCommandPairConfig {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplaterFolderTemplateConfig {
    pub folder: PathBuf,
    pub template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplaterFileTemplateConfig {
    pub regex: String,
    pub template: String,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplatesConfig {
    pub date_format: String,
    pub time_format: String,
    pub obsidian_folder: Option<PathBuf>,
    pub templater_folder: Option<PathBuf>,
    #[serde(default = "default_templater_command_timeout")]
    pub command_timeout: usize,
    #[serde(default)]
    pub templates_pairs: Vec<TemplaterCommandPairConfig>,
    #[serde(default)]
    pub trigger_on_file_creation: bool,
    #[serde(default)]
    pub auto_jump_to_cursor: bool,
    #[serde(default)]
    pub enable_system_commands: bool,
    #[serde(default)]
    pub shell_path: Option<PathBuf>,
    #[serde(default)]
    pub user_scripts_folder: Option<PathBuf>,
    #[serde(default)]
    pub web_allowlist: Vec<String>,
    #[serde(default = "default_templater_enable_folder_templates")]
    pub enable_folder_templates: bool,
    #[serde(default)]
    pub folder_templates: Vec<TemplaterFolderTemplateConfig>,
    #[serde(default)]
    pub enable_file_templates: bool,
    #[serde(default)]
    pub file_templates: Vec<TemplaterFileTemplateConfig>,
    #[serde(default = "default_templater_syntax_highlighting")]
    pub syntax_highlighting: bool,
    #[serde(default)]
    pub syntax_highlighting_mobile: bool,
    #[serde(default)]
    pub enabled_templates_hotkeys: Vec<String>,
    #[serde(default)]
    pub startup_templates: Vec<String>,
    #[serde(default = "default_templater_intellisense_render")]
    pub intellisense_render: usize,
}

impl Default for TemplatesConfig {
    fn default() -> Self {
        Self {
            date_format: "YYYY-MM-DD".to_string(),
            time_format: "HH:mm".to_string(),
            obsidian_folder: None,
            templater_folder: None,
            command_timeout: default_templater_command_timeout(),
            templates_pairs: Vec::new(),
            trigger_on_file_creation: false,
            auto_jump_to_cursor: false,
            enable_system_commands: false,
            shell_path: None,
            user_scripts_folder: None,
            web_allowlist: Vec::new(),
            enable_folder_templates: default_templater_enable_folder_templates(),
            folder_templates: Vec::new(),
            enable_file_templates: false,
            file_templates: Vec::new(),
            syntax_highlighting: default_templater_syntax_highlighting(),
            syntax_highlighting_mobile: false,
            enabled_templates_hotkeys: Vec::new(),
            startup_templates: Vec::new(),
            intellisense_render: default_templater_intellisense_render(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuickAddCreateFileConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub create_with_template: bool,
    #[serde(default)]
    pub template: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuickAddInsertAfterConfig {
    pub heading: String,
    #[serde(default)]
    pub insert_at_end: bool,
    #[serde(default)]
    pub consider_subsections: bool,
    #[serde(default)]
    pub create_if_not_found: bool,
    #[serde(default)]
    pub create_if_not_found_location: Option<String>,
    #[serde(default)]
    pub inline: bool,
    #[serde(default)]
    pub replace_existing: bool,
    #[serde(default)]
    pub blank_line_after_match_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuickAddTemplateFolderConfig {
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    #[serde(default)]
    pub choose_when_creating_note: bool,
    #[serde(default)]
    pub create_in_same_folder_as_active_file: bool,
    #[serde(default)]
    pub choose_from_subfolders: bool,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuickAddCaptureChoiceConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub capture_to: Option<String>,
    #[serde(default)]
    pub capture_to_active_file: bool,
    #[serde(default)]
    pub active_file_write_position: Option<String>,
    #[serde(default)]
    pub create_file_if_missing: QuickAddCreateFileConfig,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub use_selection_as_capture_value: Option<bool>,
    #[serde(default)]
    pub prepend: bool,
    #[serde(default)]
    pub task: bool,
    #[serde(default)]
    pub insert_after: Option<QuickAddInsertAfterConfig>,
    #[serde(default)]
    pub new_line_capture_direction: Option<String>,
    #[serde(default)]
    pub open_file: bool,
    #[serde(default)]
    pub templater_after_capture: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuickAddTemplateChoiceConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub template_path: Option<PathBuf>,
    #[serde(default)]
    pub folder: QuickAddTemplateFolderConfig,
    #[serde(default)]
    pub file_name_format: Option<String>,
    #[serde(default)]
    pub open_file: bool,
    #[serde(default)]
    pub file_exists_behavior: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuickAddAiProviderConfig {
    pub name: String,
    pub endpoint: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub model_source: Option<String>,
    #[serde(default)]
    pub auto_sync_models: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuickAddAiConfig {
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub default_system_prompt: Option<String>,
    #[serde(default)]
    pub prompt_templates_folder: Option<PathBuf>,
    #[serde(default)]
    pub show_assistant: bool,
    #[serde(default)]
    pub providers: Vec<QuickAddAiProviderConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuickAddConfig {
    #[serde(default)]
    pub template_folder: Option<PathBuf>,
    #[serde(default)]
    pub global_variables: BTreeMap<String, String>,
    #[serde(default)]
    pub capture_choices: Vec<QuickAddCaptureChoiceConfig>,
    #[serde(default)]
    pub template_choices: Vec<QuickAddTemplateChoiceConfig>,
    #[serde(default)]
    pub ai: Option<QuickAddAiConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantConfig {
    #[serde(default = "default_assistant_prompts_folder")]
    pub prompts_folder: PathBuf,
    #[serde(default = "default_assistant_skills_folder")]
    pub skills_folder: PathBuf,
    #[serde(default = "default_assistant_tools_folder")]
    pub tools_folder: PathBuf,
}

impl Default for AssistantConfig {
    fn default() -> Self {
        Self {
            prompts_folder: default_assistant_prompts_folder(),
            skills_folder: default_assistant_skills_folder(),
            tools_folder: default_assistant_tools_folder(),
        }
    }
}

fn default_assistant_prompts_folder() -> PathBuf {
    PathBuf::from("AI/Prompts")
}

fn default_assistant_skills_folder() -> PathBuf {
    PathBuf::from(".agents/skills")
}

fn default_assistant_tools_folder() -> PathBuf {
    PathBuf::from(".agents/tools")
}

/// Which HTTP-based search provider to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SearchBackendKind {
    /// Disable `web search` and `web.search()` entirely.
    Disabled,
    /// Auto-detect: try `Kagi` → `Exa` → `Tavily` → `Brave` based on available env vars, then
    /// `Ollama`, then fall back to `DuckDuckGo`.
    Auto,
    /// `DuckDuckGo` HTML search — works without an API key.
    #[default]
    Duckduckgo,
    /// Kagi Search — requires `KAGI_API_KEY` (or configured `api_key_env`).
    Kagi,
    /// Exa (formerly Metaphor) — requires `EXA_API_KEY`.
    Exa,
    /// Tavily Search — requires `TAVILY_API_KEY`.
    Tavily,
    /// Brave Search — requires `BRAVE_API_KEY`.
    Brave,
    /// Ollama Web Search — requires `OLLAMA_API_KEY`.
    Ollama,
}

impl SearchBackendKind {
    /// The environment variable that holds the API key for this backend.
    #[must_use]
    pub fn default_api_key_env(self) -> Option<&'static str> {
        match self {
            SearchBackendKind::Disabled
            | SearchBackendKind::Auto
            | SearchBackendKind::Duckduckgo => None,
            SearchBackendKind::Kagi => Some("KAGI_API_KEY"),
            SearchBackendKind::Exa => Some("EXA_API_KEY"),
            SearchBackendKind::Tavily => Some("TAVILY_API_KEY"),
            SearchBackendKind::Brave => Some("BRAVE_API_KEY"),
            SearchBackendKind::Ollama => Some("OLLAMA_API_KEY"),
        }
    }

    /// The canonical base URL for this backend's search endpoint.
    #[must_use]
    pub fn default_base_url(self) -> &'static str {
        match self {
            SearchBackendKind::Disabled => "",
            SearchBackendKind::Auto | SearchBackendKind::Duckduckgo => {
                "https://html.duckduckgo.com/html/"
            }
            SearchBackendKind::Kagi => "https://kagi.com/api/v0/search",
            SearchBackendKind::Exa => "https://api.exa.ai/search",
            SearchBackendKind::Tavily => "https://api.tavily.com/search",
            SearchBackendKind::Brave => "https://api.search.brave.com/res/v1/web/search",
            SearchBackendKind::Ollama => "https://ollama.com/api/web_search",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchConfig {
    /// Which backend to use. Defaults to `duckduckgo`; set to `auto` for env-var detection or
    /// `disabled` to turn off web search entirely.
    #[serde(default)]
    pub backend: SearchBackendKind,
    /// Override the env var name that holds the API key (defaults to backend's `default_api_key_env`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Override the search endpoint URL (defaults to backend's `default_base_url`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl WebSearchConfig {
    /// Effective env-var name for the API key, accounting for any override.
    #[must_use]
    pub fn effective_api_key_env(&self) -> Option<&str> {
        self.api_key_env
            .as_deref()
            .or_else(|| self.backend.default_api_key_env())
    }

    /// Effective base URL for the search endpoint, accounting for any override.
    #[must_use]
    pub fn effective_base_url(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or_else(|| self.backend.default_base_url())
    }
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackendKind::Duckduckgo,
            api_key_env: None,
            base_url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebConfig {
    pub user_agent: String,
    pub search: WebSearchConfig,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            user_agent: "Vulcan/0.1 (+https://github.com/tionis/vulcan)".to_string(),
            search: WebSearchConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JsRuntimeSandbox {
    #[default]
    Strict,
    Fs,
    Net,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsRuntimeConfig {
    #[serde(default = "default_js_runtime_memory_limit_mb")]
    pub memory_limit_mb: usize,
    #[serde(default = "default_js_runtime_stack_limit_kb")]
    pub stack_limit_kb: usize,
    #[serde(default = "default_js_runtime_default_timeout_seconds")]
    pub default_timeout_seconds: usize,
    #[serde(default)]
    pub default_sandbox: JsRuntimeSandbox,
    #[serde(default = "default_js_runtime_scripts_folder")]
    pub scripts_folder: PathBuf,
}

impl Default for JsRuntimeConfig {
    fn default() -> Self {
        Self {
            memory_limit_mb: default_js_runtime_memory_limit_mb(),
            stack_limit_kb: default_js_runtime_stack_limit_kb(),
            default_timeout_seconds: default_js_runtime_default_timeout_seconds(),
            default_sandbox: JsRuntimeSandbox::default(),
            scripts_folder: default_js_runtime_scripts_folder(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginEvent {
    OnNoteWrite,
    OnNoteCreate,
    OnNoteDelete,
    OnPreCommit,
    OnPostCommit,
    OnScanComplete,
    OnRefactor,
}

impl PluginEvent {
    #[must_use]
    pub fn handler_name(self) -> &'static str {
        match self {
            Self::OnNoteWrite => "on_note_write",
            Self::OnNoteCreate => "on_note_create",
            Self::OnNoteDelete => "on_note_delete",
            Self::OnPreCommit => "on_pre_commit",
            Self::OnPostCommit => "on_post_commit",
            Self::OnScanComplete => "on_scan_complete",
            Self::OnRefactor => "on_refactor",
        }
    }

    #[must_use]
    pub fn is_blocking(self) -> bool {
        matches!(self, Self::OnNoteWrite | Self::OnPreCommit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRegistration {
    #[serde(default = "default_enabled_plugin_registration")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<PluginEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<JsRuntimeSandbox>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Default for PluginRegistration {
    fn default() -> Self {
        Self {
            enabled: default_enabled_plugin_registration(),
            path: None,
            events: Vec::new(),
            sandbox: None,
            permission_profile: None,
            description: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskCompletionState {
    pub checked: bool,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStatusDefinition {
    pub symbol: String,
    pub name: String,
    #[serde(rename = "type", alias = "statusType")]
    pub status_type: String,
    #[serde(default, alias = "nextStatusSymbol")]
    pub next_symbol: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStatusState {
    pub checked: bool,
    pub completed: bool,
    pub name: String,
    pub status_type: String,
    pub next_symbol: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStatusesConfig {
    #[serde(default = "default_todo_task_statuses")]
    pub todo: Vec<String>,
    #[serde(default = "default_completed_task_statuses")]
    pub completed: Vec<String>,
    #[serde(default = "default_in_progress_task_statuses")]
    pub in_progress: Vec<String>,
    #[serde(default = "default_cancelled_task_statuses")]
    pub cancelled: Vec<String>,
    #[serde(default = "default_non_task_statuses")]
    pub non_task: Vec<String>,
    #[serde(default)]
    pub definitions: Vec<TaskStatusDefinition>,
}

impl Default for TaskStatusesConfig {
    fn default() -> Self {
        Self {
            todo: default_todo_task_statuses(),
            completed: default_completed_task_statuses(),
            in_progress: default_in_progress_task_statuses(),
            cancelled: default_cancelled_task_statuses(),
            non_task: default_non_task_statuses(),
            definitions: Vec::new(),
        }
    }
}

impl TaskStatusesConfig {
    #[must_use]
    pub fn status_state(&self, status_char: &str) -> TaskStatusState {
        let definition = self
            .definition(status_char)
            .cloned()
            .unwrap_or_else(|| self.fallback_definition(status_char));
        let status_type = normalize_task_status_type(&definition.status_type);

        TaskStatusState {
            checked: !status_char.is_empty() && status_type != "TODO",
            completed: status_type == "DONE",
            name: if definition.name.trim().is_empty() {
                default_task_status_name(&status_type)
            } else {
                definition.name
            },
            status_type,
            next_symbol: definition.next_symbol,
        }
    }

    #[must_use]
    pub fn completion_state(&self, status_char: &str) -> TaskCompletionState {
        let state = self.status_state(status_char);

        TaskCompletionState {
            checked: state.checked,
            completed: state.completed,
        }
    }

    fn definition(&self, status_char: &str) -> Option<&TaskStatusDefinition> {
        self.definitions
            .iter()
            .find(|definition| definition.symbol == status_char)
    }

    fn matches_status(status_char: &str, candidates: &[String]) -> bool {
        candidates.iter().any(|candidate| candidate == status_char)
    }

    fn fallback_definition(&self, status_char: &str) -> TaskStatusDefinition {
        let status_type = if Self::matches_status(status_char, &self.todo) {
            "TODO"
        } else if Self::matches_status(status_char, &self.completed) {
            "DONE"
        } else if Self::matches_status(status_char, &self.in_progress) {
            "IN_PROGRESS"
        } else if Self::matches_status(status_char, &self.cancelled) {
            "CANCELLED"
        } else if Self::matches_status(status_char, &self.non_task) {
            "NON_TASK"
        } else if status_char.is_empty() {
            "TODO"
        } else {
            "UNKNOWN"
        };

        TaskStatusDefinition {
            symbol: status_char.to_string(),
            name: default_task_status_name(status_type),
            status_type: status_type.to_string(),
            next_symbol: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TasksConfig {
    #[serde(default)]
    pub default_source: TasksDefaultSource,
    #[serde(default)]
    pub statuses: TaskStatusesConfig,
    #[serde(default)]
    pub global_filter: Option<String>,
    #[serde(default)]
    pub global_query: Option<String>,
    #[serde(default)]
    pub remove_global_filter: bool,
    #[serde(default)]
    pub set_created_date: bool,
    #[serde(default)]
    pub recurrence_on_completion: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TasksDefaultSource {
    #[serde(alias = "file")]
    Tasknotes,
    Inline,
    #[default]
    All,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskNotesIdentificationMethod {
    #[default]
    Tag,
    Property,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesFieldMapping {
    pub title: String,
    pub status: String,
    pub priority: String,
    pub due: String,
    pub scheduled: String,
    pub contexts: String,
    pub projects: String,
    pub time_estimate: String,
    pub completed_date: String,
    pub date_created: String,
    pub date_modified: String,
    pub recurrence: String,
    pub recurrence_anchor: String,
    pub archive_tag: String,
    pub time_entries: String,
    pub complete_instances: String,
    pub skipped_instances: String,
    pub blocked_by: String,
    pub pomodoros: String,
    pub reminders: String,
}

impl Default for TaskNotesFieldMapping {
    fn default() -> Self {
        Self {
            title: "title".to_string(),
            status: "status".to_string(),
            priority: "priority".to_string(),
            due: "due".to_string(),
            scheduled: "scheduled".to_string(),
            contexts: "contexts".to_string(),
            projects: "projects".to_string(),
            time_estimate: "timeEstimate".to_string(),
            completed_date: "completedDate".to_string(),
            date_created: "dateCreated".to_string(),
            date_modified: "dateModified".to_string(),
            recurrence: "recurrence".to_string(),
            recurrence_anchor: "recurrence_anchor".to_string(),
            archive_tag: "archived".to_string(),
            time_entries: "timeEntries".to_string(),
            complete_instances: "complete_instances".to_string(),
            skipped_instances: "skipped_instances".to_string(),
            blocked_by: "blockedBy".to_string(),
            pomodoros: "pomodoros".to_string(),
            reminders: "reminders".to_string(),
        }
    }
}

impl TaskNotesFieldMapping {
    #[must_use]
    pub fn reserved_property_names(&self) -> std::collections::HashSet<&str> {
        [
            self.title.as_str(),
            self.status.as_str(),
            self.priority.as_str(),
            self.due.as_str(),
            self.scheduled.as_str(),
            self.contexts.as_str(),
            self.projects.as_str(),
            self.time_estimate.as_str(),
            self.completed_date.as_str(),
            self.date_created.as_str(),
            self.date_modified.as_str(),
            self.recurrence.as_str(),
            self.recurrence_anchor.as_str(),
            self.time_entries.as_str(),
            self.complete_instances.as_str(),
            self.skipped_instances.as_str(),
            self.blocked_by.as_str(),
            self.pomodoros.as_str(),
            self.reminders.as_str(),
            "tags",
        ]
        .into_iter()
        .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesStatusConfig {
    pub id: String,
    pub value: String,
    pub label: String,
    pub color: String,
    #[serde(rename = "isCompleted")]
    pub is_completed: bool,
    #[serde(default)]
    pub order: usize,
    #[serde(default, rename = "autoArchive")]
    pub auto_archive: bool,
    #[serde(
        default = "default_tasknotes_auto_archive_delay",
        rename = "autoArchiveDelay"
    )]
    pub auto_archive_delay: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesPriorityConfig {
    pub id: String,
    pub value: String,
    pub label: String,
    pub color: String,
    pub weight: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskNotesUserFieldType {
    Text,
    Number,
    Date,
    Boolean,
    List,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesUserFieldConfig {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub key: String,
    #[serde(rename = "type")]
    pub field_type: TaskNotesUserFieldType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesNlpTriggerConfig {
    #[serde(alias = "propertyId")]
    pub property_id: String,
    pub trigger: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskNotesDateDefault {
    #[default]
    None,
    Today,
    Tomorrow,
    NextWeek,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskNotesRecurrenceDefault {
    #[default]
    None,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskNotesReminderUnit {
    #[default]
    Minutes,
    Hours,
    Days,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskNotesReminderDirection {
    #[default]
    Before,
    After,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskNotesReminderAnchor {
    #[default]
    Due,
    Scheduled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskNotesDefaultReminderType {
    #[default]
    Relative,
    Absolute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesDefaultReminderConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub reminder_type: TaskNotesDefaultReminderType,
    #[serde(default)]
    pub related_to: Option<TaskNotesReminderAnchor>,
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub unit: Option<TaskNotesReminderUnit>,
    #[serde(default)]
    pub direction: Option<TaskNotesReminderDirection>,
    #[serde(default)]
    pub absolute_time: Option<String>,
    #[serde(default)]
    pub absolute_date: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskNotesPomodoroStorageLocation {
    #[default]
    #[serde(alias = "plugin")]
    Task,
    #[serde(alias = "daily-notes")]
    DailyNote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesPomodoroConfig {
    #[serde(default = "default_tasknotes_pomodoro_work_duration")]
    pub work_duration: usize,
    #[serde(default = "default_tasknotes_pomodoro_short_break")]
    pub short_break: usize,
    #[serde(default = "default_tasknotes_pomodoro_long_break")]
    pub long_break: usize,
    #[serde(default = "default_tasknotes_pomodoro_long_break_interval")]
    pub long_break_interval: usize,
    #[serde(default)]
    pub storage_location: TaskNotesPomodoroStorageLocation,
}

impl Default for TaskNotesPomodoroConfig {
    fn default() -> Self {
        Self {
            work_duration: default_tasknotes_pomodoro_work_duration(),
            short_break: default_tasknotes_pomodoro_short_break(),
            long_break: default_tasknotes_pomodoro_long_break(),
            long_break_interval: default_tasknotes_pomodoro_long_break_interval(),
            storage_location: TaskNotesPomodoroStorageLocation::Task,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesTaskCreationDefaults {
    #[serde(default)]
    pub default_contexts: Vec<String>,
    #[serde(default)]
    pub default_tags: Vec<String>,
    #[serde(default)]
    pub default_projects: Vec<String>,
    #[serde(default)]
    pub default_time_estimate: Option<usize>,
    #[serde(default)]
    pub default_due_date: TaskNotesDateDefault,
    #[serde(default)]
    pub default_scheduled_date: TaskNotesDateDefault,
    #[serde(default)]
    pub default_recurrence: TaskNotesRecurrenceDefault,
    #[serde(default)]
    pub default_reminders: Vec<TaskNotesDefaultReminderConfig>,
}

impl Default for TaskNotesTaskCreationDefaults {
    fn default() -> Self {
        Self {
            default_contexts: Vec::new(),
            default_tags: Vec::new(),
            default_projects: Vec::new(),
            default_time_estimate: None,
            default_due_date: TaskNotesDateDefault::None,
            default_scheduled_date: TaskNotesDateDefault::None,
            default_recurrence: TaskNotesRecurrenceDefault::None,
            default_reminders: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TaskNotesSavedViewFilterValue {
    Bool(bool),
    Integer(i64),
    Text(String),
    TextList(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesSavedViewCondition {
    #[serde(rename = "type")]
    pub node_type: String,
    pub id: String,
    pub property: String,
    pub operator: String,
    #[serde(default)]
    pub value: Option<TaskNotesSavedViewFilterValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesSavedViewGroup {
    #[serde(rename = "type")]
    pub node_type: String,
    pub id: String,
    pub conjunction: String,
    #[serde(default)]
    pub children: Vec<TaskNotesSavedViewNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TaskNotesSavedViewNode {
    Condition(TaskNotesSavedViewCondition),
    Group(TaskNotesSavedViewGroup),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesSavedViewQuery {
    #[serde(rename = "type")]
    pub node_type: String,
    pub id: String,
    pub conjunction: String,
    #[serde(default)]
    pub children: Vec<TaskNotesSavedViewNode>,
    #[serde(default, rename = "sortKey")]
    pub sort_key: Option<String>,
    #[serde(default, rename = "sortDirection")]
    pub sort_direction: Option<String>,
    #[serde(default, rename = "groupKey")]
    pub group_key: Option<String>,
    #[serde(default, rename = "subgroupKey")]
    pub subgroup_key: Option<String>,
}

impl Default for TaskNotesSavedViewQuery {
    fn default() -> Self {
        Self {
            node_type: "group".to_string(),
            id: "root".to_string(),
            conjunction: "and".to_string(),
            children: Vec::new(),
            sort_key: None,
            sort_direction: None,
            group_key: None,
            subgroup_key: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TaskNotesSavedViewOptionValue {
    Bool(bool),
    Integer(i64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesSavedViewConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub query: TaskNotesSavedViewQuery,
    #[serde(default, rename = "viewOptions")]
    pub view_options: BTreeMap<String, TaskNotesSavedViewOptionValue>,
    #[serde(default, rename = "visibleProperties")]
    pub visible_properties: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotesConfig {
    pub tasks_folder: String,
    pub archive_folder: String,
    pub task_tag: String,
    pub identification_method: TaskNotesIdentificationMethod,
    pub task_property_name: Option<String>,
    pub task_property_value: Option<String>,
    #[serde(default)]
    pub excluded_folders: Vec<String>,
    pub default_status: String,
    pub default_priority: String,
    #[serde(default)]
    pub field_mapping: TaskNotesFieldMapping,
    #[serde(default = "default_tasknotes_statuses")]
    pub statuses: Vec<TaskNotesStatusConfig>,
    #[serde(default = "default_tasknotes_priorities")]
    pub priorities: Vec<TaskNotesPriorityConfig>,
    #[serde(default)]
    pub user_fields: Vec<TaskNotesUserFieldConfig>,
    #[serde(default = "default_true")]
    pub enable_natural_language_input: bool,
    #[serde(default)]
    pub nlp_default_to_scheduled: bool,
    #[serde(default = "default_tasknotes_nlp_language")]
    pub nlp_language: String,
    #[serde(default = "default_tasknotes_nlp_triggers")]
    pub nlp_triggers: Vec<TaskNotesNlpTriggerConfig>,
    #[serde(default)]
    pub pomodoro: TaskNotesPomodoroConfig,
    #[serde(default)]
    pub task_creation_defaults: TaskNotesTaskCreationDefaults,
    #[serde(default)]
    pub saved_views: Vec<TaskNotesSavedViewConfig>,
}

impl Default for TaskNotesConfig {
    fn default() -> Self {
        Self {
            tasks_folder: "TaskNotes/Tasks".to_string(),
            archive_folder: "TaskNotes/Archive".to_string(),
            task_tag: "task".to_string(),
            identification_method: TaskNotesIdentificationMethod::Tag,
            task_property_name: None,
            task_property_value: None,
            excluded_folders: Vec::new(),
            default_status: "open".to_string(),
            default_priority: "normal".to_string(),
            field_mapping: TaskNotesFieldMapping::default(),
            statuses: default_tasknotes_statuses(),
            priorities: default_tasknotes_priorities(),
            user_fields: Vec::new(),
            enable_natural_language_input: true,
            nlp_default_to_scheduled: false,
            nlp_language: default_tasknotes_nlp_language(),
            nlp_triggers: default_tasknotes_nlp_triggers(),
            pomodoro: TaskNotesPomodoroConfig::default(),
            task_creation_defaults: TaskNotesTaskCreationDefaults::default(),
            saved_views: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KanbanMetadataKeyConfig {
    Detailed(KanbanMetadataFieldConfig),
    Key(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KanbanMetadataFieldConfig {
    #[serde(alias = "metadataKey")]
    pub metadata_key: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default, alias = "shouldHideLabel")]
    pub should_hide_label: bool,
    #[serde(default, alias = "containsMarkdown")]
    pub contains_markdown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KanbanTagColorConfig {
    #[serde(alias = "tagKey")]
    pub tag_key: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default, alias = "backgroundColor")]
    pub background_color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KanbanTagSortConfig {
    pub tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KanbanDateColorConfig {
    #[serde(default, alias = "isToday")]
    pub is_today: Option<bool>,
    #[serde(default, alias = "isBefore")]
    pub is_before: Option<bool>,
    #[serde(default, alias = "isAfter")]
    pub is_after: Option<bool>,
    #[serde(default)]
    pub distance: Option<usize>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default, alias = "backgroundColor")]
    pub background_color: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KanbanConfig {
    #[serde(default = "default_kanban_date_trigger")]
    pub date_trigger: String,
    #[serde(default = "default_kanban_time_trigger")]
    pub time_trigger: String,
    #[serde(default = "default_kanban_date_format")]
    pub date_format: String,
    #[serde(default = "default_kanban_time_format")]
    pub time_format: String,
    #[serde(default)]
    pub date_display_format: Option<String>,
    #[serde(default)]
    pub date_time_display_format: Option<String>,
    #[serde(default)]
    pub link_date_to_daily_note: bool,
    #[serde(default)]
    pub metadata_keys: Vec<KanbanMetadataKeyConfig>,
    #[serde(default)]
    pub archive_with_date: bool,
    #[serde(default)]
    pub append_archive_date: bool,
    #[serde(default = "default_kanban_archive_date_format")]
    pub archive_date_format: String,
    #[serde(default)]
    pub archive_date_separator: Option<String>,
    #[serde(default = "default_kanban_new_card_insertion_method")]
    pub new_card_insertion_method: String,
    #[serde(default)]
    pub new_line_trigger: Option<String>,
    #[serde(default)]
    pub new_note_folder: Option<String>,
    #[serde(default)]
    pub new_note_template: Option<String>,
    #[serde(default)]
    pub hide_card_count: bool,
    #[serde(default)]
    pub hide_tags_in_title: bool,
    #[serde(default)]
    pub hide_tags_display: bool,
    #[serde(default)]
    pub inline_metadata_position: Option<String>,
    #[serde(default)]
    pub lane_width: Option<usize>,
    #[serde(default)]
    pub full_list_lane_width: Option<bool>,
    #[serde(default)]
    pub list_collapse: Vec<bool>,
    #[serde(default)]
    pub max_archive_size: Option<usize>,
    #[serde(default)]
    pub show_checkboxes: bool,
    #[serde(default)]
    pub move_dates: Option<bool>,
    #[serde(default)]
    pub move_tags: Option<bool>,
    #[serde(default)]
    pub move_task_metadata: Option<bool>,
    #[serde(default)]
    pub show_add_list: Option<bool>,
    #[serde(default)]
    pub show_archive_all: Option<bool>,
    #[serde(default)]
    pub show_board_settings: Option<bool>,
    #[serde(default)]
    pub show_relative_date: Option<bool>,
    #[serde(default)]
    pub show_search: Option<bool>,
    #[serde(default)]
    pub show_set_view: Option<bool>,
    #[serde(default)]
    pub show_view_as_markdown: Option<bool>,
    #[serde(default)]
    pub date_picker_week_start: Option<usize>,
    #[serde(default)]
    pub table_sizing: BTreeMap<String, usize>,
    #[serde(default)]
    pub tag_action: Option<String>,
    #[serde(default)]
    pub tag_colors: Vec<KanbanTagColorConfig>,
    #[serde(default)]
    pub tag_sort: Vec<KanbanTagSortConfig>,
    #[serde(default)]
    pub date_colors: Vec<KanbanDateColorConfig>,
}

impl Default for KanbanConfig {
    fn default() -> Self {
        Self {
            date_trigger: default_kanban_date_trigger(),
            time_trigger: default_kanban_time_trigger(),
            date_format: default_kanban_date_format(),
            time_format: default_kanban_time_format(),
            date_display_format: None,
            date_time_display_format: None,
            link_date_to_daily_note: false,
            metadata_keys: Vec::new(),
            archive_with_date: false,
            append_archive_date: false,
            archive_date_format: default_kanban_archive_date_format(),
            archive_date_separator: None,
            new_card_insertion_method: default_kanban_new_card_insertion_method(),
            new_line_trigger: None,
            new_note_folder: None,
            new_note_template: None,
            hide_card_count: false,
            hide_tags_in_title: false,
            hide_tags_display: false,
            inline_metadata_position: None,
            lane_width: None,
            full_list_lane_width: None,
            list_collapse: Vec::new(),
            max_archive_size: None,
            show_checkboxes: false,
            move_dates: None,
            move_tags: None,
            move_task_metadata: None,
            show_add_list: None,
            show_archive_all: None,
            show_board_settings: None,
            show_relative_date: None,
            show_search: None,
            show_set_view: None,
            show_view_as_markdown: None,
            date_picker_week_start: None,
            table_sizing: BTreeMap::new(),
            tag_action: None,
            tag_colors: Vec::new(),
            tag_sort: Vec::new(),
            date_colors: Vec::new(),
        }
    }
}

fn default_kanban_date_trigger() -> String {
    "@".to_string()
}

fn default_kanban_time_trigger() -> String {
    "@@".to_string()
}

fn default_kanban_date_format() -> String {
    "YYYY-MM-DD".to_string()
}

fn default_kanban_time_format() -> String {
    "HH:mm".to_string()
}

fn default_kanban_archive_date_format() -> String {
    derived_kanban_archive_date_format(&default_kanban_date_format(), &default_kanban_time_format())
}

fn default_kanban_new_card_insertion_method() -> String {
    "append".to_string()
}

fn default_templater_command_timeout() -> usize {
    5
}

fn default_templater_enable_folder_templates() -> bool {
    true
}

fn default_templater_syntax_highlighting() -> bool {
    true
}

fn default_templater_intellisense_render() -> usize {
    1
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataviewConfig {
    #[serde(default = "default_dataview_inline_query_prefix")]
    pub inline_query_prefix: String,
    #[serde(default = "default_dataview_inline_js_query_prefix")]
    pub inline_js_query_prefix: String,
    #[serde(default = "default_dataview_enable_dataview_js")]
    pub enable_dataview_js: bool,
    #[serde(default = "default_dataview_enable_inline_dataview_js")]
    pub enable_inline_dataview_js: bool,
    #[serde(default = "default_dataview_task_completion_tracking")]
    pub task_completion_tracking: bool,
    #[serde(default = "default_dataview_task_completion_use_emoji_shorthand")]
    pub task_completion_use_emoji_shorthand: bool,
    #[serde(default = "default_dataview_task_completion_text")]
    pub task_completion_text: String,
    #[serde(default = "default_dataview_recursive_subtask_completion")]
    pub recursive_subtask_completion: bool,
    #[serde(default = "default_dataview_display_result_count")]
    pub display_result_count: bool,
    #[serde(default = "default_dataview_default_date_format")]
    pub default_date_format: String,
    #[serde(default = "default_dataview_default_datetime_format")]
    pub default_datetime_format: String,
    pub timezone: Option<String>,
    #[serde(default = "default_dataview_max_recursive_render_depth")]
    pub max_recursive_render_depth: usize,
    #[serde(default = "default_dataview_primary_column_name")]
    pub primary_column_name: String,
    #[serde(default = "default_dataview_group_column_name")]
    pub group_column_name: String,
    #[serde(default = "default_dataview_js_timeout_seconds")]
    pub js_timeout_seconds: usize,
    #[serde(default = "default_dataview_js_memory_limit_bytes")]
    pub js_memory_limit_bytes: usize,
    #[serde(default = "default_dataview_js_max_stack_size_bytes")]
    pub js_max_stack_size_bytes: usize,
}

impl Default for DataviewConfig {
    fn default() -> Self {
        Self {
            inline_query_prefix: default_dataview_inline_query_prefix(),
            inline_js_query_prefix: default_dataview_inline_js_query_prefix(),
            enable_dataview_js: default_dataview_enable_dataview_js(),
            enable_inline_dataview_js: default_dataview_enable_inline_dataview_js(),
            task_completion_tracking: default_dataview_task_completion_tracking(),
            task_completion_use_emoji_shorthand:
                default_dataview_task_completion_use_emoji_shorthand(),
            task_completion_text: default_dataview_task_completion_text(),
            recursive_subtask_completion: default_dataview_recursive_subtask_completion(),
            display_result_count: default_dataview_display_result_count(),
            default_date_format: default_dataview_default_date_format(),
            default_datetime_format: default_dataview_default_datetime_format(),
            timezone: None,
            max_recursive_render_depth: default_dataview_max_recursive_render_depth(),
            primary_column_name: default_dataview_primary_column_name(),
            group_column_name: default_dataview_group_column_name(),
            js_timeout_seconds: default_dataview_js_timeout_seconds(),
            js_memory_limit_bytes: default_dataview_js_memory_limit_bytes(),
            js_max_stack_size_bytes: default_dataview_js_max_stack_size_bytes(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeriodicStartOfWeek {
    #[default]
    Monday,
    Sunday,
    Saturday,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeriodicCadenceUnit {
    #[serde(alias = "day")]
    Days,
    #[serde(alias = "week")]
    Weeks,
    #[serde(alias = "month")]
    Months,
    #[serde(alias = "quarter")]
    Quarters,
    #[serde(alias = "year")]
    Years,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodicNoteConfig {
    #[serde(default = "default_periodic_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub folder: PathBuf,
    #[serde(default = "default_periodic_format")]
    pub format: String,
    #[serde(default)]
    pub unit: Option<PeriodicCadenceUnit>,
    #[serde(default = "default_periodic_interval")]
    pub interval: usize,
    #[serde(default)]
    pub anchor_date: Option<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub start_of_week: PeriodicStartOfWeek,
    #[serde(default)]
    pub schedule_heading: Option<String>,
}

impl PeriodicNoteConfig {
    #[must_use]
    pub fn built_in(name: &str) -> Self {
        match name {
            "daily" => Self {
                enabled: true,
                folder: PathBuf::from("Journal/Daily"),
                format: "YYYY-MM-DD".to_string(),
                unit: Some(PeriodicCadenceUnit::Days),
                interval: 1,
                anchor_date: None,
                template: Some("daily".to_string()),
                start_of_week: PeriodicStartOfWeek::Monday,
                schedule_heading: None,
            },
            "weekly" => Self {
                enabled: true,
                folder: PathBuf::from("Journal/Weekly"),
                format: "YYYY-[W]ww".to_string(),
                unit: Some(PeriodicCadenceUnit::Weeks),
                interval: 1,
                anchor_date: None,
                template: Some("weekly".to_string()),
                start_of_week: PeriodicStartOfWeek::Monday,
                schedule_heading: None,
            },
            "monthly" => Self {
                enabled: true,
                folder: PathBuf::from("Journal/Monthly"),
                format: "YYYY-MM".to_string(),
                unit: Some(PeriodicCadenceUnit::Months),
                interval: 1,
                anchor_date: None,
                template: Some("monthly".to_string()),
                start_of_week: PeriodicStartOfWeek::Monday,
                schedule_heading: None,
            },
            "quarterly" => Self {
                enabled: false,
                folder: PathBuf::from("Journal/Quarterly"),
                format: "YYYY-[Q]Q".to_string(),
                unit: Some(PeriodicCadenceUnit::Quarters),
                interval: 1,
                anchor_date: None,
                template: Some("quarterly".to_string()),
                start_of_week: PeriodicStartOfWeek::Monday,
                schedule_heading: None,
            },
            "yearly" => Self {
                enabled: false,
                folder: PathBuf::from("Journal/Yearly"),
                format: "YYYY".to_string(),
                unit: Some(PeriodicCadenceUnit::Years),
                interval: 1,
                anchor_date: None,
                template: Some("yearly".to_string()),
                start_of_week: PeriodicStartOfWeek::Monday,
                schedule_heading: None,
            },
            _ => Self {
                enabled: false,
                folder: PathBuf::new(),
                format: default_periodic_format(),
                unit: None,
                interval: default_periodic_interval(),
                anchor_date: None,
                template: None,
                start_of_week: PeriodicStartOfWeek::Monday,
                schedule_heading: None,
            },
        }
    }
}

impl Default for PeriodicNoteConfig {
    fn default() -> Self {
        Self::built_in("daily")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodicConfig {
    #[serde(flatten)]
    pub notes: BTreeMap<String, PeriodicNoteConfig>,
}

impl PeriodicConfig {
    #[must_use]
    pub fn note(&self, period_type: &str) -> Option<&PeriodicNoteConfig> {
        self.notes.get(period_type)
    }

    pub fn note_mut(&mut self, period_type: &str) -> &mut PeriodicNoteConfig {
        self.notes
            .entry(period_type.to_string())
            .or_insert_with(|| PeriodicNoteConfig::built_in(period_type))
    }
}

impl Default for PeriodicConfig {
    fn default() -> Self {
        let mut notes = BTreeMap::new();
        for name in ["daily", "weekly", "monthly", "quarterly", "yearly"] {
            notes.insert(name.to_string(), PeriodicNoteConfig::built_in(name));
        }
        Self { notes }
    }
}

fn default_periodic_enabled() -> bool {
    true
}

fn default_periodic_format() -> String {
    "YYYY-MM-DD".to_string()
}

fn default_periodic_interval() -> usize {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanConfig {
    pub default_mode: AutoScanMode,
    pub browse_mode: AutoScanMode,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            default_mode: AutoScanMode::Blocking,
            browse_mode: AutoScanMode::Background,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathPermissionKeyword {
    All,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PathPermissionRules {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PathPermissionConfig {
    Keyword(PathPermissionKeyword),
    Rules(PathPermissionRules),
}

impl Default for PathPermissionConfig {
    fn default() -> Self {
        Self::Keyword(PathPermissionKeyword::None)
    }
}

impl PathPermissionConfig {
    #[must_use]
    pub fn is_all(&self) -> bool {
        matches!(self, Self::Keyword(PathPermissionKeyword::All))
    }

    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, Self::Keyword(PathPermissionKeyword::None))
    }

    #[must_use]
    pub fn is_scoped(&self) -> bool {
        matches!(self, Self::Rules(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    Allow,
    #[default]
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConfigPermissionMode {
    Read,
    Write,
    #[default]
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NetworkPermissionDetails {
    pub allow: bool,
    #[serde(default)]
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NetworkPermissionConfig {
    Mode(PermissionMode),
    Details(NetworkPermissionDetails),
}

impl Default for NetworkPermissionConfig {
    fn default() -> Self {
        Self::Mode(PermissionMode::Deny)
    }
}

impl NetworkPermissionConfig {
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        match self {
            Self::Mode(mode) => matches!(mode, PermissionMode::Allow),
            Self::Details(details) => details.allow,
        }
    }

    #[must_use]
    pub fn domain_allowlist(&self) -> &[String] {
        match self {
            Self::Mode(_) => &[],
            Self::Details(details) => &details.domains,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionLimitKeyword {
    Unlimited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionLimit {
    Value(usize),
    Keyword(PermissionLimitKeyword),
}

impl Default for PermissionLimit {
    fn default() -> Self {
        Self::Keyword(PermissionLimitKeyword::Unlimited)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionProfile {
    #[serde(default)]
    pub read: PathPermissionConfig,
    #[serde(default)]
    pub write: PathPermissionConfig,
    #[serde(default)]
    pub refactor: PathPermissionConfig,
    #[serde(default)]
    pub git: PermissionMode,
    #[serde(default)]
    pub network: NetworkPermissionConfig,
    #[serde(default)]
    pub index: PermissionMode,
    #[serde(default)]
    pub config: ConfigPermissionMode,
    #[serde(default)]
    pub execute: PermissionMode,
    #[serde(default)]
    pub shell: PermissionMode,
    #[serde(default)]
    pub cpu_limit_ms: PermissionLimit,
    #[serde(default)]
    pub memory_limit_mb: PermissionLimit,
    #[serde(default)]
    pub stack_limit_kb: PermissionLimit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_hook: Option<PathBuf>,
}

impl Default for PermissionProfile {
    fn default() -> Self {
        Self {
            read: PathPermissionConfig::Keyword(PathPermissionKeyword::None),
            write: PathPermissionConfig::Keyword(PathPermissionKeyword::None),
            refactor: PathPermissionConfig::Keyword(PathPermissionKeyword::None),
            git: PermissionMode::Deny,
            network: NetworkPermissionConfig::Mode(PermissionMode::Deny),
            index: PermissionMode::Deny,
            config: ConfigPermissionMode::None,
            execute: PermissionMode::Deny,
            shell: PermissionMode::Deny,
            cpu_limit_ms: PermissionLimit::Keyword(PermissionLimitKeyword::Unlimited),
            memory_limit_mb: PermissionLimit::Keyword(PermissionLimitKeyword::Unlimited),
            stack_limit_kb: PermissionLimit::Keyword(PermissionLimitKeyword::Unlimited),
            policy_hook: None,
        }
    }
}

impl PermissionProfile {
    #[must_use]
    pub fn unrestricted() -> Self {
        Self {
            read: PathPermissionConfig::Keyword(PathPermissionKeyword::All),
            write: PathPermissionConfig::Keyword(PathPermissionKeyword::All),
            refactor: PathPermissionConfig::Keyword(PathPermissionKeyword::All),
            git: PermissionMode::Allow,
            network: NetworkPermissionConfig::Mode(PermissionMode::Allow),
            index: PermissionMode::Allow,
            config: ConfigPermissionMode::Write,
            execute: PermissionMode::Allow,
            shell: PermissionMode::Allow,
            cpu_limit_ms: PermissionLimit::Keyword(PermissionLimitKeyword::Unlimited),
            memory_limit_mb: PermissionLimit::Keyword(PermissionLimitKeyword::Unlimited),
            stack_limit_kb: PermissionLimit::Keyword(PermissionLimitKeyword::Unlimited),
            policy_hook: None,
        }
    }

    #[must_use]
    pub fn readonly() -> Self {
        Self {
            read: PathPermissionConfig::Keyword(PathPermissionKeyword::All),
            config: ConfigPermissionMode::Read,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn daily_wiki_agent() -> Self {
        Self {
            read: PathPermissionConfig::Keyword(PathPermissionKeyword::All),
            write: PathPermissionConfig::Keyword(PathPermissionKeyword::All),
            index: PermissionMode::Allow,
            config: ConfigPermissionMode::Read,
            cpu_limit_ms: PermissionLimit::Value(5000),
            memory_limit_mb: PermissionLimit::Value(64),
            stack_limit_kb: PermissionLimit::Value(256),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn is_unrestricted(&self) -> bool {
        self == &Self::unrestricted()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PermissionsConfig {
    #[serde(default)]
    pub profiles: BTreeMap<String, PermissionProfile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExportProfileFormat {
    Markdown,
    Json,
    Csv,
    Graph,
    Epub,
    Zip,
    Sqlite,
    SearchIndex,
    FrontendBundle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportGraphFormatConfig {
    Json,
    Dot,
    Graphml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportEpubTocStyleConfig {
    Tree,
    Flat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExportConfig {
    #[serde(default)]
    pub profiles: BTreeMap<String, ExportProfileConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExportProfileConfig {
    pub format: Option<ExportProfileFormat>,
    pub query: Option<String>,
    pub query_json: Option<String>,
    pub path: Option<PathBuf>,
    pub site_profile: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub toc: Option<ExportEpubTocStyleConfig>,
    pub backlinks: Option<bool>,
    pub frontmatter: Option<bool>,
    pub pretty: Option<bool>,
    pub graph_format: Option<ExportGraphFormatConfig>,
    #[serde(
        default,
        rename = "content_transforms",
        skip_serializing_if = "Option::is_none"
    )]
    pub content_transform_rules: Option<Vec<ContentTransformRuleConfig>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SiteLinkPolicyConfig {
    Error,
    #[default]
    Warn,
    DropLink,
    RenderPlainText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SiteAssetPolicyModeConfig {
    #[default]
    CopyReferenced,
    ErrorOnMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SiteDataviewJsPolicyConfig {
    #[default]
    Off,
    Static,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SiteRawHtmlPolicyConfig {
    #[default]
    Passthrough,
    Sanitize,
    Strip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SitePaletteModeConfig {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SiteFolderClickBehaviorConfig {
    Collapse,
    #[default]
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SiteExplorerFolderStateConfig {
    #[default]
    Collapsed,
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SiteShellOptionsConfig {
    pub reader_mode: Option<bool>,
    pub default_palette: Option<SitePaletteModeConfig>,
    pub left_rail: Option<bool>,
    pub right_rail: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SiteNavigationOptionsConfig {
    pub explorer: Option<bool>,
    pub folder_click: Option<SiteFolderClickBehaviorConfig>,
    pub default_folder_state: Option<SiteExplorerFolderStateConfig>,
    pub use_saved_state: Option<bool>,
    pub show_home: Option<bool>,
    pub show_recent: Option<bool>,
    pub show_folders: Option<bool>,
    pub show_tags: Option<bool>,
    pub show_graph: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SiteModulesOptionsConfig {
    pub toc: Option<bool>,
    pub graph: Option<bool>,
    pub backlinks: Option<bool>,
    pub outgoing_links: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteAssetPolicyConfig {
    #[serde(default)]
    pub mode: SiteAssetPolicyModeConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_folders: Vec<String>,
}

impl Default for SiteAssetPolicyConfig {
    fn default() -> Self {
        Self {
            mode: SiteAssetPolicyModeConfig::CopyReferenced,
            include_folders: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SiteConfig {
    #[serde(default)]
    pub profiles: BTreeMap<String, SiteProfileConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SiteProfileConfig {
    pub title: Option<String>,
    pub page_title_template: Option<String>,
    pub base_url: Option<String>,
    pub deploy_path: Option<String>,
    pub output_dir: Option<PathBuf>,
    pub home: Option<String>,
    pub language: Option<String>,
    pub theme: Option<String>,
    pub search: Option<bool>,
    pub graph: Option<bool>,
    pub backlinks: Option<bool>,
    pub rss: Option<bool>,
    #[serde(default)]
    pub shell: SiteShellOptionsConfig,
    #[serde(default)]
    pub navigation: SiteNavigationOptionsConfig,
    #[serde(default)]
    pub modules: SiteModulesOptionsConfig,
    pub favicon: Option<PathBuf>,
    pub logo: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_css: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_js: Vec<PathBuf>,
    pub include_query: Option<String>,
    pub include_query_json: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_folders: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_folders: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_tags: Vec<String>,
    pub link_policy: Option<SiteLinkPolicyConfig>,
    #[serde(default)]
    pub asset_policy: SiteAssetPolicyConfig,
    pub dataview_js: Option<SiteDataviewJsPolicyConfig>,
    pub raw_html: Option<SiteRawHtmlPolicyConfig>,
    #[serde(
        default,
        rename = "content_transforms",
        skip_serializing_if = "Option::is_none"
    )]
    pub content_transform_rules: Option<Vec<ContentTransformRuleConfig>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultConfig {
    pub scan: ScanConfig,
    pub chunking: ChunkingConfig,
    pub link_resolution: LinkResolutionMode,
    pub link_style: LinkStylePreference,
    pub attachment_folder: PathBuf,
    pub strict_line_breaks: bool,
    pub property_types: BTreeMap<String, String>,
    pub embedding: Option<EmbeddingProviderConfig>,
    pub extraction: Option<AttachmentExtractionConfig>,
    pub git: GitConfig,
    pub inbox: InboxConfig,
    pub tasks: TasksConfig,
    pub tasknotes: TaskNotesConfig,
    pub kanban: KanbanConfig,
    pub dataview: DataviewConfig,
    pub js_runtime: JsRuntimeConfig,
    pub templates: TemplatesConfig,
    pub quickadd: QuickAddConfig,
    #[serde(default)]
    pub assistant: AssistantConfig,
    pub web: WebConfig,
    pub periodic: PeriodicConfig,
    pub export: ExportConfig,
    #[serde(default)]
    pub site: SiteConfig,
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginRegistration>,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            scan: ScanConfig::default(),
            chunking: ChunkingConfig::default(),
            link_resolution: LinkResolutionMode::Shortest,
            link_style: LinkStylePreference::Wikilink,
            attachment_folder: PathBuf::from("."),
            strict_line_breaks: false,
            property_types: BTreeMap::new(),
            embedding: None,
            extraction: None,
            git: GitConfig::default(),
            inbox: InboxConfig::default(),
            tasks: TasksConfig::default(),
            tasknotes: TaskNotesConfig::default(),
            kanban: KanbanConfig::default(),
            dataview: DataviewConfig::default(),
            js_runtime: JsRuntimeConfig::default(),
            templates: TemplatesConfig::default(),
            quickadd: QuickAddConfig::default(),
            assistant: AssistantConfig::default(),
            web: WebConfig::default(),
            periodic: PeriodicConfig::default(),
            export: ExportConfig::default(),
            site: SiteConfig::default(),
            plugins: BTreeMap::new(),
            aliases: builtin_command_aliases(),
        }
    }
}

fn builtin_command_aliases() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("q".to_string(), "query".to_string()),
        ("t".to_string(), "tasks list".to_string()),
        ("today".to_string(), "daily today".to_string()),
    ])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDiagnostic {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLoadResult {
    pub config: VaultConfig,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionProfilesLoadResult {
    pub profiles: BTreeMap<String, PermissionProfile>,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[must_use]
pub fn default_config_template() -> &'static str {
    DEFAULT_CONFIG_TEMPLATE
}

pub fn create_default_config(paths: &VaultPaths) -> Result<bool, std::io::Error> {
    ensure_vulcan_dir(paths)?;

    if paths.config_file().exists() {
        return Ok(false);
    }

    fs::write(paths.config_file(), default_config_template())?;
    Ok(true)
}

pub fn validate_vulcan_overrides_toml(contents: &str) -> Result<(), ConfigImportError> {
    toml::from_str::<PartialVulcanConfig>(contents)
        .map(|_| ())
        .map_err(ConfigImportError::from)
}

#[must_use]
pub fn load_vault_config(paths: &VaultPaths) -> ConfigLoadResult {
    let mut loaded = load_vault_config_base(paths);

    if let Some(vulcan_config) = load_vulcan_overrides(
        paths.config_file(),
        "Vulcan config",
        &mut loaded.diagnostics,
    ) {
        apply_vulcan_overrides(&mut loaded.config, vulcan_config);
    }

    if let Some(local_config) = load_vulcan_overrides(
        paths.local_config_file(),
        "local Vulcan config",
        &mut loaded.diagnostics,
    ) {
        apply_vulcan_overrides(&mut loaded.config, local_config);
    }

    loaded
}

#[must_use]
pub fn load_vault_config_with_overrides(
    paths: &VaultPaths,
    shared_override: Option<&TomlValue>,
    local_override: Option<&TomlValue>,
) -> ConfigLoadResult {
    let mut loaded = load_vault_config_base(paths);
    apply_vulcan_override_value(
        &mut loaded.config,
        shared_override,
        "Vulcan config",
        paths.config_file(),
        &mut loaded.diagnostics,
    );
    apply_vulcan_override_value(
        &mut loaded.config,
        local_override,
        "local Vulcan config",
        paths.local_config_file(),
        &mut loaded.diagnostics,
    );
    loaded
}

fn load_vault_config_base(paths: &VaultPaths) -> ConfigLoadResult {
    let mut config = VaultConfig::default();
    let mut diagnostics = Vec::new();

    if let Some(obsidian_app) = load_obsidian_app_config(paths, &mut diagnostics) {
        apply_obsidian_defaults(&mut config, obsidian_app);
    }

    if let Some(obsidian_templates) = load_obsidian_templates_config(paths, &mut diagnostics) {
        apply_obsidian_template_defaults(&mut config, obsidian_templates);
    }

    if let Some(obsidian_daily_notes) = load_obsidian_daily_notes_config(paths, &mut diagnostics) {
        apply_obsidian_daily_notes_defaults(&mut config, obsidian_daily_notes);
    }

    if let Some(obsidian_periodic_notes) =
        load_obsidian_periodic_notes_config(paths, &mut diagnostics)
    {
        apply_obsidian_periodic_notes_defaults(&mut config, obsidian_periodic_notes);
    }

    if let Some(obsidian_templater) = load_obsidian_templater_config(paths, &mut diagnostics) {
        apply_obsidian_templater_defaults(&mut config, obsidian_templater);
    }

    if let Some(obsidian_quickadd) = load_obsidian_quickadd_config(paths, &mut diagnostics) {
        apply_obsidian_quickadd_defaults(&mut config, obsidian_quickadd);
    }

    if let Some(obsidian_dataview) = load_obsidian_dataview_config(paths, &mut diagnostics) {
        apply_obsidian_dataview_defaults(&mut config, obsidian_dataview);
    }

    if let Some(obsidian_tasks) = load_obsidian_tasks_config(paths, &mut diagnostics) {
        apply_obsidian_tasks_defaults(&mut config, obsidian_tasks);
    }

    if let Some(obsidian_tasknotes) = load_obsidian_tasknotes_config(paths, &mut diagnostics) {
        apply_obsidian_tasknotes_defaults(&mut config, obsidian_tasknotes);
    }

    if let Some(obsidian_kanban) = load_obsidian_kanban_config(paths, &mut diagnostics) {
        apply_obsidian_kanban_defaults(&mut config, obsidian_kanban);
    }

    config.property_types = load_obsidian_property_types(paths, &mut diagnostics);

    ConfigLoadResult {
        config,
        diagnostics,
    }
}

#[must_use]
pub fn builtin_permission_profiles() -> BTreeMap<String, PermissionProfile> {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "daily-wiki-agent".to_string(),
        PermissionProfile::daily_wiki_agent(),
    );
    profiles.insert("readonly".to_string(), PermissionProfile::readonly());
    profiles.insert(
        "unrestricted".to_string(),
        PermissionProfile::unrestricted(),
    );
    profiles
}

#[must_use]
pub fn load_permission_profiles(paths: &VaultPaths) -> PermissionProfilesLoadResult {
    let mut loaded = load_permission_profiles_base();

    if let Some(vulcan_config) = load_vulcan_overrides(
        paths.config_file(),
        "Vulcan config",
        &mut loaded.diagnostics,
    ) {
        apply_permission_profile_overrides(&mut loaded.profiles, vulcan_config.permissions);
    }

    if let Some(local_config) = load_vulcan_overrides(
        paths.local_config_file(),
        "local Vulcan config",
        &mut loaded.diagnostics,
    ) {
        apply_permission_profile_overrides(&mut loaded.profiles, local_config.permissions);
    }

    loaded
}

#[must_use]
pub fn load_permission_profiles_with_overrides(
    paths: &VaultPaths,
    shared_override: Option<&TomlValue>,
    local_override: Option<&TomlValue>,
) -> PermissionProfilesLoadResult {
    let mut loaded = load_permission_profiles_base();
    apply_permission_profile_override_value(
        &mut loaded.profiles,
        shared_override,
        "Vulcan config",
        paths.config_file(),
        &mut loaded.diagnostics,
    );
    apply_permission_profile_override_value(
        &mut loaded.profiles,
        local_override,
        "local Vulcan config",
        paths.local_config_file(),
        &mut loaded.diagnostics,
    );
    loaded
}

fn load_permission_profiles_base() -> PermissionProfilesLoadResult {
    let profiles = builtin_permission_profiles();
    let diagnostics = Vec::new();

    PermissionProfilesLoadResult {
        profiles,
        diagnostics,
    }
}

fn apply_vulcan_override_value(
    config: &mut VaultConfig,
    override_value: Option<&TomlValue>,
    description: &str,
    path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) {
    let Some(parsed) =
        parse_in_memory_vulcan_override(override_value, description, path, diagnostics)
    else {
        return;
    };
    apply_vulcan_overrides(config, parsed);
}

fn apply_permission_profile_override_value(
    profiles: &mut BTreeMap<String, PermissionProfile>,
    override_value: Option<&TomlValue>,
    description: &str,
    path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) {
    let Some(parsed) =
        parse_in_memory_vulcan_override(override_value, description, path, diagnostics)
    else {
        return;
    };
    apply_permission_profile_overrides(profiles, parsed.permissions);
}

fn parse_in_memory_vulcan_override(
    override_value: Option<&TomlValue>,
    description: &str,
    path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<PartialVulcanConfig> {
    let override_value = override_value?;

    match toml::to_string(override_value) {
        Ok(rendered) => match toml::from_str::<PartialVulcanConfig>(&rendered) {
            Ok(config) => Some(config),
            Err(error) => {
                diagnostics.push(ConfigDiagnostic {
                    path: path.to_path_buf(),
                    message: format!("failed to parse {description}: {error}"),
                });
                None
            }
        },
        Err(error) => {
            diagnostics.push(ConfigDiagnostic {
                path: path.to_path_buf(),
                message: format!("failed to serialize {description}: {error}"),
            });
            None
        }
    }
}

fn load_obsidian_app_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianAppConfig> {
    let path = paths.vault_root().join(".obsidian/app.json");

    load_json_file(&path, diagnostics)
}

fn load_obsidian_property_types(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> BTreeMap<String, String> {
    let path = paths.vault_root().join(".obsidian/types.json");
    let Some(value) = load_json_file::<Value>(&path, diagnostics) else {
        return BTreeMap::new();
    };
    match parse_obsidian_property_types_value(value) {
        Ok(types) => types,
        Err(message) => {
            diagnostics.push(ConfigDiagnostic { path, message });
            BTreeMap::new()
        }
    }
}

fn load_obsidian_daily_notes_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianDailyNotesConfig> {
    let path = paths.vault_root().join(".obsidian/daily-notes.json");

    load_json_file(&path, diagnostics)
}

fn load_obsidian_periodic_notes_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianPeriodicNotesConfig> {
    let path = paths
        .vault_root()
        .join(".obsidian/plugins/periodic-notes/data.json");

    load_json_file(&path, diagnostics)
}

fn load_explicit_obsidian_property_types(
    path: &Path,
) -> Result<BTreeMap<String, String>, ConfigImportError> {
    let value = serde_json::from_str::<Value>(&fs::read_to_string(path)?)?;
    parse_obsidian_property_types_value(value).map_err(ConfigImportError::InvalidConfig)
}

fn parse_obsidian_property_types_value(value: Value) -> Result<BTreeMap<String, String>, String> {
    if let Value::Object(entries) = value {
        Ok(entries
            .into_iter()
            .filter_map(|(key, value)| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        value
                            .get("type")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                    .map(|value_type| (key, value_type))
            })
            .collect())
    } else {
        Err("expected a JSON object of property types".to_string())
    }
}

fn load_obsidian_templates_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianTemplatesConfig> {
    let path = paths.vault_root().join(".obsidian/templates.json");

    load_json_file(&path, diagnostics)
}

fn load_obsidian_templater_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianTemplaterConfig> {
    let path = paths
        .vault_root()
        .join(".obsidian/plugins/templater-obsidian/data.json");

    load_json_file(&path, diagnostics)
}

fn load_obsidian_quickadd_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianQuickAddConfig> {
    let path = paths
        .vault_root()
        .join(".obsidian/plugins/quickadd/data.json");

    load_json_file(&path, diagnostics)
}

fn load_obsidian_dataview_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianDataviewConfig> {
    let path = paths
        .vault_root()
        .join(".obsidian/plugins/dataview/data.json");

    load_json_file(&path, diagnostics)
}

fn load_obsidian_tasks_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianTasksConfig> {
    let path = paths
        .vault_root()
        .join(".obsidian/plugins/obsidian-tasks-plugin/data.json");

    load_json_file(&path, diagnostics)
}

fn load_obsidian_tasknotes_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianTaskNotesConfig> {
    let path = paths
        .vault_root()
        .join(".obsidian/plugins/tasknotes/data.json");

    load_json_file(&path, diagnostics)
}

fn load_obsidian_kanban_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianKanbanConfig> {
    let path = paths
        .vault_root()
        .join(".obsidian/plugins/obsidian-kanban/data.json");

    load_json_file(&path, diagnostics)
}

fn load_vulcan_overrides(
    path: &Path,
    description: &str,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<PartialVulcanConfig> {
    if !path.exists() {
        return None;
    }

    match fs::read_to_string(path) {
        Ok(contents) => match toml::from_str::<PartialVulcanConfig>(&contents) {
            Ok(config) => Some(config),
            Err(error) => {
                diagnostics.push(ConfigDiagnostic {
                    path: path.to_path_buf(),
                    message: format!("failed to parse {description}: {error}"),
                });
                None
            }
        },
        Err(error) => {
            diagnostics.push(ConfigDiagnostic {
                path: path.to_path_buf(),
                message: format!("failed to read {description}: {error}"),
            });
            None
        }
    }
}

fn load_json_file<T: serde::de::DeserializeOwned>(
    path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<T> {
    if !path.exists() {
        return None;
    }

    match fs::read_to_string(path) {
        Ok(contents) => match serde_json::from_str::<T>(&contents) {
            Ok(value) => Some(value),
            Err(error) => {
                diagnostics.push(ConfigDiagnostic {
                    path: path.to_path_buf(),
                    message: format!("failed to parse JSON config: {error}"),
                });
                None
            }
        },
        Err(error) => {
            diagnostics.push(ConfigDiagnostic {
                path: path.to_path_buf(),
                message: format!("failed to read config: {error}"),
            });
            None
        }
    }
}

fn apply_obsidian_defaults(config: &mut VaultConfig, obsidian: ObsidianAppConfig) {
    if let Some(use_markdown_links) = obsidian.use_markdown_links {
        config.link_style = if use_markdown_links {
            LinkStylePreference::Markdown
        } else {
            LinkStylePreference::Wikilink
        };
    }

    if let Some(link_format) = obsidian.new_link_format {
        config.link_resolution = link_format;
    }

    if let Some(attachment_folder) = obsidian.attachment_folder_path {
        config.attachment_folder = normalize_attachment_folder(&attachment_folder);
    }

    if let Some(strict_line_breaks) = obsidian.strict_line_breaks {
        config.strict_line_breaks = strict_line_breaks;
    }
}

fn apply_obsidian_template_defaults(config: &mut VaultConfig, obsidian: ObsidianTemplatesConfig) {
    if let Some(date_format) = obsidian.date_format {
        config.templates.date_format = date_format;
    }

    if let Some(time_format) = obsidian.time_format {
        config.templates.time_format = time_format;
    }

    if let Some(folder) = obsidian.folder {
        config.templates.obsidian_folder = normalize_template_path(Some(folder));
    }
}

fn apply_obsidian_daily_notes_defaults(
    config: &mut VaultConfig,
    obsidian: ObsidianDailyNotesConfig,
) {
    let daily = config.periodic.note_mut("daily");
    if let Some(folder) = obsidian.folder {
        daily.folder = normalize_periodic_folder(folder);
    }
    if let Some(format) = normalize_optional_text(obsidian.format) {
        daily.format = format;
    }
    if let Some(template) = normalize_optional_text(obsidian.template) {
        daily.template = Some(template);
    }
}

fn apply_obsidian_periodic_notes_defaults(
    config: &mut VaultConfig,
    obsidian: ObsidianPeriodicNotesConfig,
) {
    apply_obsidian_periodic_note_defaults(config.periodic.note_mut("daily"), obsidian.daily);
    apply_obsidian_periodic_note_defaults(config.periodic.note_mut("weekly"), obsidian.weekly);
    apply_obsidian_periodic_note_defaults(config.periodic.note_mut("monthly"), obsidian.monthly);
    apply_obsidian_periodic_note_defaults(
        config.periodic.note_mut("quarterly"),
        obsidian.quarterly,
    );
    apply_obsidian_periodic_note_defaults(config.periodic.note_mut("yearly"), obsidian.yearly);
}

fn apply_obsidian_periodic_note_defaults(
    target: &mut PeriodicNoteConfig,
    settings: Option<ObsidianPeriodicNoteSettings>,
) {
    let Some(settings) = settings else {
        return;
    };

    if let Some(enabled) = settings.enabled {
        target.enabled = enabled;
    }
    if let Some(folder) = settings.folder {
        target.folder = normalize_periodic_folder(folder);
    }
    if let Some(format) = normalize_optional_text(settings.format) {
        target.format = format;
    }
    if let Some(template) = normalize_optional_text(settings.template_path) {
        target.template = Some(template);
    }
    if let Some(start_of_week) = settings.start_of_week {
        target.start_of_week = start_of_week;
    }
}

fn apply_obsidian_templater_defaults(config: &mut VaultConfig, obsidian: ObsidianTemplaterConfig) {
    if let Some(folder) = obsidian.templates_folder {
        config.templates.templater_folder = normalize_template_path(Some(folder));
    }
    if let Some(command_timeout) = obsidian.command_timeout {
        config.templates.command_timeout = command_timeout;
    }
    config.templates.templates_pairs = normalize_templater_command_pairs(obsidian.templates_pairs);
    if let Some(trigger_on_file_creation) = obsidian.trigger_on_file_creation {
        config.templates.trigger_on_file_creation = trigger_on_file_creation;
    }
    if let Some(auto_jump_to_cursor) = obsidian.auto_jump_to_cursor {
        config.templates.auto_jump_to_cursor = auto_jump_to_cursor;
    }
    if let Some(enable_system_commands) = obsidian.enable_system_commands {
        config.templates.enable_system_commands = enable_system_commands;
    }
    if let Some(shell_path) = obsidian.shell_path {
        config.templates.shell_path = normalize_filesystem_path(Some(shell_path));
    }
    if let Some(user_scripts_folder) = obsidian.user_scripts_folder {
        config.templates.user_scripts_folder = normalize_template_path(Some(user_scripts_folder));
    }
    if let Some(enable_folder_templates) = obsidian.enable_folder_templates {
        config.templates.enable_folder_templates = enable_folder_templates;
    }
    config.templates.folder_templates =
        normalize_templater_folder_templates(obsidian.folder_templates);
    if let Some(enable_file_templates) = obsidian.enable_file_templates {
        config.templates.enable_file_templates = enable_file_templates;
    }
    config.templates.file_templates = normalize_templater_file_templates(obsidian.file_templates);
    if let Some(syntax_highlighting) = obsidian.syntax_highlighting {
        config.templates.syntax_highlighting = syntax_highlighting;
    }
    if let Some(syntax_highlighting_mobile) = obsidian.syntax_highlighting_mobile {
        config.templates.syntax_highlighting_mobile = syntax_highlighting_mobile;
    }
    config.templates.enabled_templates_hotkeys =
        normalize_string_list(obsidian.enabled_templates_hotkeys);
    config.templates.startup_templates = normalize_string_list(obsidian.startup_templates);
    if let Some(intellisense_render) = obsidian.intellisense_render {
        config.templates.intellisense_render = intellisense_render;
    }
}

fn apply_obsidian_quickadd_defaults(config: &mut VaultConfig, obsidian: ObsidianQuickAddConfig) {
    if let Some(folder) = obsidian.template_folder_path {
        config.quickadd.template_folder = normalize_template_path(Some(folder));
    }
    config.quickadd.global_variables =
        normalize_quickadd_global_variables(obsidian.global_variables);
    config.quickadd.capture_choices = obsidian
        .choices
        .iter()
        .enumerate()
        .filter_map(|(index, choice)| quickadd_capture_choice_from_obsidian(choice, index))
        .collect();
    config.quickadd.template_choices = obsidian
        .choices
        .iter()
        .enumerate()
        .filter_map(|(index, choice)| quickadd_template_choice_from_obsidian(choice, index))
        .collect();
    config.quickadd.ai = obsidian.ai.and_then(normalize_quickadd_ai_config);
}

fn quickadd_capture_choice_from_obsidian(
    choice: &ObsidianQuickAddChoice,
    ordinal: usize,
) -> Option<QuickAddCaptureChoiceConfig> {
    if !quickadd_choice_type(choice).eq_ignore_ascii_case("Capture") {
        return None;
    }
    let (id, name) = quickadd_choice_identity(choice, ordinal, "capture");
    let format = choice
        .format
        .as_ref()
        .and_then(normalize_quickadd_format_value);
    let insert_after = choice.insert_after.as_ref().and_then(|insert_after| {
        let enabled = insert_after.enabled.unwrap_or(false);
        let heading = normalize_optional_text(insert_after.heading.clone())?;
        enabled.then_some(QuickAddInsertAfterConfig {
            heading,
            insert_at_end: insert_after.insert_at_end.unwrap_or(false),
            consider_subsections: insert_after.consider_subsections.unwrap_or(false),
            create_if_not_found: insert_after.create_if_not_found.unwrap_or(false),
            create_if_not_found_location: normalize_optional_text(
                insert_after.create_if_not_found_location.clone(),
            ),
            inline: insert_after.inline.unwrap_or(false),
            replace_existing: insert_after.replace_existing.unwrap_or(false),
            blank_line_after_match_mode: normalize_optional_text(
                insert_after.blank_line_after_match_mode.clone(),
            ),
        })
    });
    let new_line_capture_direction = choice
        .new_line_capture
        .as_ref()
        .and_then(|capture| capture.enabled.unwrap_or(false).then_some(capture))
        .and_then(|capture| normalize_optional_text(capture.direction.clone()));

    Some(QuickAddCaptureChoiceConfig {
        id,
        name,
        capture_to: normalize_optional_text(choice.capture_to.clone()),
        capture_to_active_file: choice.capture_to_active_file.unwrap_or(false),
        active_file_write_position: normalize_optional_text(
            choice.active_file_write_position.clone(),
        ),
        create_file_if_missing: QuickAddCreateFileConfig {
            enabled: choice
                .create_file_if_it_doesnt_exist
                .as_ref()
                .and_then(|create| create.enabled)
                .unwrap_or(false),
            create_with_template: choice
                .create_file_if_it_doesnt_exist
                .as_ref()
                .and_then(|create| create.create_with_template)
                .unwrap_or(false),
            template: choice
                .create_file_if_it_doesnt_exist
                .as_ref()
                .and_then(|create| normalize_optional_text(create.template.clone())),
        },
        format,
        use_selection_as_capture_value: choice.use_selection_as_capture_value,
        prepend: choice.prepend.unwrap_or(false),
        task: choice.task.unwrap_or(false),
        insert_after,
        new_line_capture_direction,
        open_file: choice.open_file.unwrap_or(false),
        templater_after_capture: choice
            .templater
            .as_ref()
            .and_then(|templater| normalize_optional_text(templater.after_capture.clone())),
    })
}

fn quickadd_template_choice_from_obsidian(
    choice: &ObsidianQuickAddChoice,
    ordinal: usize,
) -> Option<QuickAddTemplateChoiceConfig> {
    if !quickadd_choice_type(choice).eq_ignore_ascii_case("Template") {
        return None;
    }
    let (id, name) = quickadd_choice_identity(choice, ordinal, "template");
    let folder =
        choice
            .folder
            .as_ref()
            .map_or_else(QuickAddTemplateFolderConfig::default, |folder| {
                if folder.enabled.unwrap_or(false) {
                    QuickAddTemplateFolderConfig {
                        folders: normalize_quickadd_path_list(folder.folders.clone()),
                        choose_when_creating_note: folder
                            .choose_when_creating_note
                            .unwrap_or(false),
                        create_in_same_folder_as_active_file: folder
                            .create_in_same_folder_as_active_file
                            .unwrap_or(false),
                        choose_from_subfolders: folder.choose_from_subfolders.unwrap_or(false),
                    }
                } else {
                    QuickAddTemplateFolderConfig::default()
                }
            });

    Some(QuickAddTemplateChoiceConfig {
        id,
        name,
        template_path: normalize_template_path(choice.template_path.clone()),
        folder,
        file_name_format: choice
            .file_name_format
            .as_ref()
            .and_then(normalize_quickadd_format_value),
        open_file: choice.open_file.unwrap_or(false),
        file_exists_behavior: normalize_optional_text(choice.file_exists_behavior.clone()),
    })
}

fn apply_obsidian_dataview_defaults(config: &mut VaultConfig, obsidian: ObsidianDataviewConfig) {
    if let Some(prefix) = obsidian.inline_query_prefix {
        config.dataview.inline_query_prefix = prefix;
    }
    if let Some(prefix) = obsidian.inline_js_query_prefix {
        config.dataview.inline_js_query_prefix = prefix;
    }
    if let Some(enabled) = obsidian.enable_dataview_js {
        config.dataview.enable_dataview_js = enabled;
    }
    if let Some(enabled) = obsidian.enable_inline_dataview_js {
        config.dataview.enable_inline_dataview_js = enabled;
    }
    if let Some(tracking) = obsidian.task_completion_tracking {
        config.dataview.task_completion_tracking = tracking;
    }
    if let Some(use_emoji_shorthand) = obsidian.task_completion_use_emoji_shorthand {
        config.dataview.task_completion_use_emoji_shorthand = use_emoji_shorthand;
    }
    if let Some(text) = obsidian.task_completion_text {
        config.dataview.task_completion_text = text;
    }
    if let Some(recursive) = obsidian.recursive_subtask_completion {
        config.dataview.recursive_subtask_completion = recursive;
    }
    if let Some(display_result_count) = obsidian.display_result_count {
        config.dataview.display_result_count = display_result_count;
    }
    if let Some(format) = obsidian.default_date_format {
        config.dataview.default_date_format = format;
    }
    if let Some(format) = obsidian.default_datetime_format {
        config.dataview.default_datetime_format = format;
    }
    if let Some(timezone) = obsidian.timezone {
        config.dataview.timezone = Some(timezone);
    }
    if let Some(depth) = obsidian.max_recursive_render_depth {
        config.dataview.max_recursive_render_depth = depth;
    }
    if let Some(name) = obsidian.primary_column_name {
        config.dataview.primary_column_name = name;
    }
    if let Some(name) = obsidian.group_column_name {
        config.dataview.group_column_name = name;
    }
}

fn apply_obsidian_tasks_defaults(config: &mut VaultConfig, obsidian: ObsidianTasksConfig) {
    let recurrence_on_completion = normalize_obsidian_task_recurrence_mode(&obsidian);
    config.tasks.global_filter = normalize_optional_text(obsidian.global_filter);
    config.tasks.global_query = normalize_optional_text(obsidian.global_query);
    if let Some(remove_global_filter) = obsidian.remove_global_filter {
        config.tasks.remove_global_filter = remove_global_filter;
    }
    if let Some(set_created_date) = obsidian.set_created_date {
        config.tasks.set_created_date = set_created_date;
    }
    if let Some(recurrence_on_completion) = recurrence_on_completion {
        config.tasks.recurrence_on_completion = Some(recurrence_on_completion);
    }

    let Some(status_settings) = obsidian.status_settings else {
        return;
    };

    let mut definitions = status_settings.core_statuses;
    definitions.extend(status_settings.custom_statuses);
    if definitions.is_empty() {
        return;
    }

    apply_task_status_definitions(&mut config.tasks.statuses, definitions);
}

fn apply_obsidian_tasknotes_defaults(config: &mut VaultConfig, obsidian: ObsidianTaskNotesConfig) {
    if let Some(tasks_folder) = obsidian.tasks_folder {
        config.tasknotes.tasks_folder = tasks_folder;
    }
    if let Some(archive_folder) = obsidian.archive_folder {
        config.tasknotes.archive_folder = archive_folder;
    }
    if let Some(task_tag) = obsidian.task_tag {
        config.tasknotes.task_tag = task_tag;
    }
    if let Some(method) = obsidian.task_identification_method {
        config.tasknotes.identification_method = method;
    }
    config.tasknotes.task_property_name = normalize_optional_text(obsidian.task_property_name);
    config.tasknotes.task_property_value = normalize_optional_text(obsidian.task_property_value);
    if let Some(excluded_folders) = obsidian.excluded_folders {
        config.tasknotes.excluded_folders = normalize_comma_separated_paths(&excluded_folders);
    }
    if let Some(default_task_status) = obsidian.default_task_status {
        config.tasknotes.default_status = default_task_status;
    }
    if let Some(default_task_priority) = obsidian.default_task_priority {
        config.tasknotes.default_priority = default_task_priority;
    }
    if let Some(field_mapping) = obsidian.field_mapping {
        apply_obsidian_tasknotes_field_mapping(&mut config.tasknotes.field_mapping, field_mapping);
    }
    if !obsidian.custom_statuses.is_empty() {
        config.tasknotes.statuses = obsidian.custom_statuses;
    }
    if !obsidian.custom_priorities.is_empty() {
        config.tasknotes.priorities = obsidian.custom_priorities;
    }
    if !obsidian.user_fields.is_empty() {
        config.tasknotes.user_fields = obsidian.user_fields;
    }
    if let Some(enabled) = obsidian.enable_natural_language_input {
        config.tasknotes.enable_natural_language_input = enabled;
    }
    if let Some(default_to_scheduled) = obsidian.nlp_default_to_scheduled {
        config.tasknotes.nlp_default_to_scheduled = default_to_scheduled;
    }
    if let Some(language) = normalize_optional_text(obsidian.nlp_language) {
        config.tasknotes.nlp_language = language;
    }
    if let Some(nlp_triggers) = obsidian.nlp_triggers {
        if !nlp_triggers.triggers.is_empty() {
            config.tasknotes.nlp_triggers = nlp_triggers.triggers;
        }
    }
    if let Some(work_duration) = obsidian.pomodoro_work_duration {
        config.tasknotes.pomodoro.work_duration = work_duration;
    }
    if let Some(short_break_duration) = obsidian.pomodoro_short_break_duration {
        config.tasknotes.pomodoro.short_break = short_break_duration;
    }
    if let Some(long_break_duration) = obsidian.pomodoro_long_break_duration {
        config.tasknotes.pomodoro.long_break = long_break_duration;
    }
    if let Some(long_break_interval) = obsidian.pomodoro_long_break_interval {
        config.tasknotes.pomodoro.long_break_interval = long_break_interval;
    }
    if let Some(storage_location) = obsidian.pomodoro_storage_location {
        config.tasknotes.pomodoro.storage_location = storage_location;
    }
    if let Some(defaults) = obsidian.task_creation_defaults {
        apply_obsidian_tasknotes_creation_defaults(
            &mut config.tasknotes.task_creation_defaults,
            defaults,
        );
    }
    if !obsidian.saved_views.is_empty() {
        config.tasknotes.saved_views = obsidian.saved_views;
    }
}

fn apply_obsidian_tasknotes_field_mapping(
    mapping: &mut TaskNotesFieldMapping,
    obsidian: ObsidianTaskNotesFieldMapping,
) {
    if let Some(title) = obsidian.title {
        mapping.title = title;
    }
    if let Some(status) = obsidian.status {
        mapping.status = status;
    }
    if let Some(priority) = obsidian.priority {
        mapping.priority = priority;
    }
    if let Some(due) = obsidian.due {
        mapping.due = due;
    }
    if let Some(scheduled) = obsidian.scheduled {
        mapping.scheduled = scheduled;
    }
    if let Some(contexts) = obsidian.contexts {
        mapping.contexts = contexts;
    }
    if let Some(projects) = obsidian.projects {
        mapping.projects = projects;
    }
    if let Some(time_estimate) = obsidian.time_estimate {
        mapping.time_estimate = time_estimate;
    }
    if let Some(completed_date) = obsidian.completed_date {
        mapping.completed_date = completed_date;
    }
    if let Some(date_created) = obsidian.date_created {
        mapping.date_created = date_created;
    }
    if let Some(date_modified) = obsidian.date_modified {
        mapping.date_modified = date_modified;
    }
    if let Some(recurrence) = obsidian.recurrence {
        mapping.recurrence = recurrence;
    }
    if let Some(recurrence_anchor) = obsidian.recurrence_anchor {
        mapping.recurrence_anchor = recurrence_anchor;
    }
    if let Some(archive_tag) = obsidian.archive_tag {
        mapping.archive_tag = archive_tag;
    }
    if let Some(time_entries) = obsidian.time_entries {
        mapping.time_entries = time_entries;
    }
    if let Some(complete_instances) = obsidian.complete_instances {
        mapping.complete_instances = complete_instances;
    }
    if let Some(skipped_instances) = obsidian.skipped_instances {
        mapping.skipped_instances = skipped_instances;
    }
    if let Some(blocked_by) = obsidian.blocked_by {
        mapping.blocked_by = blocked_by;
    }
    if let Some(pomodoros) = obsidian.pomodoros {
        mapping.pomodoros = pomodoros;
    }
    if let Some(reminders) = obsidian.reminders {
        mapping.reminders = reminders;
    }
}

fn apply_obsidian_tasknotes_creation_defaults(
    defaults: &mut TaskNotesTaskCreationDefaults,
    obsidian: ObsidianTaskNotesCreationDefaults,
) {
    if let Some(default_contexts) = obsidian.default_contexts {
        defaults.default_contexts =
            normalize_string_list(default_contexts.split(',').map(ToOwned::to_owned).collect());
    }
    if let Some(default_tags) = obsidian.default_tags {
        defaults.default_tags =
            normalize_string_list(default_tags.split(',').map(ToOwned::to_owned).collect());
    }
    if let Some(default_projects) = obsidian.default_projects {
        defaults.default_projects =
            normalize_string_list(default_projects.split(',').map(ToOwned::to_owned).collect());
    }
    if let Some(default_time_estimate) = obsidian.default_time_estimate {
        defaults.default_time_estimate = Some(default_time_estimate);
    }
    if let Some(default_due_date) = obsidian.default_due_date {
        defaults.default_due_date = default_due_date;
    }
    if let Some(default_scheduled_date) = obsidian.default_scheduled_date {
        defaults.default_scheduled_date = default_scheduled_date;
    }
    if let Some(default_recurrence) = obsidian.default_recurrence {
        defaults.default_recurrence = default_recurrence;
    }
    if !obsidian.default_reminders.is_empty() {
        defaults.default_reminders = obsidian
            .default_reminders
            .into_iter()
            .filter_map(normalize_obsidian_tasknotes_default_reminder)
            .collect();
    }
}

fn normalize_obsidian_tasknotes_default_reminder(
    reminder: ObsidianTaskNotesDefaultReminder,
) -> Option<TaskNotesDefaultReminderConfig> {
    let id = normalize_optional_text(reminder.id)?;
    let reminder_type = reminder.reminder_type?;

    match reminder_type {
        TaskNotesDefaultReminderType::Relative => {
            let related_to = reminder.related_to?;
            let offset = reminder.offset?;
            let unit = reminder.unit?;
            let direction = reminder.direction?;
            Some(TaskNotesDefaultReminderConfig {
                id,
                reminder_type,
                related_to: Some(related_to),
                offset: Some(offset),
                unit: Some(unit),
                direction: Some(direction),
                absolute_time: None,
                absolute_date: None,
                description: normalize_optional_text(reminder.description),
            })
        }
        TaskNotesDefaultReminderType::Absolute => Some(TaskNotesDefaultReminderConfig {
            id,
            reminder_type,
            related_to: None,
            offset: None,
            unit: None,
            direction: None,
            absolute_time: normalize_optional_text(reminder.absolute_time),
            absolute_date: normalize_optional_text(reminder.absolute_date),
            description: normalize_optional_text(reminder.description),
        }),
    }
}

#[allow(clippy::too_many_lines)]
fn apply_obsidian_kanban_defaults(config: &mut VaultConfig, obsidian: ObsidianKanbanConfig) {
    let previous_default =
        derived_kanban_archive_date_format(&config.kanban.date_format, &config.kanban.time_format);
    let archive_format_was_default = config.kanban.archive_date_format == previous_default;
    let date_format_changed = obsidian.date_format.is_some();
    let time_format_changed = obsidian.time_format.is_some();

    if let Some(date_trigger) = obsidian.date_trigger {
        config.kanban.date_trigger = date_trigger;
    }
    if let Some(time_trigger) = obsidian.time_trigger {
        config.kanban.time_trigger = time_trigger;
    }
    if let Some(date_format) = obsidian.date_format {
        config.kanban.date_format = date_format;
    }
    if let Some(time_format) = obsidian.time_format {
        config.kanban.time_format = time_format;
    }
    if let Some(date_display_format) = obsidian.date_display_format {
        config.kanban.date_display_format = normalize_optional_text(Some(date_display_format));
    }
    if let Some(date_time_display_format) = obsidian.date_time_display_format {
        config.kanban.date_time_display_format =
            normalize_optional_text(Some(date_time_display_format));
    }
    if let Some(link_date_to_daily_note) = obsidian.link_date_to_daily_note {
        config.kanban.link_date_to_daily_note = link_date_to_daily_note;
    }

    if let Some(metadata_keys) = obsidian.metadata_keys {
        let metadata_keys = normalize_kanban_metadata_keys(metadata_keys);
        config.kanban.metadata_keys = metadata_keys;
    }

    if let Some(archive_with_date) = obsidian.archive_with_date {
        config.kanban.archive_with_date = archive_with_date;
    }
    if let Some(append_archive_date) = obsidian.append_archive_date {
        config.kanban.append_archive_date = append_archive_date;
    }
    if let Some(archive_date_format) = obsidian.archive_date_format {
        config.kanban.archive_date_format = archive_date_format;
    } else if archive_format_was_default && (date_format_changed || time_format_changed) {
        config.kanban.archive_date_format = derived_kanban_archive_date_format(
            &config.kanban.date_format,
            &config.kanban.time_format,
        );
    }
    if let Some(archive_date_separator) = obsidian.archive_date_separator {
        config.kanban.archive_date_separator =
            (!archive_date_separator.is_empty()).then_some(archive_date_separator);
    }
    if let Some(new_card_insertion_method) = obsidian.new_card_insertion_method {
        config.kanban.new_card_insertion_method = new_card_insertion_method;
    }
    if let Some(new_line_trigger) = obsidian.new_line_trigger {
        config.kanban.new_line_trigger = normalize_optional_text(Some(new_line_trigger));
    }
    if let Some(new_note_folder) = obsidian.new_note_folder {
        config.kanban.new_note_folder = normalize_optional_text(Some(new_note_folder));
    }
    if let Some(new_note_template) = obsidian.new_note_template {
        config.kanban.new_note_template = normalize_optional_text(Some(new_note_template));
    }
    if let Some(hide_card_count) = obsidian.hide_card_count {
        config.kanban.hide_card_count = hide_card_count;
    }
    if let Some(hide_tags_in_title) = obsidian.hide_tags_in_title {
        config.kanban.hide_tags_in_title = hide_tags_in_title;
    }
    if let Some(hide_tags_display) = obsidian.hide_tags_display {
        config.kanban.hide_tags_display = hide_tags_display;
    }
    if let Some(inline_metadata_position) = obsidian.inline_metadata_position {
        config.kanban.inline_metadata_position =
            normalize_optional_text(Some(inline_metadata_position));
    }
    if obsidian.lane_width.is_some() {
        config.kanban.lane_width = obsidian.lane_width;
    }
    if let Some(full_list_lane_width) = obsidian.full_list_lane_width {
        config.kanban.full_list_lane_width = Some(full_list_lane_width);
    }
    if let Some(list_collapse) = obsidian.list_collapse {
        config.kanban.list_collapse = list_collapse;
    }
    if obsidian.max_archive_size.is_some() {
        config.kanban.max_archive_size = obsidian.max_archive_size;
    }
    if let Some(show_checkboxes) = obsidian.show_checkboxes {
        config.kanban.show_checkboxes = show_checkboxes;
    }
    if let Some(move_dates) = obsidian.move_dates {
        config.kanban.move_dates = Some(move_dates);
    }
    if let Some(move_tags) = obsidian.move_tags {
        config.kanban.move_tags = Some(move_tags);
    }
    if let Some(move_task_metadata) = obsidian.move_task_metadata {
        config.kanban.move_task_metadata = Some(move_task_metadata);
    }
    if let Some(show_add_list) = obsidian.show_add_list {
        config.kanban.show_add_list = Some(show_add_list);
    }
    if let Some(show_archive_all) = obsidian.show_archive_all {
        config.kanban.show_archive_all = Some(show_archive_all);
    }
    if let Some(show_board_settings) = obsidian.show_board_settings {
        config.kanban.show_board_settings = Some(show_board_settings);
    }
    if let Some(show_relative_date) = obsidian.show_relative_date {
        config.kanban.show_relative_date = Some(show_relative_date);
    }
    if let Some(show_search) = obsidian.show_search {
        config.kanban.show_search = Some(show_search);
    }
    if let Some(show_set_view) = obsidian.show_set_view {
        config.kanban.show_set_view = Some(show_set_view);
    }
    if let Some(show_view_as_markdown) = obsidian.show_view_as_markdown {
        config.kanban.show_view_as_markdown = Some(show_view_as_markdown);
    }
    if let Some(date_picker_week_start) = obsidian.date_picker_week_start {
        config.kanban.date_picker_week_start = Some(date_picker_week_start);
    }
    if let Some(table_sizing) = obsidian.table_sizing {
        config.kanban.table_sizing = table_sizing;
    }
    if let Some(tag_action) = obsidian.tag_action {
        config.kanban.tag_action = normalize_optional_text(Some(tag_action));
    }
    if let Some(tag_colors) = obsidian.tag_colors {
        config.kanban.tag_colors = tag_colors;
    }
    if let Some(tag_sort) = obsidian.tag_sort {
        config.kanban.tag_sort = tag_sort;
    }
    if let Some(date_colors) = obsidian.date_colors {
        config.kanban.date_colors = date_colors;
    }
}

fn merge_export_profile_config(target: &mut ExportProfileConfig, profile: ExportProfileConfig) {
    if let Some(format) = profile.format {
        target.format = Some(format);
    }
    if let Some(query) = profile.query {
        target.query = Some(query);
    }
    if let Some(query_json) = profile.query_json {
        target.query_json = Some(query_json);
    }
    if let Some(path) = profile.path {
        target.path = Some(path);
    }
    if let Some(site_profile) = profile.site_profile {
        target.site_profile = Some(site_profile);
    }
    if let Some(title) = profile.title {
        target.title = Some(title);
    }
    if let Some(author) = profile.author {
        target.author = Some(author);
    }
    if let Some(toc) = profile.toc {
        target.toc = Some(toc);
    }
    if let Some(backlinks) = profile.backlinks {
        target.backlinks = Some(backlinks);
    }
    if let Some(frontmatter) = profile.frontmatter {
        target.frontmatter = Some(frontmatter);
    }
    if let Some(pretty) = profile.pretty {
        target.pretty = Some(pretty);
    }
    if let Some(graph_format) = profile.graph_format {
        target.graph_format = Some(graph_format);
    }
    if let Some(content_transform_rules) = profile.content_transform_rules {
        target.content_transform_rules = Some(content_transform_rules);
    }
}

fn merge_site_shell_options_config(
    target: &mut SiteShellOptionsConfig,
    options: &SiteShellOptionsConfig,
) {
    if let Some(reader_mode) = options.reader_mode {
        target.reader_mode = Some(reader_mode);
    }
    if let Some(default_palette) = options.default_palette {
        target.default_palette = Some(default_palette);
    }
    if let Some(left_rail) = options.left_rail {
        target.left_rail = Some(left_rail);
    }
    if let Some(right_rail) = options.right_rail {
        target.right_rail = Some(right_rail);
    }
}

fn merge_site_navigation_options_config(
    target: &mut SiteNavigationOptionsConfig,
    options: &SiteNavigationOptionsConfig,
) {
    if let Some(explorer) = options.explorer {
        target.explorer = Some(explorer);
    }
    if let Some(folder_click) = options.folder_click {
        target.folder_click = Some(folder_click);
    }
    if let Some(default_folder_state) = options.default_folder_state {
        target.default_folder_state = Some(default_folder_state);
    }
    if let Some(use_saved_state) = options.use_saved_state {
        target.use_saved_state = Some(use_saved_state);
    }
    if let Some(show_home) = options.show_home {
        target.show_home = Some(show_home);
    }
    if let Some(show_recent) = options.show_recent {
        target.show_recent = Some(show_recent);
    }
    if let Some(show_folders) = options.show_folders {
        target.show_folders = Some(show_folders);
    }
    if let Some(show_tags) = options.show_tags {
        target.show_tags = Some(show_tags);
    }
    if let Some(show_graph) = options.show_graph {
        target.show_graph = Some(show_graph);
    }
}

fn merge_site_modules_options_config(
    target: &mut SiteModulesOptionsConfig,
    options: &SiteModulesOptionsConfig,
) {
    if let Some(toc) = options.toc {
        target.toc = Some(toc);
    }
    if let Some(graph) = options.graph {
        target.graph = Some(graph);
    }
    if let Some(backlinks) = options.backlinks {
        target.backlinks = Some(backlinks);
    }
    if let Some(outgoing_links) = options.outgoing_links {
        target.outgoing_links = Some(outgoing_links);
    }
}

fn merge_site_profile_config(target: &mut SiteProfileConfig, profile: SiteProfileConfig) {
    if let Some(title) = profile.title {
        target.title = Some(title);
    }
    if let Some(page_title_template) = profile.page_title_template {
        target.page_title_template = Some(page_title_template);
    }
    if let Some(base_url) = profile.base_url {
        target.base_url = Some(base_url);
    }
    if let Some(deploy_path) = profile.deploy_path {
        target.deploy_path = Some(deploy_path);
    }
    if let Some(output_dir) = profile.output_dir {
        target.output_dir = Some(output_dir);
    }
    if let Some(home) = profile.home {
        target.home = Some(home);
    }
    if let Some(language) = profile.language {
        target.language = Some(language);
    }
    if let Some(theme) = profile.theme {
        target.theme = Some(theme);
    }
    if let Some(search) = profile.search {
        target.search = Some(search);
    }
    if let Some(graph) = profile.graph {
        target.graph = Some(graph);
    }
    if let Some(backlinks) = profile.backlinks {
        target.backlinks = Some(backlinks);
    }
    if let Some(rss) = profile.rss {
        target.rss = Some(rss);
    }
    merge_site_shell_options_config(&mut target.shell, &profile.shell);
    merge_site_navigation_options_config(&mut target.navigation, &profile.navigation);
    merge_site_modules_options_config(&mut target.modules, &profile.modules);
    if let Some(favicon) = profile.favicon {
        target.favicon = Some(favicon);
    }
    if let Some(logo) = profile.logo {
        target.logo = Some(logo);
    }
    if !profile.extra_css.is_empty() {
        target.extra_css = profile.extra_css;
    }
    if !profile.extra_js.is_empty() {
        target.extra_js = profile.extra_js;
    }
    if let Some(include_query) = profile.include_query {
        target.include_query = Some(include_query);
    }
    if let Some(include_query_json) = profile.include_query_json {
        target.include_query_json = Some(include_query_json);
    }
    if !profile.include_paths.is_empty() {
        target.include_paths = profile.include_paths;
    }
    if !profile.include_folders.is_empty() {
        target.include_folders = profile.include_folders;
    }
    if !profile.exclude_paths.is_empty() {
        target.exclude_paths = profile.exclude_paths;
    }
    if !profile.exclude_folders.is_empty() {
        target.exclude_folders = profile.exclude_folders;
    }
    if !profile.exclude_tags.is_empty() {
        target.exclude_tags = profile.exclude_tags;
    }
    if let Some(link_policy) = profile.link_policy {
        target.link_policy = Some(link_policy);
    }
    target.asset_policy = profile.asset_policy;
    if let Some(dataview_js) = profile.dataview_js {
        target.dataview_js = Some(dataview_js);
    }
    if let Some(raw_html) = profile.raw_html {
        target.raw_html = Some(raw_html);
    }
    if let Some(content_transform_rules) = profile.content_transform_rules {
        target.content_transform_rules = Some(content_transform_rules);
    }
}

#[allow(clippy::too_many_lines)]
fn apply_vulcan_overrides(config: &mut VaultConfig, overrides: PartialVulcanConfig) {
    if let Some(scan) = overrides.scan {
        if let Some(default_mode) = scan.default_mode {
            config.scan.default_mode = default_mode;
        }
        if let Some(browse_mode) = scan.browse_mode {
            config.scan.browse_mode = browse_mode;
        }
    }

    if let Some(chunking) = overrides.chunking {
        if let Some(strategy) = chunking.strategy {
            config.chunking.strategy = strategy;
        }
        if let Some(target_size) = chunking.target_size {
            config.chunking.target_size = target_size;
        }
        if let Some(overlap) = chunking.overlap {
            config.chunking.overlap = overlap;
        }
    }

    if let Some(links) = overrides.links {
        if let Some(resolution) = links.resolution {
            config.link_resolution = resolution;
        }
        if let Some(style) = links.style {
            config.link_style = style;
        }
        if let Some(attachment_folder) = links.attachment_folder {
            config.attachment_folder = attachment_folder;
        }
    }

    if let Some(embedding) = overrides.embedding {
        let target = config
            .embedding
            .get_or_insert_with(EmbeddingProviderConfig::default);
        if let Some(provider) = embedding.provider {
            target.provider = Some(provider);
        }
        if let Some(base_url) = embedding.base_url {
            target.base_url = base_url;
        }
        if let Some(model) = embedding.model {
            target.model = model;
        }
        if let Some(api_key_env) = embedding.api_key_env {
            target.api_key_env = Some(api_key_env);
        }
        if let Some(normalized) = embedding.normalized {
            target.normalized = Some(normalized);
        }
        if let Some(max_batch_size) = embedding.max_batch_size {
            target.max_batch_size = Some(max_batch_size);
        }
        if let Some(max_input_tokens) = embedding.max_input_tokens {
            target.max_input_tokens = Some(max_input_tokens);
        }
        if let Some(max_concurrency) = embedding.max_concurrency {
            target.max_concurrency = Some(max_concurrency);
        }
        if let Some(cache_key) = embedding.cache_key {
            target.cache_key = Some(cache_key);
        }
    }
    if let Some(extraction) = overrides.extraction {
        let target = config
            .extraction
            .get_or_insert_with(AttachmentExtractionConfig::default);
        if let Some(command) = extraction.command {
            target.command = command;
        }
        if let Some(args) = extraction.args {
            target.args = args;
        }
        if let Some(extensions) = extraction.extensions {
            target.extensions = normalize_string_list(extensions);
        }
        if let Some(max_output_bytes) = extraction.max_output_bytes {
            target.max_output_bytes = Some(max_output_bytes);
        }
    }
    if let Some(git) = overrides.git {
        if let Some(auto_commit) = git.auto_commit {
            config.git.auto_commit = auto_commit;
        }
        if let Some(trigger) = git.trigger {
            config.git.trigger = trigger;
        }
        if let Some(message) = git.message {
            config.git.message = message;
        }
        if let Some(scope) = git.scope {
            config.git.scope = scope;
        }
        if let Some(exclude) = git.exclude {
            config.git.exclude = exclude;
        }
    }
    if let Some(inbox) = overrides.inbox {
        if let Some(path) = inbox.path {
            config.inbox.path = path;
        }
        if let Some(format) = inbox.format {
            config.inbox.format = format;
        }
        if let Some(timestamp) = inbox.timestamp {
            config.inbox.timestamp = timestamp;
        }
        if let Some(heading) = inbox.heading {
            config.inbox.heading = Some(heading);
        }
    }

    if let Some(tasks) = overrides.tasks {
        if let Some(default_source) = tasks.default_source {
            config.tasks.default_source = default_source;
        }
        if let Some(global_filter) = tasks.global_filter {
            config.tasks.global_filter = normalize_optional_text(Some(global_filter));
        }
        if let Some(global_query) = tasks.global_query {
            config.tasks.global_query = normalize_optional_text(Some(global_query));
        }
        if let Some(remove_global_filter) = tasks.remove_global_filter {
            config.tasks.remove_global_filter = remove_global_filter;
        }
        if let Some(set_created_date) = tasks.set_created_date {
            config.tasks.set_created_date = set_created_date;
        }
        if let Some(recurrence_on_completion) = tasks.recurrence_on_completion {
            config.tasks.recurrence_on_completion =
                normalize_optional_text(Some(recurrence_on_completion));
        }
        if let Some(statuses) = tasks.statuses {
            if let Some(definitions) = statuses.definitions {
                apply_task_status_definitions(&mut config.tasks.statuses, definitions);
            }
            if let Some(todo) = statuses.todo {
                config.tasks.statuses.todo = todo;
            }
            if let Some(completed) = statuses.completed {
                config.tasks.statuses.completed = completed;
            }
            if let Some(in_progress) = statuses.in_progress {
                config.tasks.statuses.in_progress = in_progress;
            }
            if let Some(cancelled) = statuses.cancelled {
                config.tasks.statuses.cancelled = cancelled;
            }
            if let Some(non_task) = statuses.non_task {
                config.tasks.statuses.non_task = non_task;
            }
        }
    }

    if let Some(tasknotes) = overrides.tasknotes {
        if let Some(tasks_folder) = tasknotes.tasks_folder {
            config.tasknotes.tasks_folder = tasks_folder;
        }
        if let Some(archive_folder) = tasknotes.archive_folder {
            config.tasknotes.archive_folder = archive_folder;
        }
        if let Some(task_tag) = tasknotes.task_tag {
            config.tasknotes.task_tag = task_tag;
        }
        if let Some(identification_method) = tasknotes.identification_method {
            config.tasknotes.identification_method = identification_method;
        }
        if let Some(task_property_name) = tasknotes.task_property_name {
            config.tasknotes.task_property_name = normalize_optional_text(Some(task_property_name));
        }
        if let Some(task_property_value) = tasknotes.task_property_value {
            config.tasknotes.task_property_value =
                normalize_optional_text(Some(task_property_value));
        }
        if let Some(excluded_folders) = tasknotes.excluded_folders {
            config.tasknotes.excluded_folders = normalize_string_list(excluded_folders);
        }
        if let Some(default_status) = tasknotes.default_status {
            config.tasknotes.default_status = default_status;
        }
        if let Some(default_priority) = tasknotes.default_priority {
            config.tasknotes.default_priority = default_priority;
        }
        if let Some(field_mapping) = tasknotes.field_mapping {
            apply_partial_tasknotes_field_mapping(
                &mut config.tasknotes.field_mapping,
                field_mapping,
            );
        }
        if let Some(statuses) = tasknotes.statuses {
            config.tasknotes.statuses = statuses;
        }
        if let Some(priorities) = tasknotes.priorities {
            config.tasknotes.priorities = priorities;
        }
        if let Some(user_fields) = tasknotes.user_fields {
            config.tasknotes.user_fields = user_fields;
        }
        if let Some(enable_natural_language_input) = tasknotes.enable_natural_language_input {
            config.tasknotes.enable_natural_language_input = enable_natural_language_input;
        }
        if let Some(nlp_default_to_scheduled) = tasknotes.nlp_default_to_scheduled {
            config.tasknotes.nlp_default_to_scheduled = nlp_default_to_scheduled;
        }
        if let Some(nlp_language) = tasknotes.nlp_language {
            if let Some(language) = normalize_optional_text(Some(nlp_language)) {
                config.tasknotes.nlp_language = language;
            }
        }
        if let Some(nlp_triggers) = tasknotes.nlp_triggers {
            config.tasknotes.nlp_triggers = nlp_triggers;
        }
        if let Some(pomodoro) = tasknotes.pomodoro {
            config.tasknotes.pomodoro = pomodoro;
        }
        if let Some(task_creation_defaults) = tasknotes.task_creation_defaults {
            config.tasknotes.task_creation_defaults = task_creation_defaults;
        }
        if let Some(saved_views) = tasknotes.saved_views {
            config.tasknotes.saved_views = saved_views;
        }
    }

    if let Some(kanban) = overrides.kanban {
        let previous_default = derived_kanban_archive_date_format(
            &config.kanban.date_format,
            &config.kanban.time_format,
        );
        let archive_format_was_default = config.kanban.archive_date_format == previous_default;
        let date_format_changed = kanban.date_format.is_some();
        let time_format_changed = kanban.time_format.is_some();

        if let Some(date_trigger) = kanban.date_trigger {
            config.kanban.date_trigger = date_trigger;
        }
        if let Some(time_trigger) = kanban.time_trigger {
            config.kanban.time_trigger = time_trigger;
        }
        if let Some(date_format) = kanban.date_format {
            config.kanban.date_format = date_format;
        }
        if let Some(time_format) = kanban.time_format {
            config.kanban.time_format = time_format;
        }
        if let Some(date_display_format) = kanban.date_display_format {
            config.kanban.date_display_format = normalize_optional_text(Some(date_display_format));
        }
        if let Some(date_time_display_format) = kanban.date_time_display_format {
            config.kanban.date_time_display_format =
                normalize_optional_text(Some(date_time_display_format));
        }
        if let Some(link_date_to_daily_note) = kanban.link_date_to_daily_note {
            config.kanban.link_date_to_daily_note = link_date_to_daily_note;
        }
        if let Some(metadata_keys) = kanban.metadata_keys {
            config.kanban.metadata_keys = normalize_kanban_metadata_keys(metadata_keys);
        }
        if let Some(archive_with_date) = kanban.archive_with_date {
            config.kanban.archive_with_date = archive_with_date;
        }
        if let Some(append_archive_date) = kanban.append_archive_date {
            config.kanban.append_archive_date = append_archive_date;
        }
        if let Some(archive_date_format) = kanban.archive_date_format {
            config.kanban.archive_date_format = archive_date_format;
        } else if archive_format_was_default && (date_format_changed || time_format_changed) {
            config.kanban.archive_date_format = derived_kanban_archive_date_format(
                &config.kanban.date_format,
                &config.kanban.time_format,
            );
        }
        if let Some(archive_date_separator) = kanban.archive_date_separator {
            config.kanban.archive_date_separator =
                (!archive_date_separator.is_empty()).then_some(archive_date_separator);
        }
        if let Some(new_card_insertion_method) = kanban.new_card_insertion_method {
            config.kanban.new_card_insertion_method = new_card_insertion_method;
        }
        if let Some(new_line_trigger) = kanban.new_line_trigger {
            config.kanban.new_line_trigger = normalize_optional_text(Some(new_line_trigger));
        }
        if let Some(new_note_folder) = kanban.new_note_folder {
            config.kanban.new_note_folder = normalize_optional_text(Some(new_note_folder));
        }
        if let Some(new_note_template) = kanban.new_note_template {
            config.kanban.new_note_template = normalize_optional_text(Some(new_note_template));
        }
        if let Some(hide_card_count) = kanban.hide_card_count {
            config.kanban.hide_card_count = hide_card_count;
        }
        if let Some(hide_tags_in_title) = kanban.hide_tags_in_title {
            config.kanban.hide_tags_in_title = hide_tags_in_title;
        }
        if let Some(hide_tags_display) = kanban.hide_tags_display {
            config.kanban.hide_tags_display = hide_tags_display;
        }
        if let Some(inline_metadata_position) = kanban.inline_metadata_position {
            config.kanban.inline_metadata_position =
                normalize_optional_text(Some(inline_metadata_position));
        }
        if let Some(lane_width) = kanban.lane_width {
            config.kanban.lane_width = Some(lane_width);
        }
        if let Some(full_list_lane_width) = kanban.full_list_lane_width {
            config.kanban.full_list_lane_width = Some(full_list_lane_width);
        }
        if let Some(list_collapse) = kanban.list_collapse {
            config.kanban.list_collapse = list_collapse;
        }
        if let Some(max_archive_size) = kanban.max_archive_size {
            config.kanban.max_archive_size = Some(max_archive_size);
        }
        if let Some(show_checkboxes) = kanban.show_checkboxes {
            config.kanban.show_checkboxes = show_checkboxes;
        }
        if let Some(move_dates) = kanban.move_dates {
            config.kanban.move_dates = Some(move_dates);
        }
        if let Some(move_tags) = kanban.move_tags {
            config.kanban.move_tags = Some(move_tags);
        }
        if let Some(move_task_metadata) = kanban.move_task_metadata {
            config.kanban.move_task_metadata = Some(move_task_metadata);
        }
        if let Some(show_add_list) = kanban.show_add_list {
            config.kanban.show_add_list = Some(show_add_list);
        }
        if let Some(show_archive_all) = kanban.show_archive_all {
            config.kanban.show_archive_all = Some(show_archive_all);
        }
        if let Some(show_board_settings) = kanban.show_board_settings {
            config.kanban.show_board_settings = Some(show_board_settings);
        }
        if let Some(show_relative_date) = kanban.show_relative_date {
            config.kanban.show_relative_date = Some(show_relative_date);
        }
        if let Some(show_search) = kanban.show_search {
            config.kanban.show_search = Some(show_search);
        }
        if let Some(show_set_view) = kanban.show_set_view {
            config.kanban.show_set_view = Some(show_set_view);
        }
        if let Some(show_view_as_markdown) = kanban.show_view_as_markdown {
            config.kanban.show_view_as_markdown = Some(show_view_as_markdown);
        }
        if let Some(date_picker_week_start) = kanban.date_picker_week_start {
            config.kanban.date_picker_week_start = Some(date_picker_week_start);
        }
        if let Some(table_sizing) = kanban.table_sizing {
            config.kanban.table_sizing = table_sizing;
        }
        if let Some(tag_action) = kanban.tag_action {
            config.kanban.tag_action = normalize_optional_text(Some(tag_action));
        }
        if let Some(tag_colors) = kanban.tag_colors {
            config.kanban.tag_colors = tag_colors;
        }
        if let Some(tag_sort) = kanban.tag_sort {
            config.kanban.tag_sort = tag_sort;
        }
        if let Some(date_colors) = kanban.date_colors {
            config.kanban.date_colors = date_colors;
        }
    }

    if let Some(dataview) = overrides.dataview {
        if let Some(prefix) = dataview.inline_query_prefix {
            config.dataview.inline_query_prefix = prefix;
        }
        if let Some(prefix) = dataview.inline_js_query_prefix {
            config.dataview.inline_js_query_prefix = prefix;
        }
        if let Some(enabled) = dataview.enable_dataview_js {
            config.dataview.enable_dataview_js = enabled;
        }
        if let Some(enabled) = dataview.enable_inline_dataview_js {
            config.dataview.enable_inline_dataview_js = enabled;
        }
        if let Some(tracking) = dataview.task_completion_tracking {
            config.dataview.task_completion_tracking = tracking;
        }
        if let Some(use_emoji_shorthand) = dataview.task_completion_use_emoji_shorthand {
            config.dataview.task_completion_use_emoji_shorthand = use_emoji_shorthand;
        }
        if let Some(text) = dataview.task_completion_text {
            config.dataview.task_completion_text = text;
        }
        if let Some(recursive) = dataview.recursive_subtask_completion {
            config.dataview.recursive_subtask_completion = recursive;
        }
        if let Some(display_result_count) = dataview.display_result_count {
            config.dataview.display_result_count = display_result_count;
        }
        if let Some(format) = dataview.default_date_format {
            config.dataview.default_date_format = format;
        }
        if let Some(format) = dataview.default_datetime_format {
            config.dataview.default_datetime_format = format;
        }
        if let Some(timezone) = dataview.timezone {
            config.dataview.timezone = Some(timezone);
        }
        if let Some(depth) = dataview.max_recursive_render_depth {
            config.dataview.max_recursive_render_depth = depth;
        }
        if let Some(name) = dataview.primary_column_name {
            config.dataview.primary_column_name = name;
        }
        if let Some(name) = dataview.group_column_name {
            config.dataview.group_column_name = name;
        }
        if let Some(timeout) = dataview.js_timeout_seconds {
            config.dataview.js_timeout_seconds = timeout;
            config.js_runtime.default_timeout_seconds = timeout;
        }
        if let Some(limit) = dataview.js_memory_limit_bytes {
            config.dataview.js_memory_limit_bytes = limit;
            config.js_runtime.memory_limit_mb = bytes_to_megabytes_ceil(limit);
        }
        if let Some(limit) = dataview.js_max_stack_size_bytes {
            config.dataview.js_max_stack_size_bytes = limit;
            config.js_runtime.stack_limit_kb = bytes_to_kilobytes_ceil(limit);
        }
    }

    if let Some(js_runtime) = overrides.js_runtime {
        if let Some(limit) = js_runtime.memory_limit_mb {
            config.js_runtime.memory_limit_mb = limit;
        }
        if let Some(limit) = js_runtime.stack_limit_kb {
            config.js_runtime.stack_limit_kb = limit;
        }
        if let Some(timeout) = js_runtime.default_timeout_seconds {
            config.js_runtime.default_timeout_seconds = timeout;
        }
        if let Some(sandbox) = js_runtime.default_sandbox {
            config.js_runtime.default_sandbox = sandbox;
        }
        if let Some(scripts_folder) = js_runtime.scripts_folder {
            config.js_runtime.scripts_folder =
                normalize_filesystem_pathbuf(&scripts_folder).unwrap_or(scripts_folder);
        }
    }

    if let Some(templates) = overrides.templates {
        if let Some(date_format) = templates.date_format {
            config.templates.date_format = date_format;
        }
        if let Some(time_format) = templates.time_format {
            config.templates.time_format = time_format;
        }
        if let Some(obsidian_folder) = templates.obsidian_folder {
            config.templates.obsidian_folder = normalize_template_pathbuf(&obsidian_folder);
        }
        if let Some(templater_folder) = templates.templater_folder {
            config.templates.templater_folder = normalize_template_pathbuf(&templater_folder);
        }
        if let Some(command_timeout) = templates.command_timeout {
            config.templates.command_timeout = command_timeout;
        }
        if let Some(templates_pairs) = templates.templates_pairs {
            config.templates.templates_pairs =
                normalize_templater_command_pairs_from_config(templates_pairs);
        }
        if let Some(trigger_on_file_creation) = templates.trigger_on_file_creation {
            config.templates.trigger_on_file_creation = trigger_on_file_creation;
        }
        if let Some(auto_jump_to_cursor) = templates.auto_jump_to_cursor {
            config.templates.auto_jump_to_cursor = auto_jump_to_cursor;
        }
        if let Some(enable_system_commands) = templates.enable_system_commands {
            config.templates.enable_system_commands = enable_system_commands;
        }
        if let Some(shell_path) = templates.shell_path {
            config.templates.shell_path = normalize_filesystem_pathbuf(&shell_path);
        }
        if let Some(user_scripts_folder) = templates.user_scripts_folder {
            config.templates.user_scripts_folder = normalize_template_pathbuf(&user_scripts_folder);
        }
        if let Some(web_allowlist) = templates.web_allowlist {
            config.templates.web_allowlist = normalize_string_list(web_allowlist);
        }
        if let Some(enable_folder_templates) = templates.enable_folder_templates {
            config.templates.enable_folder_templates = enable_folder_templates;
        }
        if let Some(folder_templates) = templates.folder_templates {
            config.templates.folder_templates =
                normalize_templater_folder_templates_from_config(folder_templates);
        }
        if let Some(enable_file_templates) = templates.enable_file_templates {
            config.templates.enable_file_templates = enable_file_templates;
        }
        if let Some(file_templates) = templates.file_templates {
            config.templates.file_templates =
                normalize_templater_file_templates_from_config(file_templates);
        }
        if let Some(syntax_highlighting) = templates.syntax_highlighting {
            config.templates.syntax_highlighting = syntax_highlighting;
        }
        if let Some(syntax_highlighting_mobile) = templates.syntax_highlighting_mobile {
            config.templates.syntax_highlighting_mobile = syntax_highlighting_mobile;
        }
        if let Some(enabled_templates_hotkeys) = templates.enabled_templates_hotkeys {
            config.templates.enabled_templates_hotkeys =
                normalize_string_list(enabled_templates_hotkeys);
        }
        if let Some(startup_templates) = templates.startup_templates {
            config.templates.startup_templates = normalize_string_list(startup_templates);
        }
        if let Some(intellisense_render) = templates.intellisense_render {
            config.templates.intellisense_render = intellisense_render;
        }
    }

    if let Some(quickadd) = overrides.quickadd {
        if let Some(template_folder) = quickadd.template_folder {
            config.quickadd.template_folder = normalize_template_pathbuf(&template_folder);
        }
        if let Some(global_variables) = quickadd.global_variables {
            config.quickadd.global_variables =
                normalize_quickadd_global_variables(global_variables);
        }
        if let Some(capture_choices) = quickadd.capture_choices {
            config.quickadd.capture_choices = capture_choices;
        }
        if let Some(template_choices) = quickadd.template_choices {
            config.quickadd.template_choices = template_choices;
        }
        if let Some(ai) = quickadd.ai {
            let target = config
                .quickadd
                .ai
                .get_or_insert_with(QuickAddAiConfig::default);
            if let Some(default_model) = ai.default_model {
                target.default_model = normalize_optional_text(Some(default_model));
            }
            if let Some(default_system_prompt) = ai.default_system_prompt {
                target.default_system_prompt = normalize_optional_text(Some(default_system_prompt));
            }
            if let Some(prompt_templates_folder) = ai.prompt_templates_folder {
                target.prompt_templates_folder =
                    normalize_template_pathbuf(&prompt_templates_folder);
            }
            if let Some(show_assistant) = ai.show_assistant {
                target.show_assistant = show_assistant;
            }
            if let Some(providers) = ai.providers {
                target.providers = providers;
            }
        }
    }

    if let Some(assistant) = overrides.assistant {
        if let Some(prompts_folder) = assistant.prompts_folder {
            config.assistant.prompts_folder =
                normalize_template_pathbuf(&prompts_folder).unwrap_or_default();
        }
        if let Some(skills_folder) = assistant.skills_folder {
            config.assistant.skills_folder =
                normalize_template_pathbuf(&skills_folder).unwrap_or_default();
        }
        if let Some(tools_folder) = assistant.tools_folder {
            config.assistant.tools_folder =
                normalize_template_pathbuf(&tools_folder).unwrap_or_default();
        }
    }

    if let Some(web) = overrides.web {
        if let Some(user_agent) = web.user_agent {
            config.web.user_agent = user_agent;
        }
        if let Some(search) = web.search {
            if let Some(backend) = search.backend {
                config.web.search.backend = backend;
            }
            if let Some(api_key_env) = search.api_key_env {
                config.web.search.api_key_env = Some(api_key_env);
            }
            if let Some(base_url) = search.base_url {
                config.web.search.base_url = Some(base_url);
            }
        }
    }

    if let Some(periodic) = overrides.periodic {
        for (name, overrides) in periodic.notes {
            let note = config.periodic.note_mut(&name);
            if let Some(enabled) = overrides.enabled {
                note.enabled = enabled;
            }
            if let Some(folder) = overrides.folder {
                note.folder = normalize_periodic_folder_pathbuf(&folder);
            }
            if let Some(format) = normalize_optional_text(overrides.format) {
                note.format = format;
            }
            if let Some(unit) = overrides.unit {
                note.unit = Some(unit);
            }
            if let Some(interval) = overrides.interval {
                note.interval = interval.max(1);
            }
            if let Some(anchor_date) = overrides.anchor_date {
                note.anchor_date = normalize_optional_text(Some(anchor_date));
            }
            if let Some(template) = overrides.template {
                note.template = normalize_optional_text(Some(template));
            }
            if let Some(start_of_week) = overrides.start_of_week {
                note.start_of_week = start_of_week;
            }
            if let Some(schedule_heading) = overrides.schedule_heading {
                note.schedule_heading = normalize_optional_text(Some(schedule_heading));
            }
        }
    }

    if let Some(export) = overrides.export {
        if let Some(profiles) = export.profiles {
            for (name, profile) in profiles {
                let target = config.export.profiles.entry(name).or_default();
                merge_export_profile_config(target, profile);
            }
        }
    }

    if let Some(site) = overrides.site {
        if let Some(profiles) = site.profiles {
            for (name, profile) in profiles {
                let target = config.site.profiles.entry(name).or_default();
                merge_site_profile_config(target, profile);
            }
        }
    }

    if let Some(plugins) = overrides.plugins {
        for (name, overrides) in plugins {
            let plugin = config.plugins.entry(name).or_default();
            apply_partial_plugin_registration(plugin, overrides);
        }
    }

    if let Some(aliases) = overrides.aliases {
        config.aliases.extend(aliases);
    }
}

fn apply_permission_profile_overrides(
    profiles: &mut BTreeMap<String, PermissionProfile>,
    overrides: Option<PartialPermissionsConfig>,
) {
    let Some(overrides) = overrides else {
        return;
    };
    let Some(profile_overrides) = overrides.profiles else {
        return;
    };

    for (name, override_profile) in profile_overrides {
        let profile = profiles.entry(name).or_default();
        apply_partial_permission_profile(profile, override_profile);
    }
}

fn apply_partial_permission_profile(
    profile: &mut PermissionProfile,
    overrides: PartialPermissionProfile,
) {
    if let Some(read) = overrides.read {
        profile.read = read;
    }
    if let Some(write) = overrides.write {
        profile.write = write;
    }
    if let Some(refactor) = overrides.refactor {
        profile.refactor = refactor;
    }
    if let Some(git) = overrides.git {
        profile.git = git;
    }
    if let Some(network) = overrides.network {
        profile.network = network;
    }
    if let Some(index) = overrides.index {
        profile.index = index;
    }
    if let Some(config) = overrides.config {
        profile.config = config;
    }
    if let Some(execute) = overrides.execute {
        profile.execute = execute;
    }
    if let Some(shell) = overrides.shell {
        profile.shell = shell;
    }
    if let Some(cpu_limit_ms) = overrides.cpu_limit_ms {
        profile.cpu_limit_ms = cpu_limit_ms;
    }
    if let Some(memory_limit_mb) = overrides.memory_limit_mb {
        profile.memory_limit_mb = memory_limit_mb;
    }
    if let Some(stack_limit_kb) = overrides.stack_limit_kb {
        profile.stack_limit_kb = stack_limit_kb;
    }
    if let Some(policy_hook) = overrides.policy_hook {
        profile.policy_hook = normalize_filesystem_pathbuf(&policy_hook).or(Some(policy_hook));
    }
}

fn apply_partial_plugin_registration(
    plugin: &mut PluginRegistration,
    overrides: PartialPluginRegistration,
) {
    if let Some(enabled) = overrides.enabled {
        plugin.enabled = enabled;
    }
    if let Some(path) = overrides.path {
        plugin.path = normalize_filesystem_pathbuf(&path).or(Some(path));
    }
    if let Some(events) = overrides.events {
        let mut normalized = events;
        normalized.sort();
        normalized.dedup();
        plugin.events = normalized;
    }
    if let Some(sandbox) = overrides.sandbox {
        plugin.sandbox = Some(sandbox);
    }
    if let Some(permission_profile) = overrides.permission_profile {
        plugin.permission_profile = normalize_optional_text(Some(permission_profile));
    }
    if let Some(description) = overrides.description {
        plugin.description = normalize_optional_text(Some(description));
    }
}

fn normalize_attachment_folder(path: &str) -> PathBuf {
    if path == "/" || path.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(path)
    }
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_template_path(value: Option<String>) -> Option<PathBuf> {
    let value = normalize_optional_text(value)?;
    if value == "/" {
        Some(PathBuf::from("."))
    } else {
        let trimmed = value.trim_matches('/');
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    }
}

fn normalize_template_pathbuf(value: &Path) -> Option<PathBuf> {
    normalize_template_path(Some(value.to_string_lossy().into_owned()))
}

fn normalize_periodic_folder(value: String) -> PathBuf {
    normalize_template_path(Some(value)).unwrap_or_default()
}

fn normalize_periodic_folder_pathbuf(value: &Path) -> PathBuf {
    normalize_periodic_folder(value.to_string_lossy().into_owned())
}

fn normalize_filesystem_path(value: Option<String>) -> Option<PathBuf> {
    normalize_optional_text(value).map(PathBuf::from)
}

fn normalize_filesystem_pathbuf(value: &Path) -> Option<PathBuf> {
    normalize_filesystem_path(Some(value.to_string_lossy().into_owned()))
}

fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let Some(value) = normalize_optional_text(Some(value)) else {
            continue;
        };
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    normalized
}

fn normalize_comma_separated_paths(value: &str) -> Vec<String> {
    normalize_string_list(value.split(',').map(ToOwned::to_owned).collect())
}

fn normalize_quickadd_format_value(config: &ObsidianQuickAddFormatConfig) -> Option<String> {
    config
        .enabled
        .unwrap_or(false)
        .then(|| normalize_optional_text(config.format.clone()))
        .flatten()
}

fn normalize_quickadd_path_list(values: Vec<String>) -> Vec<PathBuf> {
    let mut normalized = Vec::new();
    for value in values {
        let Some(value) = normalize_template_path(Some(value)) else {
            continue;
        };
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    normalized
}

fn normalize_quickadd_global_variables(
    values: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    values
        .into_iter()
        .filter_map(|(key, value)| normalize_optional_text(Some(key)).map(|key| (key, value)))
        .collect()
}

fn normalize_quickadd_ai_config(config: ObsidianQuickAddAiConfig) -> Option<QuickAddAiConfig> {
    let providers = config
        .providers
        .into_iter()
        .filter_map(quickadd_provider_from_obsidian)
        .collect::<Vec<_>>();
    let default_model = normalize_optional_text(config.default_model)
        .filter(|model| !model.eq_ignore_ascii_case("Ask me"));
    let default_system_prompt = normalize_optional_text(config.default_system_prompt);
    let prompt_templates_folder = normalize_template_path(config.prompt_templates_folder_path);
    let show_assistant = config.show_assistant.unwrap_or(false);

    if default_model.is_none()
        && default_system_prompt.is_none()
        && prompt_templates_folder.is_none()
        && !show_assistant
        && providers.is_empty()
    {
        None
    } else {
        Some(QuickAddAiConfig {
            default_model,
            default_system_prompt,
            prompt_templates_folder,
            show_assistant,
            providers,
        })
    }
}

fn quickadd_provider_from_obsidian(
    provider: ObsidianQuickAddAiProviderConfig,
) -> Option<QuickAddAiProviderConfig> {
    let name = normalize_optional_text(provider.name)?;
    let endpoint = normalize_optional_text(provider.endpoint).unwrap_or_default();
    let models = normalize_string_list(
        provider
            .models
            .into_iter()
            .filter_map(|model| normalize_optional_text(model.name))
            .collect(),
    );

    Some(QuickAddAiProviderConfig {
        api_key_env: quickadd_provider_api_key_env(
            &name,
            provider.api_key_ref.as_deref(),
            provider.api_key.as_deref(),
        ),
        models,
        model_source: normalize_optional_text(provider.model_source),
        auto_sync_models: provider.auto_sync_models,
        name,
        endpoint,
    })
}

fn quickadd_provider_api_key_env(
    provider_name: &str,
    api_key_ref: Option<&str>,
    api_key: Option<&str>,
) -> Option<String> {
    let api_key_ref =
        api_key_ref.and_then(|value| normalize_optional_text(Some(value.to_string())));
    let has_plaintext_key = api_key.is_some_and(|value| !value.trim().is_empty());

    api_key_ref
        .and_then(|value| normalize_env_var_name(&value))
        .or_else(|| {
            has_plaintext_key
                .then(|| normalize_env_var_name(&format!("{provider_name}_API_KEY")))
                .flatten()
        })
}

fn normalize_env_var_name(value: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut last_was_separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_uppercase());
            last_was_separator = false;
        } else if !last_was_separator {
            normalized.push('_');
            last_was_separator = true;
        }
    }
    let normalized = normalized.trim_matches('_').to_string();
    (!normalized.is_empty()).then_some(normalized)
}

fn quickadd_choice_type(choice: &ObsidianQuickAddChoice) -> &str {
    choice.choice_type.as_deref().unwrap_or("")
}

fn quickadd_choice_identity(
    choice: &ObsidianQuickAddChoice,
    ordinal: usize,
    fallback_prefix: &str,
) -> (String, String) {
    let name = normalize_optional_text(choice.name.clone()).unwrap_or_else(|| {
        normalize_optional_text(choice.id.clone())
            .unwrap_or_else(|| format!("{fallback_prefix}-{ordinal}"))
    });
    let id = normalize_optional_text(choice.id.clone())
        .or_else(|| quickadd_slugify(&name))
        .unwrap_or_else(|| format!("{fallback_prefix}-{ordinal}"));
    (id, name)
}

fn quickadd_slugify(value: &str) -> Option<String> {
    let mut slug = String::new();
    let mut last_was_separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            slug.push('-');
            last_was_separator = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    (!slug.is_empty()).then_some(slug)
}

fn apply_partial_tasknotes_field_mapping(
    mapping: &mut TaskNotesFieldMapping,
    overrides: PartialTaskNotesFieldMapping,
) {
    if let Some(title) = overrides.title {
        mapping.title = title;
    }
    if let Some(status) = overrides.status {
        mapping.status = status;
    }
    if let Some(priority) = overrides.priority {
        mapping.priority = priority;
    }
    if let Some(due) = overrides.due {
        mapping.due = due;
    }
    if let Some(scheduled) = overrides.scheduled {
        mapping.scheduled = scheduled;
    }
    if let Some(contexts) = overrides.contexts {
        mapping.contexts = contexts;
    }
    if let Some(projects) = overrides.projects {
        mapping.projects = projects;
    }
    if let Some(time_estimate) = overrides.time_estimate {
        mapping.time_estimate = time_estimate;
    }
    if let Some(completed_date) = overrides.completed_date {
        mapping.completed_date = completed_date;
    }
    if let Some(date_created) = overrides.date_created {
        mapping.date_created = date_created;
    }
    if let Some(date_modified) = overrides.date_modified {
        mapping.date_modified = date_modified;
    }
    if let Some(recurrence) = overrides.recurrence {
        mapping.recurrence = recurrence;
    }
    if let Some(recurrence_anchor) = overrides.recurrence_anchor {
        mapping.recurrence_anchor = recurrence_anchor;
    }
    if let Some(archive_tag) = overrides.archive_tag {
        mapping.archive_tag = archive_tag;
    }
    if let Some(time_entries) = overrides.time_entries {
        mapping.time_entries = time_entries;
    }
    if let Some(complete_instances) = overrides.complete_instances {
        mapping.complete_instances = complete_instances;
    }
    if let Some(skipped_instances) = overrides.skipped_instances {
        mapping.skipped_instances = skipped_instances;
    }
    if let Some(blocked_by) = overrides.blocked_by {
        mapping.blocked_by = blocked_by;
    }
    if let Some(pomodoros) = overrides.pomodoros {
        mapping.pomodoros = pomodoros;
    }
    if let Some(reminders) = overrides.reminders {
        mapping.reminders = reminders;
    }
}

fn normalize_templater_command_pairs(
    raw_pairs: Vec<[String; 2]>,
) -> Vec<TemplaterCommandPairConfig> {
    normalize_templater_command_pairs_from_config(
        raw_pairs
            .into_iter()
            .map(|[name, command]| TemplaterCommandPairConfig { name, command })
            .collect(),
    )
}

fn normalize_templater_command_pairs_from_config(
    raw_pairs: Vec<TemplaterCommandPairConfig>,
) -> Vec<TemplaterCommandPairConfig> {
    let mut normalized = Vec::new();

    for pair in raw_pairs {
        let Some(name) = normalize_optional_text(Some(pair.name)) else {
            continue;
        };
        let Some(command) = normalize_optional_text(Some(pair.command)) else {
            continue;
        };
        if !normalized
            .iter()
            .any(|existing: &TemplaterCommandPairConfig| existing.name == name)
        {
            normalized.push(TemplaterCommandPairConfig { name, command });
        }
    }

    normalized
}

fn normalize_templater_folder_templates(
    raw_templates: Vec<ObsidianTemplaterFolderTemplateConfig>,
) -> Vec<TemplaterFolderTemplateConfig> {
    normalize_templater_folder_templates_from_config(
        raw_templates
            .into_iter()
            .map(|template| TemplaterFolderTemplateConfig {
                folder: PathBuf::from(template.folder),
                template: template.template,
            })
            .collect(),
    )
}

fn normalize_templater_folder_templates_from_config(
    raw_templates: Vec<TemplaterFolderTemplateConfig>,
) -> Vec<TemplaterFolderTemplateConfig> {
    let mut normalized = Vec::new();

    for template in raw_templates {
        let Some(folder) = normalize_template_pathbuf(&template.folder) else {
            continue;
        };
        let Some(template_name) = normalize_optional_text(Some(template.template)) else {
            continue;
        };
        if !normalized
            .iter()
            .any(|existing: &TemplaterFolderTemplateConfig| existing.folder == folder)
        {
            normalized.push(TemplaterFolderTemplateConfig {
                folder,
                template: template_name,
            });
        }
    }

    normalized
}

fn normalize_templater_file_templates(
    raw_templates: Vec<TemplaterFileTemplateConfig>,
) -> Vec<TemplaterFileTemplateConfig> {
    normalize_templater_file_templates_from_config(raw_templates)
}

fn normalize_templater_file_templates_from_config(
    raw_templates: Vec<TemplaterFileTemplateConfig>,
) -> Vec<TemplaterFileTemplateConfig> {
    let mut normalized = Vec::new();

    for template in raw_templates {
        let Some(regex) = normalize_optional_text(Some(template.regex)) else {
            continue;
        };
        let Some(template_name) = normalize_optional_text(Some(template.template)) else {
            continue;
        };
        if !normalized
            .iter()
            .any(|existing: &TemplaterFileTemplateConfig| existing.regex == regex)
        {
            normalized.push(TemplaterFileTemplateConfig {
                regex,
                template: template_name,
            });
        }
    }

    normalized
}

fn normalize_kanban_metadata_keys(
    metadata_keys: Vec<KanbanMetadataKeyConfig>,
) -> Vec<KanbanMetadataKeyConfig> {
    let mut normalized = Vec::new();

    for key in metadata_keys {
        let (normalized_key, key) = match key {
            KanbanMetadataKeyConfig::Detailed(mut field) => {
                let Some(metadata_key) =
                    normalize_optional_text(Some(std::mem::take(&mut field.metadata_key)))
                else {
                    continue;
                };
                field.metadata_key = metadata_key;
                field.label = normalize_optional_text(field.label);
                let normalized_key = field.metadata_key.clone();
                (normalized_key, KanbanMetadataKeyConfig::Detailed(field))
            }
            KanbanMetadataKeyConfig::Key(key) => {
                let Some(key) = normalize_optional_text(Some(key)) else {
                    continue;
                };
                (key.clone(), KanbanMetadataKeyConfig::Key(key))
            }
        };
        let duplicate = normalized.iter().any(|existing| match existing {
            KanbanMetadataKeyConfig::Detailed(field) => field.metadata_key == normalized_key,
            KanbanMetadataKeyConfig::Key(key) => key == &normalized_key,
        });
        if !duplicate {
            normalized.push(key);
        }
    }

    normalized
}

fn derived_kanban_archive_date_format(date_format: &str, time_format: &str) -> String {
    format!("{date_format} {time_format}")
}

fn apply_task_status_definitions(
    config: &mut TaskStatusesConfig,
    definitions: Vec<TaskStatusDefinition>,
) {
    config.todo = status_symbols_for_type(&definitions, "TODO");
    config.completed = status_symbols_for_type(&definitions, "DONE");
    config.in_progress = status_symbols_for_type(&definitions, "IN_PROGRESS");
    config.cancelled = status_symbols_for_type(&definitions, "CANCELLED");
    config.non_task = status_symbols_for_type(&definitions, "NON_TASK");
    config.definitions = definitions;
}

fn status_symbols_for_type(definitions: &[TaskStatusDefinition], status_type: &str) -> Vec<String> {
    definitions
        .iter()
        .filter(|definition| normalize_task_status_type(&definition.status_type) == status_type)
        .map(|definition| definition.symbol.clone())
        .collect()
}

fn normalize_task_status_type(value: &str) -> String {
    let mut normalized = String::new();
    let mut last_was_separator = false;

    for ch in value.trim().chars() {
        if ch.is_alphanumeric() {
            normalized.extend(ch.to_uppercase());
            last_was_separator = false;
        } else if !normalized.is_empty() && !last_was_separator {
            normalized.push('_');
            last_was_separator = true;
        }
    }

    if normalized.is_empty() {
        "UNKNOWN".to_string()
    } else {
        normalized.trim_matches('_').to_string()
    }
}

fn default_task_status_name(status_type: &str) -> String {
    match status_type {
        "TODO" => "Todo".to_string(),
        "DONE" => "Done".to_string(),
        "IN_PROGRESS" => "In Progress".to_string(),
        "CANCELLED" => "Cancelled".to_string(),
        "NON_TASK" => "Non-task".to_string(),
        _ => "Unknown".to_string(),
    }
}

fn normalize_obsidian_task_recurrence_mode(config: &ObsidianTasksConfig) -> Option<String> {
    normalize_optional_text(config.recurrence_on_completion.clone()).or_else(|| {
        config
            .recurrence_on_next_line
            .map(|next_line| (if next_line { "next-line" } else { "same-line" }).to_string())
    })
}

#[cfg(test)]
mod tests;
