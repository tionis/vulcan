use crate::paths::{ensure_vulcan_dir, VaultPaths};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG_TEMPLATE: &str = r###"# Vulcan configuration
# Settings in this file override compatible values from `.obsidian/app.json`.
# Shared vault settings belong here. Device-local overrides can go in
# `.vulcan/config.local.toml`, which is loaded after this file and ignored by
# the default `.vulcan/.gitignore`.

# [scan]
# default_mode = "blocking"   # off | blocking | background
# browse_mode = "background"  # off | blocking | background

# [chunking]
# strategy = "heading"
# target_size = 4000
# overlap = 0

# [links]
# resolution = "shortest"
# style = "wikilink"
# attachment_folder = "."

# [embedding]
# provider = "openai-compatible"
# base_url = "http://localhost:11434/v1"
# model = "text-embedding-3-small"
# api_key_env = "OPENAI_API_KEY"
# normalized = true
# max_batch_size = 32
# max_input_tokens = 8192
# max_concurrency = 4
# cache_key = "openai-compatible:text-embedding-3-small"  # optional; override to keep vectors when switching endpoints

# [extraction]
# command = "sh"
# args = ["-c", "case \"$2\" in pdf) pdftotext \"$1\" - ;; png|jpg|jpeg|webp) tesseract \"$1\" stdout ;; *) exit 0 ;; esac", "sh", "{path}", "{extension}"]
# extensions = ["pdf", "png", "jpg", "jpeg", "webp"]
# max_output_bytes = 262144

# [git]
# auto_commit = false
# trigger = "mutation"
# message = "vulcan {action}: {files}"
# scope = "vulcan-only"
# exclude = [".obsidian/workspace.json", ".obsidian/workspace-mobile.json"]

# [inbox]
# path = "Inbox.md"
# format = "- {text}"
# timestamp = true
# heading = "## Inbox"

# [tasks.statuses]
# todo = [" "]
# completed = ["x", "X"]
# in_progress = ["/"]
# cancelled = ["-"]
# non_task = []
# global_filter = "#task"
# global_query = "not done"
# remove_global_filter = true
# set_created_date = false
# recurrence_on_completion = "next-line"  # same-line | next-line
#
# [[tasks.statuses.definitions]]
# symbol = "!"
# name = "Important"
# type = "TODO"
# next_symbol = "x"

# [dataview]
# inline_query_prefix = "="
# inline_js_query_prefix = "$="
# enable_dataview_js = true
# enable_inline_dataview_js = false
# task_completion_tracking = false
# task_completion_use_emoji_shorthand = false
# task_completion_text = "completion"
# recursive_subtask_completion = false
# display_result_count = true
# default_date_format = "MMMM dd, yyyy"
# default_datetime_format = "h:mm a - MMMM dd, yyyy"
# max_recursive_render_depth = 4
# primary_column_name = "File"
# group_column_name = "Group"

# [templates]
# date_format = "YYYY-MM-DD"
# time_format = "HH:mm"
"###;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutoScanMode {
    Off,
    Blocking,
    Background,
}

impl Default for AutoScanMode {
    fn default() -> Self {
        Self::Blocking
    }
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
pub struct TemplatesConfig {
    pub date_format: String,
    pub time_format: String,
    pub obsidian_folder: Option<PathBuf>,
}

impl Default for TemplatesConfig {
    fn default() -> Self {
        Self {
            date_format: "YYYY-MM-DD".to_string(),
            time_format: "HH:mm".to_string(),
            obsidian_folder: None,
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
    #[serde(default = "default_dataview_max_recursive_render_depth")]
    pub max_recursive_render_depth: usize,
    #[serde(default = "default_dataview_primary_column_name")]
    pub primary_column_name: String,
    #[serde(default = "default_dataview_group_column_name")]
    pub group_column_name: String,
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
            max_recursive_render_depth: default_dataview_max_recursive_render_depth(),
            primary_column_name: default_dataview_primary_column_name(),
            group_column_name: default_dataview_group_column_name(),
        }
    }
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
    pub dataview: DataviewConfig,
    pub templates: TemplatesConfig,
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
            dataview: DataviewConfig::default(),
            templates: TemplatesConfig::default(),
        }
    }
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigImportMapping {
    pub source: String,
    pub target: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigImportReport {
    pub plugin: String,
    pub source_path: PathBuf,
    pub config_path: PathBuf,
    pub created_config: bool,
    pub updated: bool,
    pub mappings: Vec<ConfigImportMapping>,
}

#[derive(Debug)]
pub enum ConfigImportError {
    Io(std::io::Error),
    Json(serde_json::Error),
    MissingSource(PathBuf),
    TomlDeserialize(toml::de::Error),
    TomlSerialize(toml::ser::Error),
    InvalidConfig(String),
}

impl Display for ConfigImportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::MissingSource(path) => write!(
                formatter,
                "missing Tasks plugin config at {}",
                path.display()
            ),
            Self::TomlDeserialize(error) => write!(formatter, "{error}"),
            Self::TomlSerialize(error) => write!(formatter, "{error}"),
            Self::InvalidConfig(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ConfigImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::TomlDeserialize(error) => Some(error),
            Self::TomlSerialize(error) => Some(error),
            Self::MissingSource(_) | Self::InvalidConfig(_) => None,
        }
    }
}

