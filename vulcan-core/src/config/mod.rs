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

# [kanban]
# date_trigger = "@"
# time_trigger = "@@"
# date_format = "YYYY-MM-DD"
# time_format = "HH:mm"
# date_display_format = "YYYY-MM-DD"
# date_time_display_format = "YYYY-MM-DD HH:mm"
# link_date_to_daily_note = false
# metadata_keys = [
#   { metadata_key = "status", label = "Status", should_hide_label = false, contains_markdown = false },
#   { metadata_key = "owner", label = "Owner" },
# ]
# archive_with_date = false
# append_archive_date = false
# archive_date_format = "YYYY-MM-DD HH:mm"
# archive_date_separator = ""
# new_card_insertion_method = "append"  # prepend | prepend-compact | append
# new_line_trigger = "shift-enter"  # enter | shift-enter
# new_note_folder = "Cards"
# new_note_template = "Kanban Card"
# hide_card_count = false
# hide_tags_in_title = false
# hide_tags_display = false
# inline_metadata_position = "body"  # body | footer | metadata-table
# lane_width = 272
# full_list_lane_width = false
# list_collapse = [false, true]
# max_archive_size = 100
# move_dates = true
# move_tags = true
# move_task_metadata = true
# show_add_list = true
# show_archive_all = true
# show_board_settings = true
# show_checkboxes = false
# show_relative_date = true
# show_search = true
# show_set_view = true
# show_view_as_markdown = true
# date_picker_week_start = 1
# table_sizing = { Title = 240, Tags = 120 }
# tag_action = "obsidian"  # kanban | obsidian
# tag_colors = [{ tag_key = "#urgent", color = "#ffffff", background_color = "#cc0000" }]
# tag_sort = [{ tag = "#urgent" }]
# date_colors = [{ is_today = true, color = "#ffffff", background_color = "#2d6cdf" }]

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
# js_timeout_seconds = 5
# js_memory_limit_bytes = 16777216
# js_max_stack_size_bytes = 262144