impl From<std::io::Error> for ConfigImportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ConfigImportError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<toml::de::Error> for ConfigImportError {
    fn from(error: toml::de::Error) -> Self {
        Self::TomlDeserialize(error)
    }
}

impl From<toml::ser::Error> for ConfigImportError {
    fn from(error: toml::ser::Error) -> Self {
        Self::TomlSerialize(error)
    }
}

#[derive(Debug, Deserialize, Default)]
struct PartialVulcanConfig {
    scan: Option<PartialScanConfig>,
    chunking: Option<PartialChunkingConfig>,
    links: Option<PartialLinksConfig>,
    embedding: Option<EmbeddingProviderConfig>,
    extraction: Option<AttachmentExtractionConfig>,
    git: Option<PartialGitConfig>,
    inbox: Option<PartialInboxConfig>,
    tasks: Option<PartialTasksConfig>,
    dataview: Option<PartialDataviewConfig>,
    templates: Option<PartialTemplatesConfig>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialScanConfig {
    default_mode: Option<AutoScanMode>,
    browse_mode: Option<AutoScanMode>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialChunkingConfig {
    strategy: Option<ChunkingStrategy>,
    target_size: Option<usize>,
    overlap: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialLinksConfig {
    resolution: Option<LinkResolutionMode>,
    style: Option<LinkStylePreference>,
    attachment_folder: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialGitConfig {
    auto_commit: Option<bool>,
    trigger: Option<GitTrigger>,
    message: Option<String>,
    scope: Option<GitScope>,
    exclude: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialInboxConfig {
    path: Option<String>,
    format: Option<String>,
    timestamp: Option<bool>,
    heading: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialTemplatesConfig {
    date_format: Option<String>,
    time_format: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialTasksConfig {
    statuses: Option<PartialTaskStatusesConfig>,
    global_filter: Option<String>,
    global_query: Option<String>,
    remove_global_filter: Option<bool>,
    set_created_date: Option<bool>,
    recurrence_on_completion: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialTaskStatusesConfig {
    todo: Option<Vec<String>>,
    completed: Option<Vec<String>>,
    in_progress: Option<Vec<String>>,
    cancelled: Option<Vec<String>>,
    non_task: Option<Vec<String>>,
    definitions: Option<Vec<TaskStatusDefinition>>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialDataviewConfig {
    inline_query_prefix: Option<String>,
    inline_js_query_prefix: Option<String>,
    enable_dataview_js: Option<bool>,
    enable_inline_dataview_js: Option<bool>,
    task_completion_tracking: Option<bool>,
    task_completion_use_emoji_shorthand: Option<bool>,
    task_completion_text: Option<String>,
    recursive_subtask_completion: Option<bool>,
    display_result_count: Option<bool>,
    default_date_format: Option<String>,
    default_datetime_format: Option<String>,
    max_recursive_render_depth: Option<usize>,
    primary_column_name: Option<String>,
    group_column_name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianAppConfig {
    #[serde(rename = "useMarkdownLinks")]
    use_markdown_links: Option<bool>,
    #[serde(rename = "newLinkFormat")]
    new_link_format: Option<LinkResolutionMode>,
    #[serde(rename = "attachmentFolderPath")]
    attachment_folder_path: Option<String>,
    #[serde(rename = "strictLineBreaks")]
    strict_line_breaks: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianTemplatesConfig {
    #[serde(rename = "dateFormat")]
    date_format: Option<String>,
    #[serde(rename = "timeFormat")]
    time_format: Option<String>,
    #[serde(
        rename = "folder",
        alias = "templateFolder",
        alias = "folderPath",
        alias = "templateFolderPath"
    )]
    folder: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianDataviewConfig {
    #[serde(rename = "inlineQueryPrefix")]
    inline_query_prefix: Option<String>,
    #[serde(rename = "inlineJsQueryPrefix")]
    inline_js_query_prefix: Option<String>,
    #[serde(rename = "enableDataviewJs")]
    enable_dataview_js: Option<bool>,
    #[serde(rename = "enableInlineDataviewJs")]
    enable_inline_dataview_js: Option<bool>,
    #[serde(rename = "taskCompletionTracking")]
    task_completion_tracking: Option<bool>,
    #[serde(rename = "taskCompletionUseEmojiShorthand")]
    task_completion_use_emoji_shorthand: Option<bool>,
    #[serde(rename = "taskCompletionText")]
    task_completion_text: Option<String>,
    #[serde(rename = "recursiveSubTaskCompletion")]
    recursive_subtask_completion: Option<bool>,
    #[serde(rename = "displayResultCount", alias = "showResultCount")]
    display_result_count: Option<bool>,
    #[serde(rename = "defaultDateFormat")]
    default_date_format: Option<String>,
    #[serde(rename = "defaultDateTimeFormat")]
    default_datetime_format: Option<String>,
    #[serde(rename = "maxRecursiveRenderDepth")]
    max_recursive_render_depth: Option<usize>,
    #[serde(rename = "primaryColumnName", alias = "tableIdColumnName")]
    primary_column_name: Option<String>,
    #[serde(rename = "groupColumnName", alias = "tableGroupColumnName")]
    group_column_name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianTasksConfig {
    #[serde(rename = "globalFilter")]
    global_filter: Option<String>,
    #[serde(rename = "globalQuery")]
    global_query: Option<String>,
    #[serde(rename = "removeGlobalFilter")]
    remove_global_filter: Option<bool>,
    #[serde(rename = "setCreatedDate")]
    set_created_date: Option<bool>,
    #[serde(rename = "recurrenceOnCompletion")]
    recurrence_on_completion: Option<String>,
    #[serde(rename = "recurrenceOnNextLine")]
    recurrence_on_next_line: Option<bool>,
    #[serde(rename = "statusSettings")]
    status_settings: Option<ObsidianTasksStatusSettings>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianTasksStatusSettings {
    #[serde(rename = "coreStatuses", default)]
    core_statuses: Vec<TaskStatusDefinition>,
    #[serde(rename = "customStatuses", default)]
    custom_statuses: Vec<TaskStatusDefinition>,
}

#[must_use]
pub fn default_config_template() -> &'static str {
    DEFAULT_CONFIG_TEMPLATE
}

fn default_attachment_extraction_extensions() -> Vec<String> {
    [
        "pdf", "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

fn default_todo_task_statuses() -> Vec<String> {
    vec![" ".to_string()]
}

fn default_completed_task_statuses() -> Vec<String> {
    vec!["x".to_string(), "X".to_string()]
}

fn default_in_progress_task_statuses() -> Vec<String> {
    vec!["/".to_string()]
}

fn default_cancelled_task_statuses() -> Vec<String> {
    vec!["-".to_string()]
}

fn default_non_task_statuses() -> Vec<String> {
    Vec::new()
}

fn default_dataview_inline_query_prefix() -> String {
    "=".to_string()
}

fn default_dataview_inline_js_query_prefix() -> String {
    "$=".to_string()
}

fn default_dataview_enable_dataview_js() -> bool {
    true
}

fn default_dataview_enable_inline_dataview_js() -> bool {
    false
}

fn default_dataview_task_completion_tracking() -> bool {
    false
}

fn default_dataview_task_completion_use_emoji_shorthand() -> bool {
    false
}

fn default_dataview_task_completion_text() -> String {
    "completion".to_string()
}

fn default_dataview_recursive_subtask_completion() -> bool {
    false
}

fn default_dataview_display_result_count() -> bool {
    true
}

fn default_dataview_default_date_format() -> String {
    "MMMM dd, yyyy".to_string()
}

fn default_dataview_default_datetime_format() -> String {
    "h:mm a - MMMM dd, yyyy".to_string()
}

fn default_dataview_max_recursive_render_depth() -> usize {
    4
}

fn default_dataview_primary_column_name() -> String {
    "File".to_string()
}

fn default_dataview_group_column_name() -> String {
    "Group".to_string()
}

pub fn create_default_config(paths: &VaultPaths) -> Result<bool, std::io::Error> {
    ensure_vulcan_dir(paths)?;

    if paths.config_file().exists() {
        return Ok(false);
    }

    fs::write(paths.config_file(), default_config_template())?;
    Ok(true)
}

pub fn import_tasks_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    let source_path = paths
        .vault_root()
        .join(".obsidian/plugins/obsidian-tasks-plugin/data.json");
    if !source_path.exists() {
        return Err(ConfigImportError::MissingSource(source_path));
    }

    let obsidian = serde_json::from_str::<ObsidianTasksConfig>(&fs::read_to_string(&source_path)?)?;
    let imported_tasks = imported_tasks_config(obsidian);
    let mappings = tasks_config_import_mappings(&imported_tasks)?;

    ensure_vulcan_dir(paths)?;
    let config_path = paths.config_file().to_path_buf();
    let created_config = !config_path.exists();
    let existing_contents = fs::read_to_string(&config_path).ok();
    let mut config_value = load_config_value(&config_path)?;
    write_tasks_import(&mut config_value, &imported_tasks)?;
    let rendered = toml::to_string_pretty(&config_value)?;
    let updated = existing_contents.as_deref() != Some(rendered.as_str());
    if updated {
        fs::write(&config_path, rendered)?;
    }

    Ok(ConfigImportReport {
        plugin: "tasks".to_string(),
        source_path,
        config_path,
        created_config,
        updated,
        mappings,
    })
}

#[must_use]
pub fn load_vault_config(paths: &VaultPaths) -> ConfigLoadResult {
    let mut config = VaultConfig::default();
    let mut diagnostics = Vec::new();

    if let Some(obsidian_app) = load_obsidian_app_config(paths, &mut diagnostics) {
        apply_obsidian_defaults(&mut config, obsidian_app);
    }

    if let Some(obsidian_templates) = load_obsidian_templates_config(paths, &mut diagnostics) {
        apply_obsidian_template_defaults(&mut config, obsidian_templates);
    }

    if let Some(obsidian_dataview) = load_obsidian_dataview_config(paths, &mut diagnostics) {
        apply_obsidian_dataview_defaults(&mut config, obsidian_dataview);
    }

    if let Some(obsidian_tasks) = load_obsidian_tasks_config(paths, &mut diagnostics) {
        apply_obsidian_tasks_defaults(&mut config, obsidian_tasks);
    }

    config.property_types = load_obsidian_property_types(paths, &mut diagnostics);

    if let Some(vulcan_config) =
        load_vulcan_overrides(paths.config_file(), "Vulcan config", &mut diagnostics)
    {
        apply_vulcan_overrides(&mut config, vulcan_config);
    }

    if let Some(local_config) = load_vulcan_overrides(
        paths.local_config_file(),
        "local Vulcan config",
        &mut diagnostics,
    ) {
        apply_vulcan_overrides(&mut config, local_config);
    }

    ConfigLoadResult {
        config,
        diagnostics,
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

    if let Value::Object(entries) = value {
        entries
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
            .collect()
    } else {
        diagnostics.push(ConfigDiagnostic {
            path,
            message: "expected a JSON object of property types".to_string(),
        });
        BTreeMap::new()
    }
}

fn load_obsidian_templates_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianTemplatesConfig> {
    let path = paths.vault_root().join(".obsidian/templates.json");

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

fn imported_tasks_config(obsidian: ObsidianTasksConfig) -> TasksConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_tasks_defaults(&mut config, obsidian);
    config.tasks
}

fn tasks_config_import_mappings(
    config: &TasksConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let status_source = "statusSettings.coreStatuses + statusSettings.customStatuses";
    Ok(vec![
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.todo".to_string(),
            value: serde_json::to_value(&config.statuses.todo)?,
        },
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.completed".to_string(),
            value: serde_json::to_value(&config.statuses.completed)?,
        },
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.in_progress".to_string(),
            value: serde_json::to_value(&config.statuses.in_progress)?,
        },
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.cancelled".to_string(),
            value: serde_json::to_value(&config.statuses.cancelled)?,
        },
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.non_task".to_string(),
            value: serde_json::to_value(&config.statuses.non_task)?,
        },
        ConfigImportMapping {
            source: status_source.to_string(),
            target: "tasks.statuses.definitions".to_string(),
            value: serde_json::to_value(&config.statuses.definitions)?,
        },
        ConfigImportMapping {
            source: "globalFilter".to_string(),
            target: "tasks.global_filter".to_string(),
            value: serde_json::to_value(&config.global_filter)?,
        },
        ConfigImportMapping {
            source: "globalQuery".to_string(),
            target: "tasks.global_query".to_string(),
            value: serde_json::to_value(&config.global_query)?,
        },
        ConfigImportMapping {
            source: "removeGlobalFilter".to_string(),
            target: "tasks.remove_global_filter".to_string(),
            value: Value::Bool(config.remove_global_filter),
        },
        ConfigImportMapping {
            source: "setCreatedDate".to_string(),
            target: "tasks.set_created_date".to_string(),
            value: Value::Bool(config.set_created_date),
        },
        ConfigImportMapping {
            source: "recurrenceOnCompletion".to_string(),
            target: "tasks.recurrence_on_completion".to_string(),
            value: serde_json::to_value(&config.recurrence_on_completion)?,
        },
    ])
}

fn load_config_value(path: &Path) -> Result<toml::Value, ConfigImportError> {
    if !path.exists() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }

    let contents = fs::read_to_string(path)?;
    if contents.trim().is_empty() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }

    let value = toml::from_str::<toml::Value>(&contents)?;
    if value.is_table() {
        Ok(value)
    } else {
        Err(ConfigImportError::InvalidConfig(
            "expected .vulcan/config.toml to contain a TOML table".to_string(),
        ))
    }
}

fn write_tasks_import(
    config_value: &mut toml::Value,
    tasks: &TasksConfig,
) -> Result<(), ConfigImportError> {
    let Some(root_table) = config_value.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(
            "expected .vulcan/config.toml to contain a TOML table".to_string(),
        ));
    };

    let tasks_entry = root_table
        .entry("tasks".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !tasks_entry.is_table() {
        *tasks_entry = toml::Value::Table(toml::map::Map::new());
    }
    let Some(tasks_table) = tasks_entry.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(
            "expected [tasks] to be a TOML table".to_string(),
        ));
    };

    write_optional_toml_string(tasks_table, "global_filter", tasks.global_filter.as_deref());
    write_optional_toml_string(tasks_table, "global_query", tasks.global_query.as_deref());
    tasks_table.insert(
        "remove_global_filter".to_string(),
        toml::Value::Boolean(tasks.remove_global_filter),
    );
    tasks_table.insert(
        "set_created_date".to_string(),
        toml::Value::Boolean(tasks.set_created_date),
    );
    write_optional_toml_string(
        tasks_table,
        "recurrence_on_completion",
        tasks.recurrence_on_completion.as_deref(),
    );

    let statuses_entry = tasks_table
        .entry("statuses".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !statuses_entry.is_table() {
        *statuses_entry = toml::Value::Table(toml::map::Map::new());
    }
    let Some(statuses_table) = statuses_entry.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(
            "expected [tasks.statuses] to be a TOML table".to_string(),
        ));
    };

    write_string_array(statuses_table, "todo", &tasks.statuses.todo);
    write_string_array(statuses_table, "completed", &tasks.statuses.completed);
    write_string_array(statuses_table, "in_progress", &tasks.statuses.in_progress);
    write_string_array(statuses_table, "cancelled", &tasks.statuses.cancelled);
    write_string_array(statuses_table, "non_task", &tasks.statuses.non_task);
    statuses_table.insert(
        "definitions".to_string(),
        toml::Value::try_from(&tasks.statuses.definitions)?,
    );

    Ok(())
}

fn write_optional_toml_string(
    table: &mut toml::map::Map<String, toml::Value>,
    key: &str,
    value: Option<&str>,
) {
    match value {
        Some(value) => {
            table.insert(key.to_string(), toml::Value::String(value.to_string()));
        }
        None => {
            table.remove(key);
        }
    }
}

fn write_string_array(
    table: &mut toml::map::Map<String, toml::Value>,
    key: &str,
    values: &[String],
) {
    table.insert(
        key.to_string(),
        toml::Value::Array(values.iter().cloned().map(toml::Value::String).collect()),
    );
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
        config.templates.obsidian_folder = Some(normalize_template_folder(&folder));
    }
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
        config.embedding = Some(embedding);
    }
    if let Some(extraction) = overrides.extraction {
        config.extraction = Some(extraction);
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
        if let Some(depth) = dataview.max_recursive_render_depth {
            config.dataview.max_recursive_render_depth = depth;
        }
        if let Some(name) = dataview.primary_column_name {
            config.dataview.primary_column_name = name;
        }
        if let Some(name) = dataview.group_column_name {
            config.dataview.group_column_name = name;
        }
    }

    if let Some(templates) = overrides.templates {
        if let Some(date_format) = templates.date_format {
            config.templates.date_format = date_format;
        }
        if let Some(time_format) = templates.time_format {
            config.templates.time_format = time_format;
        }
    }
}

fn normalize_attachment_folder(path: &str) -> PathBuf {
    if path == "/" || path.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(path)
    }
}

fn normalize_template_folder(path: &str) -> PathBuf {
    PathBuf::from(path.trim_matches('/'))
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn missing_files_use_builtin_defaults() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        let loaded = load_vault_config(&paths);

        assert_eq!(loaded.config, VaultConfig::default());
        assert!(loaded.diagnostics.is_empty());
    }

    #[test]
    fn obsidian_settings_seed_defaults_and_property_types() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian")).expect("obsidian dir should be created");
        fs::write(
            vault_root.join(".obsidian/app.json"),
            r#"{
              "useMarkdownLinks": true,
              "newLinkFormat": "relative",
              "attachmentFolderPath": "/",
              "strictLineBreaks": true
            }"#,
        )
        .expect("app config should be written");
        fs::write(
            vault_root.join(".obsidian/types.json"),
            r#"{
              "status": "text",
              "priority": { "type": "number" }
            }"#,
        )
        .expect("types config should be written");
        fs::write(
            vault_root.join(".obsidian/templates.json"),
            r#"{
              "folder": "Shared Templates",
              "dateFormat": "dddd, MMMM Do YYYY",
              "timeFormat": "hh:mm A"
            }"#,
        )
        .expect("templates config should be written");
        fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
            .expect("dataview plugin dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/dataview/data.json"),
            r#"{
              "inlineQueryPrefix": "dv:",
              "inlineJsQueryPrefix": "$dv:",
              "enableDataviewJs": false,
              "enableInlineDataviewJs": true,
              "taskCompletionTracking": true,
              "taskCompletionUseEmojiShorthand": true,
              "taskCompletionText": "done-on",
              "recursiveSubTaskCompletion": true,
              "showResultCount": false,
              "defaultDateFormat": "yyyy-MM-dd",
              "defaultDateTimeFormat": "yyyy-MM-dd HH:mm",
              "maxRecursiveRenderDepth": 7,
              "tableIdColumnName": "Document",
              "tableGroupColumnName": "Bucket"
            }"#,
        )
        .expect("dataview config should be written");
        let paths = VaultPaths::new(vault_root);

        let loaded = load_vault_config(&paths);

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(loaded.config.link_style, LinkStylePreference::Markdown);
        assert_eq!(loaded.config.link_resolution, LinkResolutionMode::Relative);
        assert_eq!(loaded.config.attachment_folder, PathBuf::from("."));
        assert!(loaded.config.strict_line_breaks);
        assert_eq!(loaded.config.scan.default_mode, AutoScanMode::Blocking);
        assert_eq!(loaded.config.scan.browse_mode, AutoScanMode::Background);
        assert_eq!(loaded.config.templates.date_format, "dddd, MMMM Do YYYY");
        assert_eq!(loaded.config.templates.time_format, "hh:mm A");
        assert_eq!(
            loaded.config.templates.obsidian_folder,
            Some(PathBuf::from("Shared Templates"))
        );
        assert_eq!(
            loaded.config.property_types.get("status"),
            Some(&"text".to_string())
        );
        assert_eq!(
            loaded.config.property_types.get("priority"),
            Some(&"number".to_string())
        );
        assert_eq!(loaded.config.dataview.inline_query_prefix, "dv:");
        assert_eq!(loaded.config.dataview.inline_js_query_prefix, "$dv:");
        assert!(!loaded.config.dataview.enable_dataview_js);
        assert!(loaded.config.dataview.enable_inline_dataview_js);
        assert!(loaded.config.dataview.task_completion_tracking);
        assert!(loaded.config.dataview.task_completion_use_emoji_shorthand);
        assert_eq!(loaded.config.dataview.task_completion_text, "done-on");
        assert!(loaded.config.dataview.recursive_subtask_completion);
        assert!(!loaded.config.dataview.display_result_count);
        assert_eq!(loaded.config.dataview.default_date_format, "yyyy-MM-dd");
        assert_eq!(
            loaded.config.dataview.default_datetime_format,
            "yyyy-MM-dd HH:mm"
        );
        assert_eq!(loaded.config.dataview.max_recursive_render_depth, 7);
        assert_eq!(loaded.config.dataview.primary_column_name, "Document");
        assert_eq!(loaded.config.dataview.group_column_name, "Bucket");
    }

    #[test]
    fn vulcan_config_overrides_obsidian_values() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian")).expect("obsidian dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".obsidian/app.json"),
            r#"{
              "useMarkdownLinks": true,
              "newLinkFormat": "relative",
              "attachmentFolderPath": "attachments"
            }"#,
        )
        .expect("app config should be written");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r###"[scan]