# [templates]
# date_format = "YYYY-MM-DD"
# time_format = "HH:mm"
# obsidian_folder = "Shared Templates"  # Obsidian core Templates plugin
# templater_folder = "Templates"        # Templater plugin templates_folder
# command_timeout = 5
# trigger_on_file_creation = false
# auto_jump_to_cursor = false
# enable_system_commands = false
# shell_path = "/bin/bash"
# user_scripts_folder = "Scripts"
# enable_folder_templates = true
# enable_file_templates = false
# syntax_highlighting = true
# syntax_highlighting_mobile = false
# intellisense_render = 1
# enabled_templates_hotkeys = ["Daily"]
# startup_templates = ["Startup"]
# templates_pairs = [{ name = "slugify", command = "node scripts/slugify.js" }]
# folder_templates = [{ folder = "Daily", template = "Daily Template" }]
# file_templates = [{ regex = "^Projects/.*\\.md$", template = "Project Template" }]
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
            max_recursive_render_depth: default_dataview_max_recursive_render_depth(),
            primary_column_name: default_dataview_primary_column_name(),
            group_column_name: default_dataview_group_column_name(),
            js_timeout_seconds: default_dataview_js_timeout_seconds(),
            js_memory_limit_bytes: default_dataview_js_memory_limit_bytes(),
            js_max_stack_size_bytes: default_dataview_js_max_stack_size_bytes(),
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
    pub kanban: KanbanConfig,
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
            kanban: KanbanConfig::default(),
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
            Self::MissingSource(path) => {
                write!(formatter, "missing plugin config at {}", path.display())
            }
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
    kanban: Option<PartialKanbanConfig>,
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
    obsidian_folder: Option<PathBuf>,
    templater_folder: Option<PathBuf>,
    command_timeout: Option<usize>,
    templates_pairs: Option<Vec<TemplaterCommandPairConfig>>,
    trigger_on_file_creation: Option<bool>,
    auto_jump_to_cursor: Option<bool>,
    enable_system_commands: Option<bool>,
    shell_path: Option<PathBuf>,
    user_scripts_folder: Option<PathBuf>,
    enable_folder_templates: Option<bool>,
    folder_templates: Option<Vec<TemplaterFolderTemplateConfig>>,
    enable_file_templates: Option<bool>,
    file_templates: Option<Vec<TemplaterFileTemplateConfig>>,
    syntax_highlighting: Option<bool>,
    syntax_highlighting_mobile: Option<bool>,
    enabled_templates_hotkeys: Option<Vec<String>>,
    startup_templates: Option<Vec<String>>,
    intellisense_render: Option<usize>,
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
struct PartialKanbanConfig {
    date_trigger: Option<String>,
    time_trigger: Option<String>,
    date_format: Option<String>,
    time_format: Option<String>,
    date_display_format: Option<String>,
    date_time_display_format: Option<String>,
    link_date_to_daily_note: Option<bool>,
    metadata_keys: Option<Vec<KanbanMetadataKeyConfig>>,
    archive_with_date: Option<bool>,
    append_archive_date: Option<bool>,
    archive_date_format: Option<String>,
    archive_date_separator: Option<String>,
    new_card_insertion_method: Option<String>,
    new_line_trigger: Option<String>,
    new_note_folder: Option<String>,
    new_note_template: Option<String>,
    hide_card_count: Option<bool>,
    hide_tags_in_title: Option<bool>,
    hide_tags_display: Option<bool>,
    inline_metadata_position: Option<String>,
    lane_width: Option<usize>,
    full_list_lane_width: Option<bool>,
    list_collapse: Option<Vec<bool>>,
    max_archive_size: Option<usize>,
    show_checkboxes: Option<bool>,
    move_dates: Option<bool>,
    move_tags: Option<bool>,
    move_task_metadata: Option<bool>,
    show_add_list: Option<bool>,
    show_archive_all: Option<bool>,
    show_board_settings: Option<bool>,
    show_relative_date: Option<bool>,
    show_search: Option<bool>,
    show_set_view: Option<bool>,
    show_view_as_markdown: Option<bool>,
    date_picker_week_start: Option<usize>,
    table_sizing: Option<BTreeMap<String, usize>>,
    tag_action: Option<String>,
    tag_colors: Option<Vec<KanbanTagColorConfig>>,
    tag_sort: Option<Vec<KanbanTagSortConfig>>,
    date_colors: Option<Vec<KanbanDateColorConfig>>,
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
    js_timeout_seconds: Option<usize>,
    js_memory_limit_bytes: Option<usize>,
    js_max_stack_size_bytes: Option<usize>,
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
struct ObsidianTemplaterConfig {
    command_timeout: Option<usize>,
    templates_folder: Option<String>,
    #[serde(default)]
    templates_pairs: Vec<[String; 2]>,
    trigger_on_file_creation: Option<bool>,
    auto_jump_to_cursor: Option<bool>,
    enable_system_commands: Option<bool>,
    shell_path: Option<String>,
    user_scripts_folder: Option<String>,
    enable_folder_templates: Option<bool>,
    #[serde(default)]
    folder_templates: Vec<ObsidianTemplaterFolderTemplateConfig>,
    enable_file_templates: Option<bool>,
    #[serde(default)]
    file_templates: Vec<TemplaterFileTemplateConfig>,
    syntax_highlighting: Option<bool>,
    syntax_highlighting_mobile: Option<bool>,
    #[serde(default)]
    enabled_templates_hotkeys: Vec<String>,
    #[serde(default)]
    startup_templates: Vec<String>,
    intellisense_render: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianTemplaterFolderTemplateConfig {
    folder: String,
    template: String,
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

#[derive(Debug, Deserialize, Default)]
struct ObsidianKanbanConfig {
    #[serde(rename = "date-trigger")]
    date_trigger: Option<String>,
    #[serde(rename = "time-trigger")]
    time_trigger: Option<String>,
    #[serde(rename = "date-format")]
    date_format: Option<String>,
    #[serde(rename = "time-format")]
    time_format: Option<String>,
    #[serde(rename = "date-display-format")]
    date_display_format: Option<String>,
    #[serde(rename = "date-time-display-format")]
    date_time_display_format: Option<String>,
    #[serde(rename = "link-date-to-daily-note")]
    link_date_to_daily_note: Option<bool>,
    #[serde(rename = "metadata-keys")]
    metadata_keys: Option<Vec<KanbanMetadataKeyConfig>>,
    #[serde(rename = "archive-with-date")]
    archive_with_date: Option<bool>,
    #[serde(rename = "append-archive-date", alias = "prepend-archive-date")]
    append_archive_date: Option<bool>,
    #[serde(rename = "archive-date-format")]
    archive_date_format: Option<String>,
    #[serde(rename = "archive-date-separator")]
    archive_date_separator: Option<String>,
    #[serde(rename = "new-card-insertion-method")]
    new_card_insertion_method: Option<String>,
    #[serde(rename = "new-line-trigger")]
    new_line_trigger: Option<String>,
    #[serde(rename = "new-note-folder")]
    new_note_folder: Option<String>,
    #[serde(rename = "new-note-template")]
    new_note_template: Option<String>,
    #[serde(rename = "hide-card-count")]
    hide_card_count: Option<bool>,
    #[serde(rename = "hide-tags-in-title")]
    hide_tags_in_title: Option<bool>,
    #[serde(rename = "hide-tags-display")]
    hide_tags_display: Option<bool>,
    #[serde(rename = "inline-metadata-position")]
    inline_metadata_position: Option<String>,
    #[serde(rename = "lane-width")]
    lane_width: Option<usize>,
    #[serde(rename = "full-list-lane-width")]
    full_list_lane_width: Option<bool>,
    #[serde(rename = "list-collapse")]
    list_collapse: Option<Vec<bool>>,
    #[serde(rename = "max-archive-size")]
    max_archive_size: Option<usize>,
    #[serde(rename = "show-checkboxes")]
    show_checkboxes: Option<bool>,
    #[serde(rename = "move-dates")]
    move_dates: Option<bool>,
    #[serde(rename = "move-tags")]
    move_tags: Option<bool>,
    #[serde(rename = "move-task-metadata")]
    move_task_metadata: Option<bool>,
    #[serde(rename = "show-add-list")]
    show_add_list: Option<bool>,
    #[serde(rename = "show-archive-all")]
    show_archive_all: Option<bool>,
    #[serde(rename = "show-board-settings")]
    show_board_settings: Option<bool>,
    #[serde(rename = "show-relative-date")]
    show_relative_date: Option<bool>,
    #[serde(rename = "show-search")]
    show_search: Option<bool>,
    #[serde(rename = "show-set-view")]
    show_set_view: Option<bool>,
    #[serde(rename = "show-view-as-markdown")]
    show_view_as_markdown: Option<bool>,
    #[serde(rename = "date-picker-week-start")]
    date_picker_week_start: Option<usize>,
    #[serde(rename = "table-sizing")]
    table_sizing: Option<BTreeMap<String, usize>>,
    #[serde(rename = "tag-action")]
    tag_action: Option<String>,
    #[serde(rename = "tag-colors")]
    tag_colors: Option<Vec<KanbanTagColorConfig>>,
    #[serde(rename = "tag-sort")]
    tag_sort: Option<Vec<KanbanTagSortConfig>>,
    #[serde(rename = "date-colors")]
    date_colors: Option<Vec<KanbanDateColorConfig>>,
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

fn default_dataview_js_timeout_seconds() -> usize {
    5
}

fn default_dataview_js_memory_limit_bytes() -> usize {
    16 * 1024 * 1024
}

fn default_dataview_js_max_stack_size_bytes() -> usize {
    256 * 1024
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

pub fn import_templater_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    let source_path = paths
        .vault_root()
        .join(".obsidian/plugins/templater-obsidian/data.json");
    if !source_path.exists() {
        return Err(ConfigImportError::MissingSource(source_path));
    }

    let obsidian =
        serde_json::from_str::<ObsidianTemplaterConfig>(&fs::read_to_string(&source_path)?)?;
    let imported_templates = imported_templater_config(obsidian);
    let mappings = templater_config_import_mappings(&imported_templates)?;

    ensure_vulcan_dir(paths)?;
    let config_path = paths.config_file().to_path_buf();
    let created_config = !config_path.exists();
    let existing_contents = fs::read_to_string(&config_path).ok();
    let mut config_value = load_config_value(&config_path)?;
    write_templater_import(&mut config_value, &imported_templates)?;
    let rendered = toml::to_string_pretty(&config_value)?;
    let updated = existing_contents.as_deref() != Some(rendered.as_str());
    if updated {
        fs::write(&config_path, rendered)?;
    }

    Ok(ConfigImportReport {
        plugin: "templater".to_string(),
        source_path,
        config_path,
        created_config,
        updated,
        mappings,
    })
}

pub fn import_kanban_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    let source_path = paths
        .vault_root()
        .join(".obsidian/plugins/obsidian-kanban/data.json");
    if !source_path.exists() {
        return Err(ConfigImportError::MissingSource(source_path));
    }

    let obsidian =
        serde_json::from_str::<ObsidianKanbanConfig>(&fs::read_to_string(&source_path)?)?;
    let imported_kanban = imported_kanban_config(obsidian);
    let mappings = kanban_config_import_mappings(&imported_kanban)?;

    ensure_vulcan_dir(paths)?;
    let config_path = paths.config_file().to_path_buf();
    let created_config = !config_path.exists();
    let existing_contents = fs::read_to_string(&config_path).ok();
    let mut config_value = load_config_value(&config_path)?;
    write_kanban_import(&mut config_value, &imported_kanban)?;
    let rendered = toml::to_string_pretty(&config_value)?;
    let updated = existing_contents.as_deref() != Some(rendered.as_str());
    if updated {
        fs::write(&config_path, rendered)?;
    }

    Ok(ConfigImportReport {
        plugin: "kanban".to_string(),
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

    if let Some(obsidian_templater) = load_obsidian_templater_config(paths, &mut diagnostics) {
        apply_obsidian_templater_defaults(&mut config, obsidian_templater);
    }

    if let Some(obsidian_dataview) = load_obsidian_dataview_config(paths, &mut diagnostics) {
        apply_obsidian_dataview_defaults(&mut config, obsidian_dataview);
    }

    if let Some(obsidian_tasks) = load_obsidian_tasks_config(paths, &mut diagnostics) {
        apply_obsidian_tasks_defaults(&mut config, obsidian_tasks);
    }

    if let Some(obsidian_kanban) = load_obsidian_kanban_config(paths, &mut diagnostics) {
        apply_obsidian_kanban_defaults(&mut config, obsidian_kanban);
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

fn load_obsidian_templater_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianTemplaterConfig> {
    let path = paths
        .vault_root()
        .join(".obsidian/plugins/templater-obsidian/data.json");

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

fn load_obsidian_kanban_config(
    paths: &VaultPaths,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<ObsidianKanbanConfig> {
    let path = paths
        .vault_root()
        .join(".obsidian/plugins/obsidian-kanban/data.json");

    load_json_file(&path, diagnostics)
}

fn imported_tasks_config(obsidian: ObsidianTasksConfig) -> TasksConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_tasks_defaults(&mut config, obsidian);
    config.tasks
}

fn imported_templater_config(obsidian: ObsidianTemplaterConfig) -> TemplatesConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_templater_defaults(&mut config, obsidian);
    config.templates
}

fn imported_kanban_config(obsidian: ObsidianKanbanConfig) -> KanbanConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_kanban_defaults(&mut config, obsidian);
    config.kanban
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

fn push_config_import_mapping<T: Serialize>(
    mappings: &mut Vec<ConfigImportMapping>,
    source: &str,
    target: &str,
    value: &T,
) -> Result<(), ConfigImportError> {
    mappings.push(ConfigImportMapping {
        source: source.to_string(),
        target: target.to_string(),
        value: serde_json::to_value(value)?,
    });
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn templater_config_import_mappings(
    config: &TemplatesConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "templates_folder",
        "templates.templater_folder",
        &config.templater_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "command_timeout",
        "templates.command_timeout",
        &config.command_timeout,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "templates_pairs",
        "templates.templates_pairs",
        &config.templates_pairs,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "trigger_on_file_creation",
        "templates.trigger_on_file_creation",
        &config.trigger_on_file_creation,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "auto_jump_to_cursor",
        "templates.auto_jump_to_cursor",
        &config.auto_jump_to_cursor,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enable_system_commands",
        "templates.enable_system_commands",
        &config.enable_system_commands,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "shell_path",
        "templates.shell_path",
        &config.shell_path,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "user_scripts_folder",
        "templates.user_scripts_folder",
        &config.user_scripts_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enable_folder_templates",
        "templates.enable_folder_templates",
        &config.enable_folder_templates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "folder_templates",
        "templates.folder_templates",
        &config.folder_templates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enable_file_templates",
        "templates.enable_file_templates",
        &config.enable_file_templates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "file_templates",
        "templates.file_templates",
        &config.file_templates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "syntax_highlighting",
        "templates.syntax_highlighting",
        &config.syntax_highlighting,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "syntax_highlighting_mobile",
        "templates.syntax_highlighting_mobile",
        &config.syntax_highlighting_mobile,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enabled_templates_hotkeys",
        "templates.enabled_templates_hotkeys",
        &config.enabled_templates_hotkeys,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "startup_templates",
        "templates.startup_templates",
        &config.startup_templates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "intellisense_render",
        "templates.intellisense_render",
        &config.intellisense_render,
    )?;
    Ok(mappings)
}

#[allow(clippy::too_many_lines)]
fn kanban_config_import_mappings(
    config: &KanbanConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "date-trigger",
        "kanban.date_trigger",
        &config.date_trigger,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "time-trigger",
        "kanban.time_trigger",
        &config.time_trigger,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "date-format",
        "kanban.date_format",
        &config.date_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "time-format",
        "kanban.time_format",
        &config.time_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "date-display-format",
        "kanban.date_display_format",
        &config.date_display_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "date-time-display-format",
        "kanban.date_time_display_format",
        &config.date_time_display_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "link-date-to-daily-note",
        "kanban.link_date_to_daily_note",
        &config.link_date_to_daily_note,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "metadata-keys",
        "kanban.metadata_keys",
        &config.metadata_keys,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "archive-with-date",
        "kanban.archive_with_date",
        &config.archive_with_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "append-archive-date",
        "kanban.append_archive_date",
        &config.append_archive_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "archive-date-format",
        "kanban.archive_date_format",
        &config.archive_date_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "archive-date-separator",
        "kanban.archive_date_separator",
        &config.archive_date_separator,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "new-card-insertion-method",
        "kanban.new_card_insertion_method",
        &config.new_card_insertion_method,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "new-line-trigger",
        "kanban.new_line_trigger",
        &config.new_line_trigger,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "new-note-folder",
        "kanban.new_note_folder",
        &config.new_note_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "new-note-template",
        "kanban.new_note_template",
        &config.new_note_template,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "hide-card-count",
        "kanban.hide_card_count",
        &config.hide_card_count,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "hide-tags-in-title",
        "kanban.hide_tags_in_title",
        &config.hide_tags_in_title,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "hide-tags-display",
        "kanban.hide_tags_display",
        &config.hide_tags_display,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "inline-metadata-position",
        "kanban.inline_metadata_position",
        &config.inline_metadata_position,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "lane-width",
        "kanban.lane_width",
        &config.lane_width,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "full-list-lane-width",
        "kanban.full_list_lane_width",
        &config.full_list_lane_width,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "list-collapse",
        "kanban.list_collapse",
        &config.list_collapse,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "max-archive-size",
        "kanban.max_archive_size",
        &config.max_archive_size,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-checkboxes",
        "kanban.show_checkboxes",
        &config.show_checkboxes,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "move-dates",
        "kanban.move_dates",
        &config.move_dates,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "move-tags",
        "kanban.move_tags",
        &config.move_tags,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "move-task-metadata",
        "kanban.move_task_metadata",
        &config.move_task_metadata,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-add-list",
        "kanban.show_add_list",
        &config.show_add_list,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-archive-all",
        "kanban.show_archive_all",
        &config.show_archive_all,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-board-settings",
        "kanban.show_board_settings",
        &config.show_board_settings,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-relative-date",
        "kanban.show_relative_date",
        &config.show_relative_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-search",
        "kanban.show_search",
        &config.show_search,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-set-view",
        "kanban.show_set_view",
        &config.show_set_view,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "show-view-as-markdown",
        "kanban.show_view_as_markdown",
        &config.show_view_as_markdown,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "date-picker-week-start",
        "kanban.date_picker_week_start",
        &config.date_picker_week_start,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "table-sizing",
        "kanban.table_sizing",
        &config.table_sizing,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "tag-action",
        "kanban.tag_action",
        &config.tag_action,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "tag-colors",
        "kanban.tag_colors",
        &config.tag_colors,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "tag-sort",
        "kanban.tag_sort",
        &config.tag_sort,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "date-colors",
        "kanban.date_colors",
        &config.date_colors,
    )?;
    Ok(mappings)
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

#[allow(clippy::too_many_lines)]
fn write_templater_import(
    config_value: &mut toml::Value,
    templates: &TemplatesConfig,
) -> Result<(), ConfigImportError> {
    let Some(root_table) = config_value.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(
            "expected .vulcan/config.toml to contain a TOML table".to_string(),
        ));
    };

    let templates_entry = root_table
        .entry("templates".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !templates_entry.is_table() {
        *templates_entry = toml::Value::Table(toml::map::Map::new());
    }
    let Some(templates_table) = templates_entry.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(
            "expected [templates] to be a TOML table".to_string(),
        ));
    };

    write_optional_toml_serialized(
        templates_table,
        "templater_folder",
        templates.templater_folder.as_ref(),
    )?;
    templates_table.insert(
        "command_timeout".to_string(),
        toml::Value::Integer(i64::try_from(templates.command_timeout).map_err(|_| {
            ConfigImportError::InvalidConfig(
                "expected command_timeout to fit in a signed 64-bit integer".to_string(),
            )
        })?),
    );
    write_optional_toml_serialized(
        templates_table,
        "templates_pairs",
        (!templates.templates_pairs.is_empty()).then_some(&templates.templates_pairs),
    )?;
    templates_table.insert(
        "trigger_on_file_creation".to_string(),
        toml::Value::Boolean(templates.trigger_on_file_creation),
    );
    templates_table.insert(
        "auto_jump_to_cursor".to_string(),
        toml::Value::Boolean(templates.auto_jump_to_cursor),
    );
    templates_table.insert(
        "enable_system_commands".to_string(),
        toml::Value::Boolean(templates.enable_system_commands),
    );
    write_optional_toml_serialized(templates_table, "shell_path", templates.shell_path.as_ref())?;
    write_optional_toml_serialized(
        templates_table,
        "user_scripts_folder",
        templates.user_scripts_folder.as_ref(),
    )?;
    templates_table.insert(
        "enable_folder_templates".to_string(),
        toml::Value::Boolean(templates.enable_folder_templates),
    );
    write_optional_toml_serialized(
        templates_table,
        "folder_templates",
        (!templates.folder_templates.is_empty()).then_some(&templates.folder_templates),
    )?;
    templates_table.insert(
        "enable_file_templates".to_string(),
        toml::Value::Boolean(templates.enable_file_templates),
    );
    write_optional_toml_serialized(
        templates_table,
        "file_templates",
        (!templates.file_templates.is_empty()).then_some(&templates.file_templates),
    )?;
    templates_table.insert(
        "syntax_highlighting".to_string(),
        toml::Value::Boolean(templates.syntax_highlighting),
    );
    templates_table.insert(
        "syntax_highlighting_mobile".to_string(),
        toml::Value::Boolean(templates.syntax_highlighting_mobile),
    );
    write_optional_toml_serialized(
        templates_table,
        "enabled_templates_hotkeys",
        (!templates.enabled_templates_hotkeys.is_empty())
            .then_some(&templates.enabled_templates_hotkeys),
    )?;
    write_optional_toml_serialized(
        templates_table,
        "startup_templates",
        (!templates.startup_templates.is_empty()).then_some(&templates.startup_templates),
    )?;
    templates_table.insert(
        "intellisense_render".to_string(),
        toml::Value::Integer(i64::try_from(templates.intellisense_render).map_err(|_| {
            ConfigImportError::InvalidConfig(
                "expected intellisense_render to fit in a signed 64-bit integer".to_string(),
            )
        })?),
    );

    Ok(())
}

#[allow(clippy::too_many_lines)]
fn write_kanban_import(
    config_value: &mut toml::Value,
    kanban: &KanbanConfig,
) -> Result<(), ConfigImportError> {
    let Some(root_table) = config_value.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(
            "expected .vulcan/config.toml to contain a TOML table".to_string(),
        ));
    };

    let kanban_entry = root_table
        .entry("kanban".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !kanban_entry.is_table() {
        *kanban_entry = toml::Value::Table(toml::map::Map::new());
    }
    let Some(kanban_table) = kanban_entry.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(
            "expected [kanban] to be a TOML table".to_string(),
        ));
    };

    kanban_table.insert(
        "date_trigger".to_string(),
        toml::Value::String(kanban.date_trigger.clone()),
    );
    kanban_table.insert(
        "time_trigger".to_string(),
        toml::Value::String(kanban.time_trigger.clone()),
    );
    kanban_table.insert(
        "date_format".to_string(),
        toml::Value::String(kanban.date_format.clone()),
    );
    kanban_table.insert(
        "time_format".to_string(),
        toml::Value::String(kanban.time_format.clone()),
    );
    write_optional_toml_string(
        kanban_table,
        "date_display_format",
        kanban.date_display_format.as_deref(),
    );
    write_optional_toml_string(
        kanban_table,
        "date_time_display_format",
        kanban.date_time_display_format.as_deref(),
    );
    kanban_table.insert(
        "link_date_to_daily_note".to_string(),
        toml::Value::Boolean(kanban.link_date_to_daily_note),
    );
    write_optional_toml_serialized(
        kanban_table,
        "metadata_keys",
        (!kanban.metadata_keys.is_empty()).then_some(&kanban.metadata_keys),
    )?;
    kanban_table.insert(
        "archive_with_date".to_string(),
        toml::Value::Boolean(kanban.archive_with_date),
    );
    kanban_table.insert(
        "append_archive_date".to_string(),
        toml::Value::Boolean(kanban.append_archive_date),
    );
    kanban_table.insert(
        "archive_date_format".to_string(),
        toml::Value::String(kanban.archive_date_format.clone()),
    );
    write_optional_toml_string(
        kanban_table,
        "archive_date_separator",
        kanban.archive_date_separator.as_deref(),
    );
    kanban_table.insert(
        "new_card_insertion_method".to_string(),
        toml::Value::String(kanban.new_card_insertion_method.clone()),
    );
    write_optional_toml_string(
        kanban_table,
        "new_line_trigger",
        kanban.new_line_trigger.as_deref(),
    );
    write_optional_toml_string(
        kanban_table,
        "new_note_folder",
        kanban.new_note_folder.as_deref(),
    );
    write_optional_toml_string(
        kanban_table,
        "new_note_template",
        kanban.new_note_template.as_deref(),
    );
    kanban_table.insert(
        "hide_card_count".to_string(),
        toml::Value::Boolean(kanban.hide_card_count),
    );
    kanban_table.insert(
        "hide_tags_in_title".to_string(),
        toml::Value::Boolean(kanban.hide_tags_in_title),
    );
    kanban_table.insert(
        "hide_tags_display".to_string(),
        toml::Value::Boolean(kanban.hide_tags_display),
    );
    write_optional_toml_string(
        kanban_table,
        "inline_metadata_position",
        kanban.inline_metadata_position.as_deref(),
    );
    write_optional_toml_usize(kanban_table, "lane_width", kanban.lane_width)?;
    write_optional_toml_serialized(
        kanban_table,
        "full_list_lane_width",
        kanban.full_list_lane_width.as_ref(),
    )?;
    write_optional_toml_serialized(
        kanban_table,
        "list_collapse",
        (!kanban.list_collapse.is_empty()).then_some(&kanban.list_collapse),
    )?;
    write_optional_toml_usize(kanban_table, "max_archive_size", kanban.max_archive_size)?;
    kanban_table.insert(
        "show_checkboxes".to_string(),
        toml::Value::Boolean(kanban.show_checkboxes),
    );
    write_optional_toml_serialized(kanban_table, "move_dates", kanban.move_dates.as_ref())?;
    write_optional_toml_serialized(kanban_table, "move_tags", kanban.move_tags.as_ref())?;
    write_optional_toml_serialized(
        kanban_table,
        "move_task_metadata",
        kanban.move_task_metadata.as_ref(),
    )?;
    write_optional_toml_serialized(kanban_table, "show_add_list", kanban.show_add_list.as_ref())?;
    write_optional_toml_serialized(
        kanban_table,
        "show_archive_all",
        kanban.show_archive_all.as_ref(),
    )?;
    write_optional_toml_serialized(
        kanban_table,
        "show_board_settings",
        kanban.show_board_settings.as_ref(),
    )?;
    write_optional_toml_serialized(
        kanban_table,
        "show_relative_date",
        kanban.show_relative_date.as_ref(),
    )?;
    write_optional_toml_serialized(kanban_table, "show_search", kanban.show_search.as_ref())?;
    write_optional_toml_serialized(kanban_table, "show_set_view", kanban.show_set_view.as_ref())?;
    write_optional_toml_serialized(
        kanban_table,
        "show_view_as_markdown",
        kanban.show_view_as_markdown.as_ref(),
    )?;
    write_optional_toml_usize(
        kanban_table,
        "date_picker_week_start",
        kanban.date_picker_week_start,
    )?;
    write_optional_toml_serialized(
        kanban_table,
        "table_sizing",
        (!kanban.table_sizing.is_empty()).then_some(&kanban.table_sizing),
    )?;
    write_optional_toml_string(kanban_table, "tag_action", kanban.tag_action.as_deref());
    write_optional_toml_serialized(
        kanban_table,
        "tag_colors",
        (!kanban.tag_colors.is_empty()).then_some(&kanban.tag_colors),
    )?;
    write_optional_toml_serialized(
        kanban_table,
        "tag_sort",
        (!kanban.tag_sort.is_empty()).then_some(&kanban.tag_sort),
    )?;
    write_optional_toml_serialized(
        kanban_table,
        "date_colors",
        (!kanban.date_colors.is_empty()).then_some(&kanban.date_colors),
    )?;

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

fn write_optional_toml_usize(
    table: &mut toml::map::Map<String, toml::Value>,
    key: &str,
    value: Option<usize>,
) -> Result<(), ConfigImportError> {
    match value {
        Some(value) => {
            let value = i64::try_from(value).map_err(|_| {
                ConfigImportError::InvalidConfig(format!(
                    "expected {key} to fit in a signed 64-bit integer"
                ))
            })?;
            table.insert(key.to_string(), toml::Value::Integer(value));
        }
        None => {
            table.remove(key);
        }
    }

    Ok(())
}

fn write_optional_toml_serialized<T: Serialize>(
    table: &mut toml::map::Map<String, toml::Value>,
    key: &str,
    value: Option<&T>,
) -> Result<(), ConfigImportError> {
    match value {
        Some(value) => {
            table.insert(key.to_string(), toml::Value::try_from(value)?);
        }
        None => {
            table.remove(key);
        }
    }

    Ok(())
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
        }
        if let Some(limit) = dataview.js_memory_limit_bytes {
            config.dataview.js_memory_limit_bytes = limit;
        }
        if let Some(limit) = dataview.js_max_stack_size_bytes {
            config.dataview.js_max_stack_size_bytes = limit;
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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn kanban_metadata_key_names(keys: &[KanbanMetadataKeyConfig]) -> Vec<String> {
        keys.iter()
            .map(|key| match key {
                KanbanMetadataKeyConfig::Detailed(field) => field.metadata_key.clone(),
                KanbanMetadataKeyConfig::Key(key) => key.clone(),
            })
            .collect()
    }

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
            r##"{
              "useMarkdownLinks": true,
              "newLinkFormat": "relative",
              "attachmentFolderPath": "/",
              "strictLineBreaks": true
            }"##,
        )
        .expect("app config should be written");
        fs::write(
            vault_root.join(".obsidian/types.json"),
            r##"{
              "status": "text",
              "priority": { "type": "number" }
            }"##,
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
        fs::create_dir_all(vault_root.join(".obsidian/plugins/obsidian-kanban"))
            .expect("kanban plugin dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/obsidian-kanban/data.json"),
            r##"{
              "date-trigger": "DUE",
              "time-trigger": "AT",
              "date-format": "DD/MM/YYYY",
              "time-format": "HH:mm:ss",
              "date-display-format": "ddd DD MMM",
              "date-time-display-format": "ddd DD MMM HH:mm:ss",
              "link-date-to-daily-note": true,
              "metadata-keys": [
                {
                  "metadataKey": "status",
                  "label": "Status",
                  "shouldHideLabel": true,
                  "containsMarkdown": true
                },
                { "metadataKey": "owner", "label": "Owner" }
              ],
              "archive-with-date": true,
              "append-archive-date": true,
              "archive-date-format": "DD/MM/YYYY HH:mm:ss",
              "archive-date-separator": " :: ",
              "new-card-insertion-method": "prepend",
              "new-line-trigger": "enter",
              "new-note-folder": "Cards/Ideas",
              "new-note-template": "Kanban Card",
              "hide-card-count": true,
              "hide-tags-in-title": true,
              "hide-tags-display": true,
              "inline-metadata-position": "metadata-table",
              "lane-width": 320,
              "full-list-lane-width": true,
              "list-collapse": [true, false],
              "max-archive-size": 50,
              "show-checkboxes": true,
              "move-dates": true,
              "move-tags": false,
              "move-task-metadata": true,
              "show-add-list": false,
              "show-archive-all": false,
              "show-board-settings": false,
              "show-relative-date": true,
              "show-search": false,
              "show-set-view": false,
              "show-view-as-markdown": false,
              "date-picker-week-start": 1,
              "table-sizing": {
                "Title": 240,
                "Tags": 96
              },
              "tag-action": "kanban",
              "tag-colors": [
                {
                  "tagKey": "#urgent",
                  "color": "#ffffff",
                  "backgroundColor": "#cc0000"
                }
              ],
              "tag-sort": [
                { "tag": "#urgent" }
              ],
              "date-colors": [
                {
                  "isToday": true,
                  "backgroundColor": "#2d6cdf",
                  "color": "#ffffff"
                }
              ]
            }"##,
        )
        .expect("kanban config should be written");
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
        assert_eq!(loaded.config.kanban.date_trigger, "DUE");
        assert_eq!(loaded.config.kanban.time_trigger, "AT");
        assert_eq!(loaded.config.kanban.date_format, "DD/MM/YYYY");
        assert_eq!(loaded.config.kanban.time_format, "HH:mm:ss");
        assert_eq!(
            loaded.config.kanban.date_display_format.as_deref(),
            Some("ddd DD MMM")
        );
        assert_eq!(
            loaded.config.kanban.date_time_display_format.as_deref(),
            Some("ddd DD MMM HH:mm:ss")
        );
        assert!(loaded.config.kanban.link_date_to_daily_note);
        assert_eq!(
            kanban_metadata_key_names(&loaded.config.kanban.metadata_keys),
            vec!["status".to_string(), "owner".to_string()]
        );
        assert_eq!(
            loaded.config.kanban.metadata_keys[0],
            KanbanMetadataKeyConfig::Detailed(KanbanMetadataFieldConfig {
                metadata_key: "status".to_string(),
                label: Some("Status".to_string()),
                should_hide_label: true,
                contains_markdown: true,
            })
        );
        assert!(loaded.config.kanban.archive_with_date);
        assert!(loaded.config.kanban.append_archive_date);
        assert_eq!(
            loaded.config.kanban.archive_date_format,
            "DD/MM/YYYY HH:mm:ss"
        );
        assert_eq!(
            loaded.config.kanban.archive_date_separator.as_deref(),
            Some(" :: ")
        );
        assert_eq!(loaded.config.kanban.new_card_insertion_method, "prepend");
        assert_eq!(
            loaded.config.kanban.new_line_trigger.as_deref(),
            Some("enter")
        );
        assert_eq!(
            loaded.config.kanban.new_note_folder.as_deref(),
            Some("Cards/Ideas")
        );
        assert_eq!(
            loaded.config.kanban.new_note_template.as_deref(),
            Some("Kanban Card")
        );
        assert!(loaded.config.kanban.hide_card_count);
        assert!(loaded.config.kanban.hide_tags_in_title);
        assert!(loaded.config.kanban.hide_tags_display);
        assert_eq!(
            loaded.config.kanban.inline_metadata_position.as_deref(),
            Some("metadata-table")
        );
        assert_eq!(loaded.config.kanban.lane_width, Some(320));
        assert_eq!(loaded.config.kanban.full_list_lane_width, Some(true));
        assert_eq!(loaded.config.kanban.list_collapse, vec![true, false]);
        assert_eq!(loaded.config.kanban.max_archive_size, Some(50));
        assert!(loaded.config.kanban.show_checkboxes);
        assert_eq!(loaded.config.kanban.move_dates, Some(true));
        assert_eq!(loaded.config.kanban.move_tags, Some(false));
        assert_eq!(loaded.config.kanban.move_task_metadata, Some(true));
        assert_eq!(loaded.config.kanban.show_add_list, Some(false));
        assert_eq!(loaded.config.kanban.show_archive_all, Some(false));
        assert_eq!(loaded.config.kanban.show_board_settings, Some(false));
        assert_eq!(loaded.config.kanban.show_relative_date, Some(true));
        assert_eq!(loaded.config.kanban.show_search, Some(false));
        assert_eq!(loaded.config.kanban.show_set_view, Some(false));
        assert_eq!(loaded.config.kanban.show_view_as_markdown, Some(false));
        assert_eq!(loaded.config.kanban.date_picker_week_start, Some(1));
        assert_eq!(loaded.config.kanban.table_sizing.get("Title"), Some(&240));
        assert_eq!(loaded.config.kanban.tag_action.as_deref(), Some("kanban"));
        assert_eq!(
            loaded.config.kanban.tag_colors,
            vec![KanbanTagColorConfig {
                tag_key: "#urgent".to_string(),
                color: Some("#ffffff".to_string()),
                background_color: Some("#cc0000".to_string()),
            }]
        );
        assert_eq!(
            loaded.config.kanban.tag_sort,
            vec![KanbanTagSortConfig {
                tag: "#urgent".to_string()
            }]
        );
        assert_eq!(
            loaded.config.kanban.date_colors,
            vec![KanbanDateColorConfig {
                is_today: Some(true),
                is_before: None,
                is_after: None,
                distance: None,
                unit: None,
                direction: None,
                color: Some("#ffffff".to_string()),
                background_color: Some("#2d6cdf".to_string()),
            }]
        );
    }

    #[test]
    fn templater_plugin_settings_seed_defaults() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/templater-obsidian"))
            .expect("templater plugin dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/templater-obsidian/data.json"),
            r##"{
              "command_timeout": 9,
              "templates_folder": "Templater/Templates",
              "templates_pairs": [
                ["slugify", "node scripts/slugify.js"],
                ["", ""]
              ],
              "trigger_on_file_creation": true,
              "auto_jump_to_cursor": true,
              "enable_system_commands": true,
              "shell_path": "/bin/zsh",
              "user_scripts_folder": "Scripts/User",
              "enable_folder_templates": false,
              "folder_templates": [
                { "folder": "Daily", "template": "Daily Template" },
                { "folder": "", "template": "" }
              ],
              "enable_file_templates": true,
              "file_templates": [
                { "regex": "^Projects/.*\\\\.md$", "template": "Project Template" },
                { "regex": "", "template": "" }
              ],
              "syntax_highlighting": false,
              "syntax_highlighting_mobile": true,
              "enabled_templates_hotkeys": ["Daily", ""],
              "startup_templates": ["Startup", ""],
              "intellisense_render": 3
            }"##,
        )
        .expect("templater config should be written");

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(
            loaded.config.templates.templater_folder,
            Some(PathBuf::from("Templater/Templates"))
        );
        assert_eq!(loaded.config.templates.command_timeout, 9);
        assert_eq!(
            loaded.config.templates.templates_pairs,
            vec![TemplaterCommandPairConfig {
                name: "slugify".to_string(),
                command: "node scripts/slugify.js".to_string(),
            }]
        );
        assert!(loaded.config.templates.trigger_on_file_creation);
        assert!(loaded.config.templates.auto_jump_to_cursor);
        assert!(loaded.config.templates.enable_system_commands);
        assert_eq!(
            loaded.config.templates.shell_path,
            Some(PathBuf::from("/bin/zsh"))
        );
        assert_eq!(
            loaded.config.templates.user_scripts_folder,
            Some(PathBuf::from("Scripts/User"))
        );
        assert!(!loaded.config.templates.enable_folder_templates);
        assert_eq!(
            loaded.config.templates.folder_templates,
            vec![TemplaterFolderTemplateConfig {
                folder: PathBuf::from("Daily"),
                template: "Daily Template".to_string(),
            }]
        );
        assert!(loaded.config.templates.enable_file_templates);
        assert_eq!(
            loaded.config.templates.file_templates,
            vec![TemplaterFileTemplateConfig {
                regex: "^Projects/.*\\\\.md$".to_string(),
                template: "Project Template".to_string(),
            }]
        );
        assert!(!loaded.config.templates.syntax_highlighting);
        assert!(loaded.config.templates.syntax_highlighting_mobile);
        assert_eq!(
            loaded.config.templates.enabled_templates_hotkeys,
            vec!["Daily".to_string()]
        );
        assert_eq!(
            loaded.config.templates.startup_templates,
            vec!["Startup".to_string()]
        );
        assert_eq!(loaded.config.templates.intellisense_render, 3);
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