default_mode = "off"
browse_mode = "blocking"

[chunking]
strategy = "fixed"
target_size = 512
overlap = 64

[links]
resolution = "absolute"
style = "wikilink"
attachment_folder = "assets"

[embedding]
provider = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "nomic-embed-text"
api_key_env = "EMBEDDING_API_KEY"
normalized = false
max_batch_size = 8
max_input_tokens = 2048
max_concurrency = 2

[extraction]
command = "sh"
args = ["-c", "cat \"$1.txt\"", "sh", "{path}"]
extensions = ["pdf", "png"]
max_output_bytes = 4096

[git]
auto_commit = true
trigger = "scan"
message = "vault sync: {count}"
scope = "all"
exclude = [".obsidian/workspace.json"]

[inbox]
path = "Capture/Inbox.md"
format = "* {datetime} {text}"
timestamp = false
heading = "## Notes"

[tasks]
global_filter = "#work"
global_query = "not done"
remove_global_filter = true
set_created_date = true
recurrence_on_completion = "next-line"

[tasks.statuses]
todo = [" ", "!"]
completed = ["x", "v"]
in_progress = ["/", ">"]
cancelled = ["-"]

[dataview]
inline_query_prefix = "inline:"
inline_js_query_prefix = "$inline:"
enable_dataview_js = false
enable_inline_dataview_js = true
task_completion_tracking = true
task_completion_use_emoji_shorthand = true
task_completion_text = "done-on"
recursive_subtask_completion = true
display_result_count = false
default_date_format = "yyyy-MM-dd"
default_datetime_format = "yyyy-MM-dd HH:mm"
max_recursive_render_depth = 8
primary_column_name = "Document"
group_column_name = "Bucket"