[kanban]
date_trigger = "DUE"
time_trigger = "AT"
date_format = "DD/MM/YYYY"
time_format = "HH:mm:ss"
date_display_format = "ddd DD MMM"
date_time_display_format = "ddd DD MMM HH:mm:ss"
link_date_to_daily_note = true
metadata_keys = [
  { metadata_key = "status", label = "Status", should_hide_label = true, contains_markdown = true },
  { metadata_key = "owner", label = "Owner" },
]
archive_with_date = true
append_archive_date = true
archive_date_format = "DD/MM/YYYY HH:mm:ss"
archive_date_separator = " :: "
new_card_insertion_method = "prepend"
new_line_trigger = "enter"
new_note_folder = "Cards/Ideas"
new_note_template = "Kanban Card"
hide_card_count = true
hide_tags_in_title = true
hide_tags_display = true
inline_metadata_position = "metadata-table"
lane_width = 300
full_list_lane_width = true
list_collapse = [true, false]
max_archive_size = 42
show_checkboxes = true
move_dates = true
move_tags = false
move_task_metadata = true
show_add_list = false
show_archive_all = false
show_board_settings = false
show_relative_date = true
show_search = false
show_set_view = false
show_view_as_markdown = false
date_picker_week_start = 1
table_sizing = { Title = 240, Tags = 96 }
tag_action = "kanban"
tag_colors = [{ tag_key = "#urgent", color = "#ffffff", background_color = "#cc0000" }]
tag_sort = [{ tag = "#urgent" }]
date_colors = [{ is_today = true, background_color = "#2d6cdf", color = "#ffffff" }]

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
        assert_eq!(loaded.config.kanban.date_trigger, "DUE");
        assert_eq!(loaded.config.kanban.time_trigger, "AT");
        assert_eq!(loaded.config.kanban.date_format, "DD/MM/YYYY");
        assert_eq!(loaded.config.kanban.time_format, "HH:mm:ss");
        assert_eq!(
            loaded.config.kanban.date_display_format.as_deref(),
            Some("ddd DD MMM")
        );
        assert_eq!(
            loaded.config.kanban.date_time_display_format.as_deref(),
            Some("ddd DD MMM HH:mm:ss")
        );
        assert!(loaded.config.kanban.link_date_to_daily_note);
        assert_eq!(
            kanban_metadata_key_names(&loaded.config.kanban.metadata_keys),
            vec!["status".to_string(), "owner".to_string()]
        );
        assert!(loaded.config.kanban.archive_with_date);
        assert!(loaded.config.kanban.append_archive_date);
        assert_eq!(
            loaded.config.kanban.archive_date_format,
            "DD/MM/YYYY HH:mm:ss"
        );
        assert_eq!(
            loaded.config.kanban.archive_date_separator.as_deref(),
            Some(" :: ")
        );
        assert_eq!(loaded.config.kanban.new_card_insertion_method, "prepend");
        assert_eq!(
            loaded.config.kanban.new_line_trigger.as_deref(),
            Some("enter")
        );
        assert_eq!(
            loaded.config.kanban.new_note_folder.as_deref(),
            Some("Cards/Ideas")
        );
        assert_eq!(
            loaded.config.kanban.new_note_template.as_deref(),
            Some("Kanban Card")
        );
        assert!(loaded.config.kanban.hide_card_count);
        assert!(loaded.config.kanban.hide_tags_in_title);
        assert!(loaded.config.kanban.hide_tags_display);
        assert_eq!(
            loaded.config.kanban.inline_metadata_position.as_deref(),
            Some("metadata-table")
        );
        assert_eq!(loaded.config.kanban.lane_width, Some(300));
        assert_eq!(loaded.config.kanban.full_list_lane_width, Some(true));
        assert_eq!(loaded.config.kanban.list_collapse, vec![true, false]);
        assert_eq!(loaded.config.kanban.max_archive_size, Some(42));
        assert!(loaded.config.kanban.show_checkboxes);
        assert_eq!(loaded.config.kanban.move_dates, Some(true));
        assert_eq!(loaded.config.kanban.move_tags, Some(false));
        assert_eq!(loaded.config.kanban.move_task_metadata, Some(true));
        assert_eq!(loaded.config.kanban.show_add_list, Some(false));
        assert_eq!(loaded.config.kanban.show_archive_all, Some(false));
        assert_eq!(loaded.config.kanban.show_board_settings, Some(false));
        assert_eq!(loaded.config.kanban.show_relative_date, Some(true));
        assert_eq!(loaded.config.kanban.show_search, Some(false));
        assert_eq!(loaded.config.kanban.show_set_view, Some(false));
        assert_eq!(loaded.config.kanban.show_view_as_markdown, Some(false));
        assert_eq!(loaded.config.kanban.date_picker_week_start, Some(1));
        assert_eq!(loaded.config.kanban.table_sizing.get("Title"), Some(&240));
        assert_eq!(loaded.config.kanban.tag_action.as_deref(), Some("kanban"));
        assert_eq!(
            loaded.config.kanban.tag_colors,
            vec![KanbanTagColorConfig {
                tag_key: "#urgent".to_string(),
                color: Some("#ffffff".to_string()),
                background_color: Some("#cc0000".to_string()),
            }]
        );
        assert_eq!(
            loaded.config.kanban.tag_sort,
            vec![KanbanTagSortConfig {
                tag: "#urgent".to_string()
            }]
        );
        assert_eq!(
            loaded.config.kanban.date_colors,
            vec![KanbanDateColorConfig {
                is_today: Some(true),
                is_before: None,
                is_after: None,
                distance: None,
                unit: None,
                direction: None,
                color: Some("#ffffff".to_string()),
                background_color: Some("#2d6cdf".to_string()),
            }]
        );
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
    fn templater_settings_follow_vulcan_and_local_precedence() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/templater-obsidian"))
            .expect("templater plugin dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/templater-obsidian/data.json"),
            r##"{
              "command_timeout": 5,
              "templates_folder": "Templater/Templates",
              "templates_pairs": [["slugify", "node scripts/slugify.js"]],
              "trigger_on_file_creation": false,
              "auto_jump_to_cursor": false,
              "enable_system_commands": false,
              "shell_path": "/bin/bash",
              "user_scripts_folder": "Scripts/User",
              "enable_folder_templates": true,
              "folder_templates": [{ "folder": "Daily", "template": "Daily Template" }],
              "enable_file_templates": false,
              "file_templates": [{ "regex": "^Projects/.*\\\\.md$", "template": "Project Template" }],
              "syntax_highlighting": true,
              "syntax_highlighting_mobile": false,
              "enabled_templates_hotkeys": ["Daily"],
              "startup_templates": ["Startup"],
              "intellisense_render": 1
            }"##,
        )
        .expect("templater config should be written");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r###"[templates]