[templates]
date_format = "DD/MM/YYYY"
time_format = "HH:mm:ss"
"###,
        )
        .expect("vulcan config should be written");
        let paths = VaultPaths::new(vault_root);

        let loaded = load_vault_config(&paths);

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(loaded.config.scan.default_mode, AutoScanMode::Off);
        assert_eq!(loaded.config.scan.browse_mode, AutoScanMode::Blocking);
        assert_eq!(loaded.config.chunking.strategy, ChunkingStrategy::Fixed);
        assert_eq!(loaded.config.chunking.target_size, 512);
        assert_eq!(loaded.config.chunking.overlap, 64);
        assert_eq!(loaded.config.link_resolution, LinkResolutionMode::Absolute);
        assert_eq!(loaded.config.link_style, LinkStylePreference::Wikilink);
        assert_eq!(loaded.config.attachment_folder, PathBuf::from("assets"));
        assert_eq!(
            loaded
                .config
                .embedding
                .as_ref()
                .expect("embedding config should be present")
                .model,
            "nomic-embed-text"
        );
        assert_eq!(
            loaded
                .config
                .embedding
                .as_ref()
                .expect("embedding config should be present")
                .provider_name(),
            "openai-compatible"
        );
        assert_eq!(
            loaded
                .config
                .extraction
                .as_ref()
                .expect("extraction config should be present")
                .extensions,
            vec!["pdf".to_string(), "png".to_string()]
        );
        assert!(loaded.config.git.auto_commit);
        assert_eq!(loaded.config.git.trigger, GitTrigger::Scan);
        assert_eq!(loaded.config.git.message, "vault sync: {count}");
        assert_eq!(loaded.config.git.scope, GitScope::All);
        assert_eq!(
            loaded.config.git.exclude,
            vec![".obsidian/workspace.json".to_string()]
        );
        assert_eq!(loaded.config.inbox.path, "Capture/Inbox.md");
        assert_eq!(loaded.config.inbox.format, "* {datetime} {text}");
        assert!(!loaded.config.inbox.timestamp);
        assert_eq!(loaded.config.inbox.heading.as_deref(), Some("## Notes"));
        assert_eq!(loaded.config.tasks.global_filter, Some("#work".to_string()));
        assert_eq!(
            loaded.config.tasks.global_query,
            Some("not done".to_string())
        );
        assert!(loaded.config.tasks.remove_global_filter);
        assert!(loaded.config.tasks.set_created_date);
        assert_eq!(
            loaded.config.tasks.recurrence_on_completion,
            Some("next-line".to_string())
        );
        assert_eq!(
            loaded.config.tasks.statuses.todo,
            vec![" ".to_string(), "!".to_string()]
        );
        assert_eq!(
            loaded.config.tasks.statuses.completed,
            vec!["x".to_string(), "v".to_string()]
        );
        assert_eq!(
            loaded.config.tasks.statuses.in_progress,
            vec!["/".to_string(), ">".to_string()]
        );
        assert_eq!(
            loaded.config.tasks.statuses.cancelled,
            vec!["-".to_string()]
        );
        assert!(loaded.config.tasks.statuses.non_task.is_empty());
        assert_eq!(loaded.config.dataview.inline_query_prefix, "inline:");
        assert_eq!(loaded.config.dataview.inline_js_query_prefix, "$inline:");
        assert!(!loaded.config.dataview.enable_dataview_js);
        assert!(loaded.config.dataview.enable_inline_dataview_js);
        assert!(loaded.config.dataview.task_completion_tracking);
        assert!(loaded.config.dataview.task_completion_use_emoji_shorthand);
        assert_eq!(loaded.config.dataview.task_completion_text, "done-on");
        assert!(loaded.config.dataview.recursive_subtask_completion);
        assert!(!loaded.config.dataview.display_result_count);
        assert_eq!(loaded.config.dataview.default_date_format, "yyyy-MM-dd");
        assert_eq!(
            loaded.config.dataview.default_datetime_format,
            "yyyy-MM-dd HH:mm"
        );
        assert_eq!(loaded.config.dataview.max_recursive_render_depth, 8);
        assert_eq!(loaded.config.dataview.primary_column_name, "Document");
        assert_eq!(loaded.config.dataview.group_column_name, "Bucket");
        assert_eq!(loaded.config.templates.date_format, "DD/MM/YYYY");
        assert_eq!(loaded.config.templates.time_format, "HH:mm:ss");
    }

    #[test]
    fn malformed_vulcan_config_emits_diagnostic_and_uses_fallbacks() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(vault_root.join(".vulcan/config.toml"), "[chunking")
            .expect("broken config should be written");
        let paths = VaultPaths::new(vault_root);

        let loaded = load_vault_config(&paths);

        assert_eq!(loaded.config, VaultConfig::default());
        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0]
            .message
            .contains("failed to parse Vulcan config"));
    }

    #[test]
    fn local_config_overrides_shared_config() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r###"[scan]