templater_folder = "Shared/Templater"
command_timeout = 12
templates_pairs = [{ name = "slugify", command = "bun run slugify" }]
trigger_on_file_creation = true
auto_jump_to_cursor = true
enable_system_commands = true
shell_path = "/usr/bin/fish"
user_scripts_folder = "Scripts/Shared"
enable_folder_templates = false
folder_templates = [{ folder = "Projects", template = "Project Template" }]
enable_file_templates = true
file_templates = [{ regex = "^Daily/.*\\.md$", template = "Daily Template" }]
syntax_highlighting = false
syntax_highlighting_mobile = true
enabled_templates_hotkeys = ["Shared Daily"]
startup_templates = ["Shared Startup"]
intellisense_render = 4
"###,
        )
        .expect("shared config should be written");
        fs::write(
            vault_root.join(".vulcan/config.local.toml"),
            r###"[templates]
command_timeout = 20
templater_folder = "Device/Templates"
shell_path = "/bin/zsh"
user_scripts_folder = "Scripts/Device"
enabled_templates_hotkeys = ["Device Daily"]
startup_templates = ["Device Startup"]
intellisense_render = 2
"###,
        )
        .expect("local config should be written");

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(
            loaded.config.templates.templater_folder,
            Some(PathBuf::from("Device/Templates"))
        );
        assert_eq!(loaded.config.templates.command_timeout, 20);
        assert_eq!(
            loaded.config.templates.templates_pairs,
            vec![TemplaterCommandPairConfig {
                name: "slugify".to_string(),
                command: "bun run slugify".to_string(),
            }]
        );
        assert!(loaded.config.templates.trigger_on_file_creation);
        assert!(loaded.config.templates.auto_jump_to_cursor);
        assert!(loaded.config.templates.enable_system_commands);
        assert_eq!(
            loaded.config.templates.shell_path,
            Some(PathBuf::from("/bin/zsh"))
        );
        assert_eq!(
            loaded.config.templates.user_scripts_folder,
            Some(PathBuf::from("Scripts/Device"))
        );
        assert!(!loaded.config.templates.enable_folder_templates);
        assert_eq!(
            loaded.config.templates.folder_templates,
            vec![TemplaterFolderTemplateConfig {
                folder: PathBuf::from("Projects"),
                template: "Project Template".to_string(),
            }]
        );
        assert!(loaded.config.templates.enable_file_templates);
        assert_eq!(
            loaded.config.templates.file_templates,
            vec![TemplaterFileTemplateConfig {
                regex: "^Daily/.*\\.md$".to_string(),
                template: "Daily Template".to_string(),
            }]
        );
        assert!(!loaded.config.templates.syntax_highlighting);
        assert!(loaded.config.templates.syntax_highlighting_mobile);
        assert_eq!(
            loaded.config.templates.enabled_templates_hotkeys,
            vec!["Device Daily".to_string()]
        );
        assert_eq!(
            loaded.config.templates.startup_templates,
            vec!["Device Startup".to_string()]
        );
        assert_eq!(loaded.config.templates.intellisense_render, 2);
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