default_mode = "off"
browse_mode = "off"

[chunking]
target_size = 512

[git]
auto_commit = false

[inbox]
path = "Inbox.md"

[tasks.statuses]
completed = ["x"]

[templates]
date_format = "YYYY-MM-DD"
"###,
        )
        .expect("shared config should be written");
        fs::write(
            vault_root.join(".vulcan/config.local.toml"),
            r###"[scan]
default_mode = "blocking"
browse_mode = "background"

[chunking]
target_size = 2048

[git]
auto_commit = true

[inbox]
path = "Device/Inbox.md"

[tasks.statuses]
completed = ["x", "X", "v"]

[templates]
date_format = "DD.MM.YYYY"
time_format = "HH:mm:ss"
"###,
        )
        .expect("local config should be written");

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(loaded.config.scan.default_mode, AutoScanMode::Blocking);
        assert_eq!(loaded.config.scan.browse_mode, AutoScanMode::Background);
        assert_eq!(loaded.config.chunking.target_size, 2_048);
        assert!(loaded.config.git.auto_commit);
        assert_eq!(loaded.config.inbox.path, "Device/Inbox.md");
        assert_eq!(
            loaded.config.tasks.statuses.completed,
            vec!["x".to_string(), "X".to_string(), "v".to_string()]
        );
        assert_eq!(loaded.config.templates.date_format, "DD.MM.YYYY");
        assert_eq!(loaded.config.templates.time_format, "HH:mm:ss");
    }

    #[test]
    fn task_status_defaults_and_completion_mapping_are_configurable() {
        let defaults = TaskStatusesConfig::default();
        assert_eq!(
            defaults.completion_state(" "),
            TaskCompletionState {
                checked: false,
                completed: false,
            }
        );
        assert_eq!(
            defaults.completion_state("x"),
            TaskCompletionState {
                checked: true,
                completed: true,
            }
        );
        assert_eq!(
            defaults.completion_state("/"),
            TaskCompletionState {
                checked: true,
                completed: false,
            }
        );

        let custom = TaskStatusesConfig {
            todo: vec![" ".to_string(), "!".to_string()],
            completed: vec!["x".to_string(), "v".to_string()],
            in_progress: vec!["/".to_string()],
            cancelled: vec!["-".to_string()],
            non_task: vec!["~".to_string()],
            definitions: Vec::new(),
        };
        assert_eq!(
            custom.completion_state("!"),
            TaskCompletionState {
                checked: false,
                completed: false,
            }
        );
        assert_eq!(
            custom.completion_state("v"),
            TaskCompletionState {
                checked: true,
                completed: true,
            }
        );
        assert_eq!(custom.status_state("~").status_type, "NON_TASK");
        assert_eq!(custom.status_state("?").name, "Unknown");
    }

    #[test]
    fn tasks_plugin_status_settings_seed_named_status_definitions() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/obsidian-tasks-plugin"))
            .expect("tasks plugin dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/obsidian-tasks-plugin/data.json"),
            r##"{
              "globalFilter": "#task",
              "globalQuery": "",
              "removeGlobalFilter": true,
              "setCreatedDate": true,
              "recurrenceOnNextLine": false,
              "statusSettings": {
                "coreStatuses": [
                  { "symbol": " ", "name": "Todo", "type": "TODO", "nextStatusSymbol": ">" },
                  { "symbol": "x", "name": "Done", "type": "DONE", "nextStatusSymbol": " " }
                ],
                "customStatuses": [
                  { "symbol": ">", "name": "Waiting", "type": "IN_PROGRESS", "nextStatusSymbol": "x" },
                  { "symbol": "~", "name": "Parked", "type": "NON_TASK" }
                ]
              }
            }"##,
        )
        .expect("tasks config should be written");

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(loaded.config.tasks.global_filter, Some("#task".to_string()));
        assert_eq!(loaded.config.tasks.global_query, None);
        assert!(loaded.config.tasks.remove_global_filter);
        assert!(loaded.config.tasks.set_created_date);
        assert_eq!(
            loaded.config.tasks.recurrence_on_completion,
            Some("same-line".to_string())
        );
        assert_eq!(
            loaded.config.tasks.statuses.in_progress,
            vec![">".to_string()]
        );
        assert_eq!(loaded.config.tasks.statuses.non_task, vec!["~".to_string()]);
        assert_eq!(loaded.config.tasks.statuses.definitions.len(), 4);
        assert_eq!(
            loaded.config.tasks.statuses.status_state(">").name,
            "Waiting".to_string()
        );
        assert_eq!(
            loaded.config.tasks.statuses.status_state(">").status_type,
            "IN_PROGRESS".to_string()
        );
        assert_eq!(
            loaded.config.tasks.statuses.status_state(">").next_symbol,
            Some("x".to_string())
        );
    }

    #[test]
    fn vulcan_task_status_definitions_support_names_and_next_symbols() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r###"[tasks.statuses]