[kanban]
date_trigger = "@"
archive_date_format = "YYYY-MM-DD HH:mm"
lane_width = 256
show_search = true
metadata_keys = ["status"]

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

[kanban]
date_trigger = "DUE"
date_format = "DD.MM.YYYY"
time_format = "HH:mm:ss"
lane_width = 320
show_search = false
metadata_keys = [{ metadata_key = "owner", label = "Owner" }]

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
        assert_eq!(loaded.config.kanban.date_trigger, "DUE");
        assert_eq!(loaded.config.kanban.date_format, "DD.MM.YYYY");
        assert_eq!(loaded.config.kanban.time_format, "HH:mm:ss");
        assert_eq!(
            loaded.config.kanban.archive_date_format,
            "DD.MM.YYYY HH:mm:ss"
        );
        assert_eq!(loaded.config.kanban.lane_width, Some(320));
        assert_eq!(loaded.config.kanban.show_search, Some(false));
        assert_eq!(
            kanban_metadata_key_names(&loaded.config.kanban.metadata_keys),
            vec!["owner".to_string()]
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

    #[test]
    fn import_templater_plugin_config_preserves_existing_sections_and_is_idempotent() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/templater-obsidian"))
            .expect("templater plugin dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/templater-obsidian/data.json"),
            r##"{
              "command_timeout": 12,
              "templates_folder": "Templater/Templates",
              "templates_pairs": [["slugify", "bun run slugify"], ["", ""]],
              "trigger_on_file_creation": true,
              "auto_jump_to_cursor": true,
              "enable_system_commands": true,
              "shell_path": "/bin/zsh",
              "user_scripts_folder": "Scripts/User",
              "enable_folder_templates": false,
              "folder_templates": [
                { "folder": "Daily", "template": "Daily Template" },
                { "folder": "", "template": "" }
              ],
              "enable_file_templates": true,
              "file_templates": [
                { "regex": "^Projects/.*\\\\.md$", "template": "Project Template" },
                { "regex": "", "template": "" }
              ],
              "syntax_highlighting": false,
              "syntax_highlighting_mobile": true,
              "enabled_templates_hotkeys": ["Daily", ""],
              "startup_templates": ["Startup", ""],
              "intellisense_render": 4
            }"##,
        )
        .expect("templater config should be written");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("existing config should be written");
        let paths = VaultPaths::new(vault_root);

        let report = import_templater_plugin_config(&paths).expect("import should succeed");

        assert_eq!(report.plugin, "templater");
        assert!(!report.created_config);
        assert!(report.updated);
        assert!(report
            .mappings
            .iter()
            .any(|mapping| mapping.target == "templates.templater_folder"
                && mapping.value == Value::String("Templater/Templates".to_string())));

        let rendered = fs::read_to_string(paths.config_file()).expect("config should exist");
        assert!(rendered.contains("[git]"));
        assert!(rendered.contains("auto_commit = true"));
        assert!(rendered.contains("[templates]"));
        assert!(rendered.contains("templater_folder = \"Templater/Templates\""));
        assert!(rendered.contains("command_timeout = 12"));
        assert!(rendered.contains("[[templates.templates_pairs]]"));
        assert!(rendered.contains("name = \"slugify\""));
        assert!(rendered.contains("command = \"bun run slugify\""));
        assert!(rendered.contains("trigger_on_file_creation = true"));
        assert!(rendered.contains("auto_jump_to_cursor = true"));
        assert!(rendered.contains("enable_system_commands = true"));
        assert!(rendered.contains("shell_path = \"/bin/zsh\""));
        assert!(rendered.contains("user_scripts_folder = \"Scripts/User\""));
        assert!(rendered.contains("enable_folder_templates = false"));
        assert!(rendered.contains("[[templates.folder_templates]]"));
        assert!(rendered.contains("folder = \"Daily\""));
        assert!(rendered.contains("template = \"Daily Template\""));
        assert!(rendered.contains("enable_file_templates = true"));
        assert!(rendered.contains("[[templates.file_templates]]"));
        assert!(rendered.contains("template = \"Project Template\""));
        assert!(rendered.contains("syntax_highlighting = false"));
        assert!(rendered.contains("syntax_highlighting_mobile = true"));
        assert!(rendered.contains("enabled_templates_hotkeys = [\"Daily\"]"));
        assert!(rendered.contains("startup_templates = [\"Startup\"]"));
        assert!(rendered.contains("intellisense_render = 4"));

        let second_report =
            import_templater_plugin_config(&paths).expect("second import should succeed");
        assert!(!second_report.updated);
    }

    #[test]
    fn import_templater_plugin_config_errors_when_source_is_missing() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        let error = import_templater_plugin_config(&paths).expect_err("import should fail");
        assert!(matches!(error, ConfigImportError::MissingSource(_)));
    }

    #[test]
    fn import_kanban_plugin_config_preserves_existing_sections_and_is_idempotent() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/obsidian-kanban"))
            .expect("kanban plugin dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/obsidian-kanban/data.json"),
            r##"{
              "date-trigger": "DUE",
              "time-trigger": "AT",
              "date-format": "DD/MM/YYYY",
              "time-format": "HH:mm:ss",
              "date-display-format": "ddd DD MMM",
              "link-date-to-daily-note": true,
              "metadata-keys": [
                {
                  "metadataKey": "status",
                  "label": "Status",
                  "shouldHideLabel": true,
                  "containsMarkdown": true
                },
                { "metadataKey": "owner", "label": "Owner" }
              ],
              "archive-with-date": true,
              "append-archive-date": true,
              "archive-date-format": "DD/MM/YYYY HH:mm:ss",
              "archive-date-separator": " :: ",
              "new-card-insertion-method": "prepend",
              "new-line-trigger": "enter",
              "hide-card-count": true,
              "hide-tags-in-title": true,
              "hide-tags-display": true,
              "lane-width": 320,
              "max-archive-size": 50,
              "show-checkboxes": true,
              "show-search": false,
              "tag-action": "kanban",
              "tag-colors": [
                {
                  "tagKey": "#urgent",
                  "color": "#ffffff",
                  "backgroundColor": "#cc0000"
                }
              ]
            }"##,
        )
        .expect("kanban config should be written");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("existing config should be written");
        let paths = VaultPaths::new(vault_root);

        let report = import_kanban_plugin_config(&paths).expect("import should succeed");

        assert_eq!(report.plugin, "kanban");
        assert!(!report.created_config);
        assert!(report.updated);
        assert!(report
            .mappings
            .iter()
            .any(|mapping| mapping.target == "kanban.date_trigger"
                && mapping.value == Value::String("DUE".to_string())));

        let rendered = fs::read_to_string(paths.config_file()).expect("config should exist");
        assert!(rendered.contains("[git]"));
        assert!(rendered.contains("auto_commit = true"));
        assert!(rendered.contains("[kanban]"));
        assert!(rendered.contains("date_trigger = \"DUE\""));
        assert!(rendered.contains("time_trigger = \"AT\""));
        assert!(rendered.contains("date_format = \"DD/MM/YYYY\""));
        assert!(rendered.contains("time_format = \"HH:mm:ss\""));
        assert!(rendered.contains("date_display_format = \"ddd DD MMM\""));
        assert!(rendered.contains("link_date_to_daily_note = true"));
        assert!(rendered.contains("[[kanban.metadata_keys]]"));
        assert!(rendered.contains("metadata_key = \"status\""));
        assert!(rendered.contains("should_hide_label = true"));
        assert!(rendered.contains("contains_markdown = true"));
        assert!(rendered.contains("metadata_key = \"owner\""));
        assert!(rendered.contains("archive_with_date = true"));
        assert!(rendered.contains("append_archive_date = true"));
        assert!(rendered.contains("archive_date_format = \"DD/MM/YYYY HH:mm:ss\""));
        assert!(rendered.contains("archive_date_separator = \" :: \""));
        assert!(rendered.contains("new_card_insertion_method = \"prepend\""));
        assert!(rendered.contains("new_line_trigger = \"enter\""));
        assert!(rendered.contains("hide_card_count = true"));
        assert!(rendered.contains("hide_tags_in_title = true"));
        assert!(rendered.contains("hide_tags_display = true"));
        assert!(rendered.contains("lane_width = 320"));
        assert!(rendered.contains("max_archive_size = 50"));
        assert!(rendered.contains("show_checkboxes = true"));
        assert!(rendered.contains("show_search = false"));
        assert!(rendered.contains("tag_action = \"kanban\""));
        assert!(rendered.contains("[[kanban.tag_colors]]"));
        assert!(rendered.contains("tag_key = \"#urgent\""));

        let second_report =
            import_kanban_plugin_config(&paths).expect("second import should succeed");
        assert!(!second_report.updated);
    }

    #[test]
    fn import_kanban_plugin_config_errors_when_source_is_missing() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        let error = import_kanban_plugin_config(&paths).expect_err("import should fail");
        assert!(matches!(error, ConfigImportError::MissingSource(_)));
    }
}