todo = [" "]
completed = ["x"]

[[tasks.statuses.definitions]]
symbol = "!"
name = "Important"
type = "TODO"
next_symbol = "x"
"###,
        )
        .expect("config should be written");

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        let state = loaded.config.tasks.statuses.status_state("!");
        assert_eq!(state.name, "Important");
        assert_eq!(state.status_type, "TODO");
        assert_eq!(state.next_symbol, Some("x".to_string()));
        assert!(!state.checked);
        assert!(!state.completed);
    }

    #[test]
    fn malformed_local_config_emits_diagnostic_and_keeps_shared_config() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r###"[scan]
default_mode = "off"
"###,
        )
        .expect("shared config should be written");
        fs::write(vault_root.join(".vulcan/config.local.toml"), "[scan")
            .expect("broken local config should be written");

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert_eq!(loaded.config.scan.default_mode, AutoScanMode::Off);
        assert_eq!(loaded.config.scan.browse_mode, AutoScanMode::Background);
        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0]
            .message
            .contains("failed to parse local Vulcan config"));
    }

    #[test]
    fn create_default_config_is_idempotent() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        assert!(create_default_config(&paths).expect("config should be created"));
        assert!(!create_default_config(&paths).expect("config creation should be idempotent"));
        assert_eq!(
            fs::read_to_string(paths.config_file()).expect("config file should exist"),
            default_config_template()
        );
        assert_eq!(
            fs::read_to_string(paths.gitignore_file()).expect("gitignore should exist"),
            "*\n!.gitignore\n!config.toml\nconfig.local.toml\n!reports/\nreports/*\n!reports/*.toml\n"
        );
    }

    #[test]
    fn import_tasks_plugin_config_preserves_existing_sections_and_is_idempotent() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/obsidian-tasks-plugin"))
            .expect("tasks plugin dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/obsidian-tasks-plugin/data.json"),
            r##"{
              "globalFilter": "#task",
              "globalQuery": "not done",
              "removeGlobalFilter": true,
              "setCreatedDate": true,
              "recurrenceOnCompletion": "next-line",
              "statusSettings": {
                "coreStatuses": [
                  { "symbol": " ", "name": "Todo", "type": "TODO", "nextStatusSymbol": ">" },
                  { "symbol": "x", "name": "Done", "type": "DONE", "nextStatusSymbol": " " }
                ],
                "customStatuses": [
                  { "symbol": ">", "name": "Waiting", "type": "IN_PROGRESS", "nextStatusSymbol": "x" },
                  { "symbol": "~", "name": "Parked", "type": "NON_TASK" }
                ]
              }
            }"##,
        )
        .expect("tasks config should be written");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("existing config should be written");
        let paths = VaultPaths::new(vault_root);

        let report = import_tasks_plugin_config(&paths).expect("import should succeed");

        assert_eq!(report.plugin, "tasks");
        assert!(!report.created_config);
        assert!(report.updated);
        assert!(report
            .mappings
            .iter()
            .any(|mapping| mapping.target == "tasks.global_filter"
                && mapping.value == Value::String("#task".to_string())));

        let rendered = fs::read_to_string(paths.config_file()).expect("config should exist");
        assert!(rendered.contains("[git]"));
        assert!(rendered.contains("auto_commit = true"));
        assert!(rendered.contains("[tasks]"));
        assert!(rendered.contains("global_filter = \"#task\""));
        assert!(rendered.contains("global_query = \"not done\""));
        assert!(rendered.contains("remove_global_filter = true"));
        assert!(rendered.contains("set_created_date = true"));
        assert!(rendered.contains("recurrence_on_completion = \"next-line\""));
        assert!(rendered.contains("[tasks.statuses]"));
        assert!(rendered.contains("[[tasks.statuses.definitions]]"));
        assert!(rendered.contains("symbol = \">\""));
        assert!(rendered.contains("name = \"Waiting\""));

        let second_report =
            import_tasks_plugin_config(&paths).expect("second import should succeed");
        assert!(!second_report.updated);
    }

    #[test]
    fn import_tasks_plugin_config_errors_when_source_is_missing() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        let error = import_tasks_plugin_config(&paths).expect_err("import should fail");
        assert!(matches!(error, ConfigImportError::MissingSource(_)));
    }
}
