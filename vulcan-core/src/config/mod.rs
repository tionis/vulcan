use crate::bases::inspect_base_file;
use crate::paths::{
    ensure_vulcan_dir, normalize_relative_input_path, RelativePathOptions, VaultPaths,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
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
# timezone = "+02:00"  # optional fixed offset override; default is system local time
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
# web_allowlist = ["raw.githubusercontent.com", "templater-unsplash-2.fly.dev"]
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

# [quickadd]
# template_folder = "QuickAdd/Templates"
# global_variables = { project = "[[Projects/Alpha]]", agenda = "- {{VALUE:title}}" }
#
# [[quickadd.capture_choices]]
# id = "daily-capture"
# name = "Daily Capture"
# capture_to = "Journal/Daily/{{DATE:YYYY-MM-DD}}"
# format = "- {{VALUE:title|case:slug}}"
# prepend = false
#
# [[quickadd.template_choices]]
# id = "project-note"
# name = "Project Note"
# template_path = "Templates/Project Template.md"
# file_name_format = "{{VALUE:title|case:slug}}"

# [periodic.daily]
# enabled = true
# folder = "Journal/Daily"
# format = "YYYY-MM-DD"
# template = "daily"
# schedule_heading = "Schedule"
#
# [periodic.weekly]
# enabled = true
# folder = "Journal/Weekly"
# format = "YYYY-[W]ww"
# template = "weekly"
# start_of_week = "monday"
#
# [periodic.monthly]
# enabled = true
# folder = "Journal/Monthly"
# format = "YYYY-MM"
# template = "monthly"
#
# [periodic.quarterly]
# enabled = false
# folder = "Journal/Quarterly"
# format = "YYYY-[Q]Q"
# template = "quarterly"
#
# [periodic.yearly]
# enabled = false
# folder = "Journal/Yearly"
# format = "YYYY"
# template = "yearly"
#
# [periodic.sprint]
# enabled = true
# folder = "Journal/Sprints"
# format = "YYYY-[Sprint]-MM-DD"
# unit = "weeks"
# interval = 2
# anchor_date = "2026-01-05"
# template = "sprint"
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
pub struct WebSearchConfig {
    pub backend: String,
    pub api_key_env: String,
    pub base_url: String,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            backend: "kagi".to_string(),
            api_key_env: "KAGI_API_KEY".to_string(),
            base_url: "https://kagi.com/api/v0/search".to_string(),
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
        }
    }
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
    pub task_creation_defaults: TaskNotesTaskCreationDefaults,
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
            task_creation_defaults: TaskNotesTaskCreationDefaults::default(),
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
    pub templates: TemplatesConfig,
    pub quickadd: QuickAddConfig,
    pub web: WebConfig,
    pub periodic: PeriodicConfig,
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
            templates: TemplatesConfig::default(),
            quickadd: QuickAddConfig::default(),
            web: WebConfig::default(),
            periodic: PeriodicConfig::default(),
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
#[allow(clippy::struct_excessive_bools)]
pub struct ConfigImportReport {
    pub plugin: String,
    pub source_path: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_paths: Vec<PathBuf>,
    pub config_path: PathBuf,
    pub target_file: PathBuf,
    pub created_config: bool,
    pub updated: bool,
    #[serde(skip)]
    pub config_updated: bool,
    pub dry_run: bool,
    pub mappings: Vec<ConfigImportMapping>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub migrated_files: Vec<ImportMigratedFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<ImportSkippedSetting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<ImportConflict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMigratedFileAction {
    Copy,
    ValidateOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportMigratedFile {
    pub source: PathBuf,
    pub target: PathBuf,
    pub action: ImportMigratedFileAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportSkippedSetting {
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportConflict {
    pub key: String,
    pub sources: Vec<String>,
    pub kept_value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportTarget {
    Shared,
    Local,
}

impl ImportTarget {
    fn config_path(self, paths: &VaultPaths) -> PathBuf {
        match self {
            Self::Shared => paths.config_file().to_path_buf(),
            Self::Local => paths.local_config_file().to_path_buf(),
        }
    }
}

pub trait PluginImporter {
    fn name(&self) -> &'static str;

    fn display_name(&self) -> &'static str;

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf>;

    fn detect(&self, paths: &VaultPaths) -> bool {
        self.source_paths(paths).iter().any(|path| path.exists())
    }

    fn import(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        self.import_with_mode(paths, target, false)
    }

    fn dry_run(&self, paths: &VaultPaths) -> Result<ConfigImportReport, ConfigImportError> {
        self.import_with_mode(paths, ImportTarget::Shared, true)
    }

    fn dry_run_to(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        self.import_with_mode(paths, target, true)
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError>;
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
    tasknotes: Option<PartialTaskNotesConfig>,
    kanban: Option<PartialKanbanConfig>,
    dataview: Option<PartialDataviewConfig>,
    templates: Option<PartialTemplatesConfig>,
    quickadd: Option<PartialQuickAddConfig>,
    web: Option<PartialWebConfig>,
    periodic: Option<PartialPeriodicConfig>,
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
    web_allowlist: Option<Vec<String>>,
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
struct PartialQuickAddConfig {
    template_folder: Option<PathBuf>,
    global_variables: Option<BTreeMap<String, String>>,
    capture_choices: Option<Vec<QuickAddCaptureChoiceConfig>>,
    template_choices: Option<Vec<QuickAddTemplateChoiceConfig>>,
    ai: Option<PartialQuickAddAiConfig>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialQuickAddAiConfig {
    default_model: Option<String>,
    default_system_prompt: Option<String>,
    prompt_templates_folder: Option<PathBuf>,
    show_assistant: Option<bool>,
    providers: Option<Vec<QuickAddAiProviderConfig>>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialWebConfig {
    user_agent: Option<String>,
    search: Option<PartialWebSearchConfig>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialWebSearchConfig {
    backend: Option<String>,
    api_key_env: Option<String>,
    base_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialPeriodicConfig {
    #[serde(flatten)]
    notes: BTreeMap<String, PartialPeriodicNoteConfig>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialPeriodicNoteConfig {
    enabled: Option<bool>,
    folder: Option<PathBuf>,
    format: Option<String>,
    unit: Option<PeriodicCadenceUnit>,
    interval: Option<usize>,
    anchor_date: Option<String>,
    template: Option<String>,
    start_of_week: Option<PeriodicStartOfWeek>,
    schedule_heading: Option<String>,
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
struct PartialTaskNotesConfig {
    tasks_folder: Option<String>,
    archive_folder: Option<String>,
    task_tag: Option<String>,
    identification_method: Option<TaskNotesIdentificationMethod>,
    task_property_name: Option<String>,
    task_property_value: Option<String>,
    excluded_folders: Option<Vec<String>>,
    default_status: Option<String>,
    default_priority: Option<String>,
    field_mapping: Option<PartialTaskNotesFieldMapping>,
    statuses: Option<Vec<TaskNotesStatusConfig>>,
    priorities: Option<Vec<TaskNotesPriorityConfig>>,
    user_fields: Option<Vec<TaskNotesUserFieldConfig>>,
    enable_natural_language_input: Option<bool>,
    nlp_default_to_scheduled: Option<bool>,
    nlp_language: Option<String>,
    nlp_triggers: Option<Vec<TaskNotesNlpTriggerConfig>>,
    task_creation_defaults: Option<TaskNotesTaskCreationDefaults>,
}

#[derive(Debug, Deserialize, Default)]
struct PartialTaskNotesFieldMapping {
    title: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    due: Option<String>,
    scheduled: Option<String>,
    contexts: Option<String>,
    projects: Option<String>,
    time_estimate: Option<String>,
    completed_date: Option<String>,
    date_created: Option<String>,
    date_modified: Option<String>,
    recurrence: Option<String>,
    recurrence_anchor: Option<String>,
    archive_tag: Option<String>,
    time_entries: Option<String>,
    complete_instances: Option<String>,
    skipped_instances: Option<String>,
    blocked_by: Option<String>,
    reminders: Option<String>,
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
    timezone: Option<String>,
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
struct ObsidianDailyNotesConfig {
    folder: Option<String>,
    format: Option<String>,
    template: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianPeriodicNoteSettings {
    enabled: Option<bool>,
    folder: Option<String>,
    format: Option<String>,
    #[serde(rename = "templatePath", alias = "template")]
    template_path: Option<String>,
    #[serde(rename = "startOfWeek")]
    start_of_week: Option<PeriodicStartOfWeek>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianPeriodicNotesConfig {
    daily: Option<ObsidianPeriodicNoteSettings>,
    weekly: Option<ObsidianPeriodicNoteSettings>,
    monthly: Option<ObsidianPeriodicNoteSettings>,
    quarterly: Option<ObsidianPeriodicNoteSettings>,
    yearly: Option<ObsidianPeriodicNoteSettings>,
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
struct ObsidianQuickAddConfig {
    #[serde(rename = "templateFolderPath")]
    template_folder_path: Option<String>,
    #[serde(rename = "globalVariables", default)]
    global_variables: BTreeMap<String, String>,
    #[serde(default)]
    choices: Vec<ObsidianQuickAddChoice>,
    ai: Option<ObsidianQuickAddAiConfig>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianQuickAddChoice {
    id: Option<String>,
    name: Option<String>,
    #[serde(rename = "type")]
    choice_type: Option<String>,
    #[serde(rename = "captureTo")]
    capture_to: Option<String>,
    #[serde(rename = "captureToActiveFile")]
    capture_to_active_file: Option<bool>,
    #[serde(rename = "activeFileWritePosition")]
    active_file_write_position: Option<String>,
    #[serde(rename = "createFileIfItDoesntExist")]
    create_file_if_it_doesnt_exist: Option<ObsidianQuickAddCreateFileConfig>,
    format: Option<ObsidianQuickAddFormatConfig>,
    #[serde(rename = "useSelectionAsCaptureValue")]
    use_selection_as_capture_value: Option<bool>,
    prepend: Option<bool>,
    task: Option<bool>,
    #[serde(rename = "insertAfter")]
    insert_after: Option<ObsidianQuickAddInsertAfterConfig>,
    #[serde(rename = "newLineCapture")]
    new_line_capture: Option<ObsidianQuickAddNewLineCaptureConfig>,
    #[serde(rename = "openFile")]
    open_file: Option<bool>,
    templater: Option<ObsidianQuickAddTemplaterChoiceConfig>,
    #[serde(rename = "templatePath")]
    template_path: Option<String>,
    folder: Option<ObsidianQuickAddTemplateFolderConfig>,
    #[serde(rename = "fileNameFormat")]
    file_name_format: Option<ObsidianQuickAddFormatConfig>,
    #[serde(rename = "fileExistsBehavior")]
    file_exists_behavior: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianQuickAddCreateFileConfig {
    enabled: Option<bool>,
    #[serde(rename = "createWithTemplate")]
    create_with_template: Option<bool>,
    template: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianQuickAddFormatConfig {
    enabled: Option<bool>,
    format: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Deserialize, Default)]
struct ObsidianQuickAddInsertAfterConfig {
    enabled: Option<bool>,
    #[serde(rename = "after")]
    heading: Option<String>,
    #[serde(rename = "insertAtEnd")]
    insert_at_end: Option<bool>,
    #[serde(rename = "considerSubsections")]
    consider_subsections: Option<bool>,
    #[serde(rename = "createIfNotFound")]
    create_if_not_found: Option<bool>,
    #[serde(rename = "createIfNotFoundLocation")]
    create_if_not_found_location: Option<String>,
    inline: Option<bool>,
    #[serde(rename = "replaceExisting")]
    replace_existing: Option<bool>,
    #[serde(rename = "blankLineAfterMatchMode")]
    blank_line_after_match_mode: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianQuickAddNewLineCaptureConfig {
    enabled: Option<bool>,
    direction: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianQuickAddTemplaterChoiceConfig {
    #[serde(rename = "afterCapture")]
    after_capture: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Deserialize, Default)]
struct ObsidianQuickAddTemplateFolderConfig {
    enabled: Option<bool>,
    #[serde(default)]
    folders: Vec<String>,
    #[serde(rename = "chooseWhenCreatingNote")]
    choose_when_creating_note: Option<bool>,
    #[serde(rename = "createInSameFolderAsActiveFile")]
    create_in_same_folder_as_active_file: Option<bool>,
    #[serde(rename = "chooseFromSubfolders")]
    choose_from_subfolders: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianQuickAddAiConfig {
    #[serde(rename = "defaultModel")]
    default_model: Option<String>,
    #[serde(rename = "defaultSystemPrompt")]
    default_system_prompt: Option<String>,
    #[serde(rename = "promptTemplatesFolderPath")]
    prompt_templates_folder_path: Option<String>,
    #[serde(rename = "showAssistant")]
    show_assistant: Option<bool>,
    #[serde(default)]
    providers: Vec<ObsidianQuickAddAiProviderConfig>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianQuickAddAiProviderConfig {
    name: Option<String>,
    endpoint: Option<String>,
    #[serde(rename = "apiKeyRef")]
    api_key_ref: Option<String>,
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
    #[serde(default)]
    models: Vec<ObsidianQuickAddAiModelConfig>,
    #[serde(rename = "autoSyncModels")]
    auto_sync_models: Option<bool>,
    #[serde(rename = "modelSource")]
    model_source: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianQuickAddAiModelConfig {
    name: Option<String>,
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
    #[serde(rename = "timezone")]
    timezone: Option<String>,
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
struct ObsidianTaskNotesConfig {
    #[serde(rename = "tasksFolder")]
    tasks_folder: Option<String>,
    #[serde(rename = "archiveFolder")]
    archive_folder: Option<String>,
    #[serde(rename = "taskTag")]
    task_tag: Option<String>,
    #[serde(rename = "taskIdentificationMethod")]
    task_identification_method: Option<TaskNotesIdentificationMethod>,
    #[serde(rename = "taskPropertyName")]
    task_property_name: Option<String>,
    #[serde(rename = "taskPropertyValue")]
    task_property_value: Option<String>,
    #[serde(rename = "excludedFolders")]
    excluded_folders: Option<String>,
    #[serde(rename = "defaultTaskStatus")]
    default_task_status: Option<String>,
    #[serde(rename = "defaultTaskPriority")]
    default_task_priority: Option<String>,
    #[serde(rename = "fieldMapping")]
    field_mapping: Option<ObsidianTaskNotesFieldMapping>,
    #[serde(rename = "customStatuses", default)]
    custom_statuses: Vec<TaskNotesStatusConfig>,
    #[serde(rename = "customPriorities", default)]
    custom_priorities: Vec<TaskNotesPriorityConfig>,
    #[serde(rename = "userFields", default)]
    user_fields: Vec<TaskNotesUserFieldConfig>,
    #[serde(rename = "enableNaturalLanguageInput")]
    enable_natural_language_input: Option<bool>,
    #[serde(rename = "nlpDefaultToScheduled")]
    nlp_default_to_scheduled: Option<bool>,
    #[serde(rename = "nlpLanguage")]
    nlp_language: Option<String>,
    #[serde(rename = "nlpTriggers")]
    nlp_triggers: Option<ObsidianTaskNotesNlpTriggersConfig>,
    #[serde(rename = "taskCreationDefaults")]
    task_creation_defaults: Option<ObsidianTaskNotesCreationDefaults>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianTaskNotesFieldMapping {
    title: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    due: Option<String>,
    scheduled: Option<String>,
    contexts: Option<String>,
    projects: Option<String>,
    #[serde(rename = "timeEstimate")]
    time_estimate: Option<String>,
    #[serde(rename = "completedDate")]
    completed_date: Option<String>,
    #[serde(rename = "dateCreated")]
    date_created: Option<String>,
    #[serde(rename = "dateModified")]
    date_modified: Option<String>,
    recurrence: Option<String>,
    #[serde(rename = "recurrenceAnchor")]
    recurrence_anchor: Option<String>,
    #[serde(rename = "archiveTag")]
    archive_tag: Option<String>,
    #[serde(rename = "timeEntries")]
    time_entries: Option<String>,
    #[serde(rename = "completeInstances")]
    complete_instances: Option<String>,
    #[serde(rename = "skippedInstances")]
    skipped_instances: Option<String>,
    #[serde(rename = "blockedBy")]
    blocked_by: Option<String>,
    reminders: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ObsidianTaskNotesNlpTriggersConfig {
    #[serde(default)]
    triggers: Vec<TaskNotesNlpTriggerConfig>,
}

#[allow(clippy::struct_field_names)]
#[derive(Debug, Deserialize, Default)]
struct ObsidianTaskNotesCreationDefaults {
    #[serde(rename = "defaultContexts")]
    default_contexts: Option<String>,
    #[serde(rename = "defaultTags")]
    default_tags: Option<String>,
    #[serde(rename = "defaultProjects")]
    default_projects: Option<String>,
    #[serde(rename = "defaultTimeEstimate")]
    default_time_estimate: Option<usize>,
    #[serde(rename = "defaultDueDate")]
    default_due_date: Option<TaskNotesDateDefault>,
    #[serde(rename = "defaultScheduledDate")]
    default_scheduled_date: Option<TaskNotesDateDefault>,
    #[serde(rename = "defaultRecurrence")]
    default_recurrence: Option<TaskNotesRecurrenceDefault>,
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

fn default_tasknotes_auto_archive_delay() -> usize {
    5
}

fn default_tasknotes_statuses() -> Vec<TaskNotesStatusConfig> {
    vec![
        TaskNotesStatusConfig {
            id: "none".to_string(),
            value: "none".to_string(),
            label: "None".to_string(),
            color: "#cccccc".to_string(),
            is_completed: false,
            order: 0,
            auto_archive: false,
            auto_archive_delay: default_tasknotes_auto_archive_delay(),
        },
        TaskNotesStatusConfig {
            id: "open".to_string(),
            value: "open".to_string(),
            label: "Open".to_string(),
            color: "#808080".to_string(),
            is_completed: false,
            order: 1,
            auto_archive: false,
            auto_archive_delay: default_tasknotes_auto_archive_delay(),
        },
        TaskNotesStatusConfig {
            id: "in-progress".to_string(),
            value: "in-progress".to_string(),
            label: "In progress".to_string(),
            color: "#0066cc".to_string(),
            is_completed: false,
            order: 2,
            auto_archive: false,
            auto_archive_delay: default_tasknotes_auto_archive_delay(),
        },
        TaskNotesStatusConfig {
            id: "done".to_string(),
            value: "done".to_string(),
            label: "Done".to_string(),
            color: "#00aa00".to_string(),
            is_completed: true,
            order: 3,
            auto_archive: false,
            auto_archive_delay: default_tasknotes_auto_archive_delay(),
        },
    ]
}

fn default_tasknotes_priorities() -> Vec<TaskNotesPriorityConfig> {
    vec![
        TaskNotesPriorityConfig {
            id: "none".to_string(),
            value: "none".to_string(),
            label: "None".to_string(),
            color: "#cccccc".to_string(),
            weight: 0,
        },
        TaskNotesPriorityConfig {
            id: "low".to_string(),
            value: "low".to_string(),
            label: "Low".to_string(),
            color: "#00aa00".to_string(),
            weight: 1,
        },
        TaskNotesPriorityConfig {
            id: "normal".to_string(),
            value: "normal".to_string(),
            label: "Normal".to_string(),
            color: "#ffaa00".to_string(),
            weight: 2,
        },
        TaskNotesPriorityConfig {
            id: "high".to_string(),
            value: "high".to_string(),
            label: "High".to_string(),
            color: "#ff0000".to_string(),
            weight: 3,
        },
    ]
}

fn default_tasknotes_nlp_language() -> String {
    "en".to_string()
}

fn default_tasknotes_nlp_triggers() -> Vec<TaskNotesNlpTriggerConfig> {
    vec![
        TaskNotesNlpTriggerConfig {
            property_id: "contexts".to_string(),
            trigger: "@".to_string(),
            enabled: true,
        },
        TaskNotesNlpTriggerConfig {
            property_id: "tags".to_string(),
            trigger: "#".to_string(),
            enabled: true,
        },
        TaskNotesNlpTriggerConfig {
            property_id: "projects".to_string(),
            trigger: "+".to_string(),
            enabled: true,
        },
        TaskNotesNlpTriggerConfig {
            property_id: "status".to_string(),
            trigger: "*".to_string(),
            enabled: true,
        },
        TaskNotesNlpTriggerConfig {
            property_id: "priority".to_string(),
            trigger: "!".to_string(),
            enabled: false,
        },
    ]
}

fn default_dataview_inline_query_prefix() -> String {
    "=".to_string()
}

fn default_dataview_inline_js_query_prefix() -> String {
    "$=".to_string()
}

fn default_true() -> bool {
    true
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

#[derive(Debug, Clone)]
struct ImportSetting {
    source: String,
    target: String,
    path: Vec<String>,
    value: Value,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CoreImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct DataviewImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct KanbanImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct PeriodicNotesImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct QuickAddImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct TaskNotesImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct TasksImporter;

#[derive(Debug, Clone, Copy, Default)]
pub struct TemplaterImporter;

#[must_use]
pub fn all_importers() -> Vec<Box<dyn PluginImporter>> {
    vec![
        Box::new(CoreImporter),
        Box::new(DataviewImporter),
        Box::new(KanbanImporter),
        Box::new(PeriodicNotesImporter),
        Box::new(QuickAddImporter),
        Box::new(TaskNotesImporter),
        Box::new(TasksImporter),
        Box::new(TemplaterImporter),
    ]
}

pub fn import_tasks_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    TasksImporter.import(paths, ImportTarget::Shared)
}

pub fn import_tasknotes_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    TaskNotesImporter.import(paths, ImportTarget::Shared)
}

pub fn import_quickadd_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    QuickAddImporter.import(paths, ImportTarget::Shared)
}

pub fn import_templater_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    TemplaterImporter.import(paths, ImportTarget::Shared)
}

pub fn import_kanban_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    KanbanImporter.import(paths, ImportTarget::Shared)
}

pub fn import_periodic_notes_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    PeriodicNotesImporter.import(paths, ImportTarget::Shared)
}

pub fn import_core_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    CoreImporter.import(paths, ImportTarget::Shared)
}

pub fn import_dataview_plugin_config(
    paths: &VaultPaths,
) -> Result<ConfigImportReport, ConfigImportError> {
    DataviewImporter.import(paths, ImportTarget::Shared)
}

fn importer_source_path(paths: &VaultPaths, relative: &str) -> PathBuf {
    paths.vault_root().join(relative)
}

fn import_settings_from_mappings(mappings: Vec<ConfigImportMapping>) -> Vec<ImportSetting> {
    mappings
        .into_iter()
        .map(|mapping| ImportSetting {
            source: mapping.source,
            target: mapping.target.clone(),
            path: mapping
                .target
                .split('.')
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>(),
            value: mapping.value,
        })
        .collect()
}

fn import_settings_to_mappings(settings: &[ImportSetting]) -> Vec<ConfigImportMapping> {
    settings
        .iter()
        .map(|setting| ConfigImportMapping {
            source: setting.source.clone(),
            target: setting.target.clone(),
            value: setting.value.clone(),
        })
        .collect()
}

fn import_setting<T: Serialize>(
    settings: &mut Vec<ImportSetting>,
    source: &str,
    path: &[&str],
    value: &T,
) -> Result<(), ConfigImportError> {
    import_setting_path(
        settings,
        source,
        path.iter().map(|segment| (*segment).to_string()).collect(),
        value,
    )
}

fn import_setting_path<T: Serialize>(
    settings: &mut Vec<ImportSetting>,
    source: &str,
    path: Vec<String>,
    value: &T,
) -> Result<(), ConfigImportError> {
    settings.push(ImportSetting {
        source: source.to_string(),
        target: path.join("."),
        path,
        value: serde_json::to_value(value)?,
    });
    Ok(())
}

fn apply_import_settings(
    paths: &VaultPaths,
    plugin: &str,
    source_path: PathBuf,
    source_paths: Vec<PathBuf>,
    settings: &[ImportSetting],
    target: ImportTarget,
    dry_run: bool,
) -> Result<ConfigImportReport, ConfigImportError> {
    if !dry_run {
        ensure_vulcan_dir(paths)?;
    }

    let target_file = target.config_path(paths);
    let created_config = !target_file.exists();
    let existing_contents = fs::read_to_string(&target_file).ok();
    let mut config_value = load_config_value(&target_file)?;
    merge_import_into_toml(&mut config_value, settings)?;
    let rendered = toml::to_string_pretty(&config_value)?;
    let updated = existing_contents.as_deref() != Some(rendered.as_str());
    if updated && !dry_run {
        fs::write(&target_file, rendered)?;
    }

    Ok(ConfigImportReport {
        plugin: plugin.to_string(),
        source_path,
        source_paths,
        config_path: target_file.clone(),
        target_file,
        created_config,
        updated,
        config_updated: updated,
        dry_run,
        mappings: import_settings_to_mappings(settings),
        migrated_files: Vec::new(),
        skipped: Vec::new(),
        conflicts: Vec::new(),
    })
}

fn merge_import_into_toml(
    config_value: &mut toml::Value,
    settings: &[ImportSetting],
) -> Result<(), ConfigImportError> {
    let Some(root_table) = config_value.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(
            "expected .vulcan config to contain a TOML table".to_string(),
        ));
    };

    for setting in settings {
        merge_import_setting(root_table, &setting.path, &setting.value)?;
    }
    Ok(())
}

fn merge_import_setting(
    table: &mut toml::map::Map<String, toml::Value>,
    path: &[String],
    value: &Value,
) -> Result<(), ConfigImportError> {
    let Some((segment, rest)) = path.split_first() else {
        return Err(ConfigImportError::InvalidConfig(
            "import setting path cannot be empty".to_string(),
        ));
    };

    if rest.is_empty() {
        match json_to_toml_value(value)? {
            Some(value) => {
                table.insert(segment.clone(), value);
            }
            None => {
                table.remove(segment);
            }
        }
        return Ok(());
    }

    let entry = table
        .entry(segment.clone())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !entry.is_table() {
        *entry = toml::Value::Table(toml::map::Map::new());
    }
    let Some(child_table) = entry.as_table_mut() else {
        return Err(ConfigImportError::InvalidConfig(format!(
            "expected [{}] to be a TOML table",
            path[..path.len() - rest.len()].join(".")
        )));
    };

    merge_import_setting(child_table, rest, value)
}

fn json_to_toml_value(value: &Value) -> Result<Option<toml::Value>, ConfigImportError> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(value) => Ok(Some(toml::Value::Boolean(*value))),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(Some(toml::Value::Integer(value)))
            } else if let Some(value) = number.as_u64() {
                let integer = i64::try_from(value).map_err(|_| {
                    ConfigImportError::InvalidConfig(
                        "numeric config import value does not fit in signed 64-bit TOML integer"
                            .to_string(),
                    )
                })?;
                Ok(Some(toml::Value::Integer(integer)))
            } else if let Some(value) = number.as_f64() {
                Ok(Some(toml::Value::Float(value)))
            } else {
                Err(ConfigImportError::InvalidConfig(
                    "unsupported numeric config import value".to_string(),
                ))
            }
        }
        Value::String(text) => Ok(Some(toml::Value::String(text.clone()))),
        Value::Array(values) => {
            let mut items = Vec::new();
            for value in values {
                if let Some(value) = json_to_toml_value(value)? {
                    items.push(value);
                }
            }
            Ok(Some(toml::Value::Array(items)))
        }
        Value::Object(entries) => {
            let mut table = toml::map::Map::new();
            for (key, value) in entries {
                if let Some(value) = json_to_toml_value(value)? {
                    table.insert(key.clone(), value);
                }
            }
            Ok(Some(toml::Value::Table(table)))
        }
    }
}

pub fn annotate_import_conflicts(reports: &mut [ConfigImportReport]) {
    let mut previous_sources = BTreeMap::<String, Vec<String>>::new();

    for report in reports {
        report.conflicts.clear();
        for mapping in &report.mappings {
            if let Some(sources) = previous_sources.get_mut(&mapping.target) {
                if !sources.iter().any(|source| source == &report.plugin) {
                    sources.push(report.plugin.clone());
                }
                report.conflicts.push(ImportConflict {
                    key: mapping.target.clone(),
                    sources: sources.clone(),
                    kept_value: mapping.value.clone(),
                });
            } else {
                previous_sources.insert(mapping.target.clone(), vec![report.plugin.clone()]);
            }
        }
    }
}

impl PluginImporter for TasksImporter {
    fn name(&self) -> &'static str {
        "tasks"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian Tasks plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/obsidian-tasks-plugin/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let obsidian =
            serde_json::from_str::<ObsidianTasksConfig>(&fs::read_to_string(&source_path)?)?;
        let imported_tasks = imported_tasks_config(obsidian);
        let settings =
            import_settings_from_mappings(tasks_config_import_mappings(&imported_tasks)?);
        apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )
    }
}

impl PluginImporter for TemplaterImporter {
    fn name(&self) -> &'static str {
        "templater"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian Templater plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/templater-obsidian/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let obsidian =
            serde_json::from_str::<ObsidianTemplaterConfig>(&fs::read_to_string(&source_path)?)?;
        let imported_templates = imported_templater_config(obsidian);
        let settings =
            import_settings_from_mappings(templater_config_import_mappings(&imported_templates)?);
        apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )
    }
}

impl PluginImporter for QuickAddImporter {
    fn name(&self) -> &'static str {
        "quickadd"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian QuickAdd plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/quickadd/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let source = fs::read_to_string(&source_path)?;
        let raw = serde_json::from_str::<Value>(&source)?;
        let obsidian = serde_json::from_value::<ObsidianQuickAddConfig>(raw.clone())?;
        let imported_quickadd = imported_quickadd_config(obsidian);
        let settings =
            import_settings_from_mappings(quickadd_config_import_mappings(&imported_quickadd)?);
        let mut report = apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )?;
        report.skipped = quickadd_skipped_settings(&raw);
        Ok(report)
    }
}

impl PluginImporter for KanbanImporter {
    fn name(&self) -> &'static str {
        "kanban"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian Kanban plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/obsidian-kanban/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let obsidian =
            serde_json::from_str::<ObsidianKanbanConfig>(&fs::read_to_string(&source_path)?)?;
        let imported_kanban = imported_kanban_config(obsidian);
        let settings =
            import_settings_from_mappings(kanban_config_import_mappings(&imported_kanban)?);
        apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )
    }
}

impl PluginImporter for PeriodicNotesImporter {
    fn name(&self) -> &'static str {
        "periodic-notes"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian Daily Notes and Periodic Notes"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![
            importer_source_path(paths, ".obsidian/daily-notes.json"),
            importer_source_path(paths, ".obsidian/plugins/periodic-notes/data.json"),
        ]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_paths = self
            .source_paths(paths)
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        if source_paths.is_empty() {
            return Err(ConfigImportError::MissingSource(importer_source_path(
                paths,
                ".obsidian/plugins/periodic-notes/data.json",
            )));
        }

        let mut mappings = Vec::new();
        let daily_path = importer_source_path(paths, ".obsidian/daily-notes.json");
        if daily_path.exists() {
            let daily = serde_json::from_str::<ObsidianDailyNotesConfig>(&fs::read_to_string(
                &daily_path,
            )?)?;
            mappings.extend(periodic_daily_notes_import_mappings(&daily)?);
        }

        let periodic_path =
            importer_source_path(paths, ".obsidian/plugins/periodic-notes/data.json");
        if periodic_path.exists() {
            let periodic = serde_json::from_str::<ObsidianPeriodicNotesConfig>(
                &fs::read_to_string(&periodic_path)?,
            )?;
            mappings.extend(periodic_plugin_import_mappings(&periodic)?);
        }

        let settings = import_settings_from_mappings(mappings);
        apply_import_settings(
            paths,
            self.name(),
            source_paths[0].clone(),
            source_paths,
            &settings,
            target,
            dry_run,
        )
    }
}

impl PluginImporter for TaskNotesImporter {
    fn name(&self) -> &'static str {
        "tasknotes"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian TaskNotes plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/tasknotes/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let source = fs::read_to_string(&source_path)?;
        let raw = serde_json::from_str::<Value>(&source)?;
        let obsidian = serde_json::from_value::<ObsidianTaskNotesConfig>(raw.clone())?;
        let imported_tasknotes = imported_tasknotes_config(obsidian);
        let settings =
            import_settings_from_mappings(tasknotes_config_import_mappings(&imported_tasknotes)?);
        let mut report = apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )?;
        let migration = tasknotes_migrate_view_files(paths, &raw, dry_run)?;
        report.source_paths.extend(migration.source_paths);
        report.source_paths.sort();
        report.source_paths.dedup();
        report.migrated_files = migration.migrated_files;
        report.skipped = tasknotes_skipped_settings(&raw);
        report.skipped.extend(migration.skipped);
        if report
            .migrated_files
            .iter()
            .any(|file| matches!(file.action, ImportMigratedFileAction::Copy))
        {
            report.updated = true;
        }
        Ok(report)
    }
}

impl PluginImporter for CoreImporter {
    fn name(&self) -> &'static str {
        "core"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian core settings"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![
            importer_source_path(paths, ".obsidian/app.json"),
            importer_source_path(paths, ".obsidian/templates.json"),
            importer_source_path(paths, ".obsidian/types.json"),
        ]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_root = paths.vault_root().join(".obsidian");
        let source_paths = self
            .source_paths(paths)
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        if source_paths.is_empty() {
            return Err(ConfigImportError::MissingSource(source_root));
        }

        let settings = core_import_settings(paths)?;
        apply_import_settings(
            paths,
            self.name(),
            paths.vault_root().join(".obsidian"),
            source_paths,
            &settings,
            target,
            dry_run,
        )
    }
}

impl PluginImporter for DataviewImporter {
    fn name(&self) -> &'static str {
        "dataview"
    }

    fn display_name(&self) -> &'static str {
        "Obsidian Dataview plugin"
    }

    fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf> {
        vec![importer_source_path(
            paths,
            ".obsidian/plugins/dataview/data.json",
        )]
    }

    fn import_with_mode(
        &self,
        paths: &VaultPaths,
        target: ImportTarget,
        dry_run: bool,
    ) -> Result<ConfigImportReport, ConfigImportError> {
        let source_path = self
            .source_paths(paths)
            .into_iter()
            .next()
            .expect("source path");
        if !source_path.exists() {
            return Err(ConfigImportError::MissingSource(source_path));
        }

        let obsidian =
            serde_json::from_str::<ObsidianDataviewConfig>(&fs::read_to_string(&source_path)?)?;
        let imported_dataview = imported_dataview_config(obsidian);
        let settings =
            import_settings_from_mappings(dataview_config_import_mappings(&imported_dataview)?);
        apply_import_settings(
            paths,
            self.name(),
            source_path.clone(),
            vec![source_path],
            &settings,
            target,
            dry_run,
        )
    }
}

fn core_import_settings(paths: &VaultPaths) -> Result<Vec<ImportSetting>, ConfigImportError> {
    let app_path = importer_source_path(paths, ".obsidian/app.json");
    let templates_path = importer_source_path(paths, ".obsidian/templates.json");
    let types_path = importer_source_path(paths, ".obsidian/types.json");
    let mut settings = Vec::new();

    if app_path.exists() {
        let app = serde_json::from_str::<ObsidianAppConfig>(&fs::read_to_string(&app_path)?)?;
        if let Some(use_markdown_links) = app.use_markdown_links {
            let link_style = if use_markdown_links {
                LinkStylePreference::Markdown
            } else {
                LinkStylePreference::Wikilink
            };
            import_setting(
                &mut settings,
                "app.json.useMarkdownLinks",
                &["links", "style"],
                &link_style,
            )?;
        }
        if let Some(new_link_format) = app.new_link_format {
            import_setting(
                &mut settings,
                "app.json.newLinkFormat",
                &["links", "resolution"],
                &new_link_format,
            )?;
        }
        if let Some(attachment_folder_path) = app.attachment_folder_path {
            let normalized = normalize_attachment_folder(&attachment_folder_path);
            import_setting(
                &mut settings,
                "app.json.attachmentFolderPath",
                &["links", "attachment_folder"],
                &normalized,
            )?;
        }
        if let Some(strict_line_breaks) = app.strict_line_breaks {
            import_setting(
                &mut settings,
                "app.json.strictLineBreaks",
                &["strict_line_breaks"],
                &strict_line_breaks,
            )?;
        }
    }

    if templates_path.exists() {
        let templates =
            serde_json::from_str::<ObsidianTemplatesConfig>(&fs::read_to_string(&templates_path)?)?;
        if let Some(date_format) = templates.date_format {
            import_setting(
                &mut settings,
                "templates.json.dateFormat",
                &["templates", "date_format"],
                &date_format,
            )?;
        }
        if let Some(time_format) = templates.time_format {
            import_setting(
                &mut settings,
                "templates.json.timeFormat",
                &["templates", "time_format"],
                &time_format,
            )?;
        }
        if let Some(folder) = templates.folder {
            let normalized = normalize_template_path(Some(folder));
            import_setting(
                &mut settings,
                "templates.json.folder",
                &["templates", "obsidian_folder"],
                &normalized,
            )?;
        }
    }

    if types_path.exists() {
        for (property, value_type) in load_explicit_obsidian_property_types(&types_path)? {
            import_setting_path(
                &mut settings,
                "types.json",
                vec!["property_types".to_string(), property],
                &value_type,
            )?;
        }
    }

    Ok(settings)
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

fn imported_quickadd_config(obsidian: ObsidianQuickAddConfig) -> QuickAddConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_quickadd_defaults(&mut config, obsidian);
    config.quickadd
}

fn imported_dataview_config(obsidian: ObsidianDataviewConfig) -> DataviewConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_dataview_defaults(&mut config, obsidian);
    config.dataview
}

fn imported_tasknotes_config(obsidian: ObsidianTaskNotesConfig) -> TaskNotesConfig {
    let mut config = VaultConfig::default();
    apply_obsidian_tasknotes_defaults(&mut config, obsidian);
    config.tasknotes
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

fn quickadd_config_import_mappings(
    config: &QuickAddConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "templateFolderPath",
        "quickadd.template_folder",
        &config.template_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "globalVariables",
        "quickadd.global_variables",
        &config.global_variables,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "choices[type=Capture]",
        "quickadd.capture_choices",
        &config.capture_choices,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "choices[type=Template]",
        "quickadd.template_choices",
        &config.template_choices,
    )?;
    push_config_import_mapping(&mut mappings, "ai", "quickadd.ai", &config.ai)?;
    Ok(mappings)
}

fn dataview_config_import_mappings(
    config: &DataviewConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "inlineQueryPrefix",
        "dataview.inline_query_prefix",
        &config.inline_query_prefix,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "inlineJsQueryPrefix",
        "dataview.inline_js_query_prefix",
        &config.inline_js_query_prefix,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enableDataviewJs",
        "dataview.enable_dataview_js",
        &config.enable_dataview_js,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enableInlineDataviewJs",
        "dataview.enable_inline_dataview_js",
        &config.enable_inline_dataview_js,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCompletionTracking",
        "dataview.task_completion_tracking",
        &config.task_completion_tracking,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCompletionUseEmojiShorthand",
        "dataview.task_completion_use_emoji_shorthand",
        &config.task_completion_use_emoji_shorthand,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCompletionText",
        "dataview.task_completion_text",
        &config.task_completion_text,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "recursiveSubTaskCompletion",
        "dataview.recursive_subtask_completion",
        &config.recursive_subtask_completion,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "displayResultCount",
        "dataview.display_result_count",
        &config.display_result_count,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "defaultDateFormat",
        "dataview.default_date_format",
        &config.default_date_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "defaultDateTimeFormat",
        "dataview.default_datetime_format",
        &config.default_datetime_format,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "timezone",
        "dataview.timezone",
        &config.timezone,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "maxRecursiveRenderDepth",
        "dataview.max_recursive_render_depth",
        &config.max_recursive_render_depth,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "primaryColumnName",
        "dataview.primary_column_name",
        &config.primary_column_name,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "groupColumnName",
        "dataview.group_column_name",
        &config.group_column_name,
    )?;
    Ok(mappings)
}

fn quickadd_skipped_settings(raw: &Value) -> Vec<ImportSkippedSetting> {
    let Some(settings) = raw.as_object() else {
        return Vec::new();
    };

    let mut skipped = Vec::new();
    if let Some(choices) = settings.get("choices").and_then(Value::as_array) {
        for (index, choice) in choices.iter().enumerate() {
            let choice_type = choice.get("type").and_then(Value::as_str).unwrap_or("");
            let choice_name = choice
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .or_else(|| choice.get("id").and_then(Value::as_str))
                .unwrap_or("unnamed-choice");
            let source = format!("choices[{index}] ({choice_name})");
            if choice_type.eq_ignore_ascii_case("Macro") {
                skipped.push(ImportSkippedSetting {
                    source,
                    reason: "QuickAdd Macro choices are not imported; migrate them to `vulcan run --script` or shell automation".to_string(),
                });
            } else if choice_type.eq_ignore_ascii_case("Multi") {
                skipped.push(ImportSkippedSetting {
                    source,
                    reason: "QuickAdd Multi choices are not imported; migrate them to a `vulcan run --script` orchestration flow".to_string(),
                });
            } else if !choice_type.is_empty()
                && !choice_type.eq_ignore_ascii_case("Capture")
                && !choice_type.eq_ignore_ascii_case("Template")
            {
                skipped.push(ImportSkippedSetting {
                    source,
                    reason: format!("QuickAdd choice type `{choice_type}` is not supported"),
                });
            }
        }
    }

    if let Some(providers) = settings
        .get("ai")
        .and_then(Value::as_object)
        .and_then(|ai| ai.get("providers"))
        .and_then(Value::as_array)
    {
        for (index, provider) in providers.iter().enumerate() {
            let api_key = provider.get("apiKey").and_then(Value::as_str).unwrap_or("");
            if api_key.trim().is_empty() {
                continue;
            }
            let provider_name = provider
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("provider");
            let env_name = quickadd_provider_api_key_env(
                provider_name,
                provider.get("apiKeyRef").and_then(Value::as_str),
                Some(api_key),
            )
            .unwrap_or_else(|| "PROVIDER_API_KEY".to_string());
            skipped.push(ImportSkippedSetting {
                source: format!("ai.providers[{index}].apiKey"),
                reason: format!(
                    "stored API keys are not imported; set `{env_name}` in the environment instead"
                ),
            });
        }
    }

    skipped
}

#[allow(clippy::too_many_lines)]
fn tasknotes_config_import_mappings(
    config: &TaskNotesConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "tasksFolder",
        "tasknotes.tasks_folder",
        &config.tasks_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "archiveFolder",
        "tasknotes.archive_folder",
        &config.archive_folder,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskTag",
        "tasknotes.task_tag",
        &config.task_tag,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskIdentificationMethod",
        "tasknotes.identification_method",
        &config.identification_method,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskPropertyName",
        "tasknotes.task_property_name",
        &config.task_property_name,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskPropertyValue",
        "tasknotes.task_property_value",
        &config.task_property_value,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "excludedFolders",
        "tasknotes.excluded_folders",
        &config.excluded_folders,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "defaultTaskStatus",
        "tasknotes.default_status",
        &config.default_status,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "defaultTaskPriority",
        "tasknotes.default_priority",
        &config.default_priority,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.title",
        "tasknotes.field_mapping.title",
        &config.field_mapping.title,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.status",
        "tasknotes.field_mapping.status",
        &config.field_mapping.status,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.priority",
        "tasknotes.field_mapping.priority",
        &config.field_mapping.priority,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.due",
        "tasknotes.field_mapping.due",
        &config.field_mapping.due,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.scheduled",
        "tasknotes.field_mapping.scheduled",
        &config.field_mapping.scheduled,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.contexts",
        "tasknotes.field_mapping.contexts",
        &config.field_mapping.contexts,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.projects",
        "tasknotes.field_mapping.projects",
        &config.field_mapping.projects,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.timeEstimate",
        "tasknotes.field_mapping.time_estimate",
        &config.field_mapping.time_estimate,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.completedDate",
        "tasknotes.field_mapping.completed_date",
        &config.field_mapping.completed_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.dateCreated",
        "tasknotes.field_mapping.date_created",
        &config.field_mapping.date_created,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.dateModified",
        "tasknotes.field_mapping.date_modified",
        &config.field_mapping.date_modified,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.recurrence",
        "tasknotes.field_mapping.recurrence",
        &config.field_mapping.recurrence,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.recurrenceAnchor",
        "tasknotes.field_mapping.recurrence_anchor",
        &config.field_mapping.recurrence_anchor,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.archiveTag",
        "tasknotes.field_mapping.archive_tag",
        &config.field_mapping.archive_tag,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.timeEntries",
        "tasknotes.field_mapping.time_entries",
        &config.field_mapping.time_entries,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.completeInstances",
        "tasknotes.field_mapping.complete_instances",
        &config.field_mapping.complete_instances,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.skippedInstances",
        "tasknotes.field_mapping.skipped_instances",
        &config.field_mapping.skipped_instances,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.blockedBy",
        "tasknotes.field_mapping.blocked_by",
        &config.field_mapping.blocked_by,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "fieldMapping.reminders",
        "tasknotes.field_mapping.reminders",
        &config.field_mapping.reminders,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "customStatuses",
        "tasknotes.statuses",
        &config.statuses,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "customPriorities",
        "tasknotes.priorities",
        &config.priorities,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "userFields",
        "tasknotes.user_fields",
        &config.user_fields,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "enableNaturalLanguageInput",
        "tasknotes.enable_natural_language_input",
        &config.enable_natural_language_input,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "nlpDefaultToScheduled",
        "tasknotes.nlp_default_to_scheduled",
        &config.nlp_default_to_scheduled,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "nlpLanguage",
        "tasknotes.nlp_language",
        &config.nlp_language,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "nlpTriggers.triggers",
        "tasknotes.nlp_triggers",
        &config.nlp_triggers,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultContexts",
        "tasknotes.task_creation_defaults.default_contexts",
        &config.task_creation_defaults.default_contexts,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultTags",
        "tasknotes.task_creation_defaults.default_tags",
        &config.task_creation_defaults.default_tags,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultProjects",
        "tasknotes.task_creation_defaults.default_projects",
        &config.task_creation_defaults.default_projects,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultTimeEstimate",
        "tasknotes.task_creation_defaults.default_time_estimate",
        &config.task_creation_defaults.default_time_estimate,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultDueDate",
        "tasknotes.task_creation_defaults.default_due_date",
        &config.task_creation_defaults.default_due_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultScheduledDate",
        "tasknotes.task_creation_defaults.default_scheduled_date",
        &config.task_creation_defaults.default_scheduled_date,
    )?;
    push_config_import_mapping(
        &mut mappings,
        "taskCreationDefaults.defaultRecurrence",
        "tasknotes.task_creation_defaults.default_recurrence",
        &config.task_creation_defaults.default_recurrence,
    )?;
    Ok(mappings)
}

#[derive(Debug, Default)]
struct TaskNotesViewMigrationResult {
    source_paths: Vec<PathBuf>,
    migrated_files: Vec<ImportMigratedFile>,
    skipped: Vec<ImportSkippedSetting>,
}

fn tasknotes_view_target_path(command: &str) -> Option<&'static str> {
    match command {
        "open-calendar-view" => Some("TaskNotes/Views/mini-calendar-default.base"),
        "open-kanban-view" => Some("TaskNotes/Views/kanban-default.base"),
        "open-tasks-view" => Some("TaskNotes/Views/tasks-default.base"),
        "open-advanced-calendar-view" => Some("TaskNotes/Views/calendar-default.base"),
        "open-agenda-view" => Some("TaskNotes/Views/agenda-default.base"),
        "relationships" | "project-subtasks" => Some("TaskNotes/Views/relationships.base"),
        _ => None,
    }
}

fn tasknotes_command_file_mappings(raw: &Value) -> Vec<(String, String)> {
    let mut mappings = BTreeMap::from([
        (
            "open-calendar-view".to_string(),
            "TaskNotes/Views/mini-calendar-default.base".to_string(),
        ),
        (
            "open-kanban-view".to_string(),
            "TaskNotes/Views/kanban-default.base".to_string(),
        ),
        (
            "open-tasks-view".to_string(),
            "TaskNotes/Views/tasks-default.base".to_string(),
        ),
        (
            "open-advanced-calendar-view".to_string(),
            "TaskNotes/Views/calendar-default.base".to_string(),
        ),
        (
            "open-agenda-view".to_string(),
            "TaskNotes/Views/agenda-default.base".to_string(),
        ),
        (
            "relationships".to_string(),
            "TaskNotes/Views/relationships.base".to_string(),
        ),
    ]);

    if let Some(command_mapping) = raw.get("commandFileMapping").and_then(Value::as_object) {
        for (command, path) in command_mapping {
            if let Some(path) = path.as_str() {
                mappings.insert(command.clone(), path.to_string());
            }
        }
        if !command_mapping.contains_key("relationships") {
            if let Some(path) = command_mapping
                .get("project-subtasks")
                .and_then(Value::as_str)
            {
                mappings.insert("project-subtasks".to_string(), path.to_string());
            }
        }
    }

    mappings.into_iter().collect()
}

fn normalize_tasknotes_import_path(path: &str) -> Result<String, ConfigImportError> {
    normalize_relative_input_path(
        path,
        RelativePathOptions {
            expected_extension: Some("base"),
            append_extension_if_missing: true,
        },
    )
    .map_err(|error| ConfigImportError::InvalidConfig(error.to_string()))
}

fn normalize_tasknotes_import_source_type(source_type: &str) -> String {
    source_type
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn supports_tasknotes_import_view_type(view_type: &str) -> bool {
    matches!(
        normalize_tasknotes_import_source_type(view_type).as_str(),
        "table" | "tasknotestasklist" | "tasknoteskanban"
    )
}

fn tasknotes_base_contents_for_vulcan(source: &str, source_type: &str) -> String {
    if normalize_tasknotes_import_source_type(source_type) == "tasknotes" {
        source.to_string()
    } else {
        format!("source: tasknotes\n\n{source}")
    }
}

#[allow(clippy::too_many_lines)]
fn tasknotes_migrate_view_files(
    paths: &VaultPaths,
    raw: &Value,
    dry_run: bool,
) -> Result<TaskNotesViewMigrationResult, ConfigImportError> {
    let mut result = TaskNotesViewMigrationResult::default();
    let mut source_paths = BTreeSet::new();
    let mut target_sources = BTreeMap::<String, String>::new();
    let explicit_commands = raw
        .get("commandFileMapping")
        .and_then(Value::as_object)
        .map(|mapping| mapping.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();

    for (command, source_path) in tasknotes_command_file_mappings(raw) {
        let Some(target_path) = tasknotes_view_target_path(&command) else {
            continue;
        };
        let source_path = match normalize_tasknotes_import_path(&source_path) {
            Ok(path) => path,
            Err(error) => {
                result.skipped.push(ImportSkippedSetting {
                    source: format!("commandFileMapping.{command}"),
                    reason: error.to_string(),
                });
                continue;
            }
        };

        if let Some(previous_source) =
            target_sources.insert(target_path.to_string(), source_path.clone())
        {
            if previous_source != source_path {
                result.skipped.push(ImportSkippedSetting {
                    source: format!("commandFileMapping.{command}"),
                    reason: format!(
                        "target `{target_path}` already maps to `{previous_source}` during import"
                    ),
                });
                continue;
            }
        }

        let source_absolute = paths.vault_root().join(&source_path);
        if !source_absolute.exists() {
            if explicit_commands.contains(&command) {
                result.skipped.push(ImportSkippedSetting {
                    source: format!("commandFileMapping.{command}"),
                    reason: format!("view file `{source_path}` was not found"),
                });
            }
            continue;
        }
        source_paths.insert(source_absolute.clone());

        let info = match inspect_base_file(paths, &source_path) {
            Ok(info) => info,
            Err(error) => {
                result.skipped.push(ImportSkippedSetting {
                    source: format!("commandFileMapping.{command}"),
                    reason: format!("view file `{source_path}` could not be parsed: {error}"),
                });
                continue;
            }
        };

        if let Some(diagnostic) = info.diagnostics.first() {
            result.skipped.push(ImportSkippedSetting {
                source: format!("commandFileMapping.{command}"),
                reason: format!(
                    "view file `{source_path}` has unsupported syntax: {}",
                    diagnostic.message
                ),
            });
            continue;
        }

        let normalized_source_type = normalize_tasknotes_import_source_type(&info.source_type);
        if !matches!(normalized_source_type.as_str(), "file" | "tasknotes") {
            result.skipped.push(ImportSkippedSetting {
                source: format!("commandFileMapping.{command}"),
                reason: format!(
                    "view file `{source_path}` uses unsupported source type `{}`",
                    info.source_type
                ),
            });
            continue;
        }
        if info.views.is_empty() {
            result.skipped.push(ImportSkippedSetting {
                source: format!("commandFileMapping.{command}"),
                reason: format!("view file `{source_path}` does not define any views"),
            });
            continue;
        }

        let unsupported_view_types = info
            .views
            .iter()
            .filter(|view| !supports_tasknotes_import_view_type(&view.view_type))
            .map(|view| view.view_type.clone())
            .collect::<BTreeSet<_>>();
        if !unsupported_view_types.is_empty() {
            result.skipped.push(ImportSkippedSetting {
                source: format!("commandFileMapping.{command}"),
                reason: format!(
                    "view file `{source_path}` uses unsupported view types: {}",
                    unsupported_view_types
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
            continue;
        }

        let source_contents = fs::read_to_string(&source_absolute)?;
        let migrated_contents =
            tasknotes_base_contents_for_vulcan(&source_contents, &info.source_type);
        let target_absolute = paths.vault_root().join(target_path);
        let action = if target_absolute.exists() {
            let existing_contents = fs::read_to_string(&target_absolute)?;
            if existing_contents == migrated_contents {
                ImportMigratedFileAction::ValidateOnly
            } else if source_absolute == target_absolute {
                ImportMigratedFileAction::Copy
            } else {
                result.skipped.push(ImportSkippedSetting {
                    source: format!("commandFileMapping.{command}"),
                    reason: format!(
                        "target `{target_path}` already exists with different contents"
                    ),
                });
                continue;
            }
        } else {
            ImportMigratedFileAction::Copy
        };

        if matches!(action, ImportMigratedFileAction::Copy) && !dry_run {
            if let Some(parent) = target_absolute.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target_absolute, migrated_contents)?;
        }

        result.migrated_files.push(ImportMigratedFile {
            source: source_absolute,
            target: target_absolute,
            action,
        });
    }

    result.source_paths = source_paths.into_iter().collect();
    Ok(result)
}

#[allow(clippy::too_many_lines)]
fn tasknotes_skipped_settings(raw: &Value) -> Vec<ImportSkippedSetting> {
    let Some(settings) = raw.as_object() else {
        return Vec::new();
    };

    let mut skipped = Vec::new();
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &["calendarViewSettings"],
        "calendar view settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "pomodoroWorkDuration",
            "pomodoroShortBreakDuration",
            "pomodoroLongBreakDuration",
            "pomodoroLongBreakInterval",
            "pomodoroAutoStartBreaks",
            "pomodoroAutoStartWork",
            "pomodoroNotifications",
            "pomodoroSoundEnabled",
            "pomodoroSoundVolume",
            "pomodoroStorageLocation",
            "pomodoroMobileSidebar",
        ],
        "pomodoro settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "moveArchivedTasks",
            "hideIdentifyingTagsInCards",
            "taskOrgFiltersCollapsed",
            "taskFilenameFormat",
            "storeTitleInFilename",
            "customFilenameTemplate",
            "enableTaskLinkOverlay",
            "disableOverlayOnAlias",
            "enableInstantTaskConvert",
            "useDefaultsOnInstantConvert",
            "uiLanguage",
            "statusSuggestionTrigger",
            "projectAutosuggest",
            "singleClickAction",
            "doubleClickAction",
            "inlineTaskConvertFolder",
            "disableNoteIndexing",
            "suggestionDebounceMs",
            "recurrenceMigrated",
            "lastSeenVersion",
            "showReleaseNotesOnUpdate",
            "showTrackedTasksInStatusBar",
            "autoStopTimeTrackingOnComplete",
            "autoStopTimeTrackingNotification",
            "showRelationships",
            "relationshipsPosition",
            "showTaskCardInNote",
            "showExpandableSubtasks",
            "subtaskChevronPosition",
            "viewsButtonAlignment",
            "hideCompletedFromOverdue",
            "enableNotifications",
            "notificationType",
            "modalFieldsConfig",
            "enableModalSplitLayout",
            "defaultVisibleProperties",
            "inlineVisibleProperties",
        ],
        "UI and editor settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &["icsIntegration"],
        "ICS integration settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &["savedViews"],
        "saved views are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "enableBases",
            "enableMdbaseSpec",
            "autoCreateDefaultBasesFiles",
        ],
        "TaskNotes Bases integration settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "enableAPI",
            "apiPort",
            "apiAuthToken",
            "enableMCP",
            "webhooks",
        ],
        "API and webhook settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "googleOAuthClientId",
            "googleOAuthClientSecret",
            "enableGoogleCalendar",
            "enabledGoogleCalendars",
            "googleCalendarSyncTokens",
            "googleCalendarExport",
        ],
        "Google Calendar integration settings are not yet supported",
    );
    push_tasknotes_skipped_group(
        &mut skipped,
        settings,
        &[
            "microsoftOAuthClientId",
            "microsoftOAuthClientSecret",
            "enableMicrosoftCalendar",
            "enabledMicrosoftCalendars",
            "microsoftCalendarSyncTokens",
        ],
        "Microsoft Calendar integration settings are not yet supported",
    );

    if settings
        .get("taskCreationDefaults")
        .and_then(Value::as_object)
        .is_some_and(|defaults| defaults.contains_key("defaultReminders"))
    {
        skipped.push(ImportSkippedSetting {
            source: "taskCreationDefaults.defaultReminders".to_string(),
            reason: "default reminder settings are not yet supported".to_string(),
        });
    }

    skipped
}

fn push_tasknotes_skipped_group(
    skipped: &mut Vec<ImportSkippedSetting>,
    settings: &serde_json::Map<String, Value>,
    keys: &[&str],
    reason: &str,
) {
    let present = keys
        .iter()
        .filter(|key| settings.contains_key(**key))
        .copied()
        .collect::<Vec<_>>();
    if present.is_empty() {
        return;
    }

    skipped.push(ImportSkippedSetting {
        source: present.join(", "),
        reason: reason.to_string(),
    });
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

fn periodic_daily_notes_import_mappings(
    config: &ObsidianDailyNotesConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_config_import_mapping(
        &mut mappings,
        "daily-notes.folder",
        "periodic.daily.folder",
        &normalize_optional_text(config.folder.clone()).map(normalize_periodic_folder),
    )?;
    push_config_import_mapping(
        &mut mappings,
        "daily-notes.format",
        "periodic.daily.format",
        &normalize_optional_text(config.format.clone()),
    )?;
    push_config_import_mapping(
        &mut mappings,
        "daily-notes.template",
        "periodic.daily.template",
        &normalize_optional_text(config.template.clone()),
    )?;
    Ok(mappings)
}

fn periodic_plugin_import_mappings(
    config: &ObsidianPeriodicNotesConfig,
) -> Result<Vec<ConfigImportMapping>, ConfigImportError> {
    let mut mappings = Vec::new();
    push_periodic_plugin_mappings(&mut mappings, "daily", config.daily.as_ref())?;
    push_periodic_plugin_mappings(&mut mappings, "weekly", config.weekly.as_ref())?;
    push_periodic_plugin_mappings(&mut mappings, "monthly", config.monthly.as_ref())?;
    push_periodic_plugin_mappings(&mut mappings, "quarterly", config.quarterly.as_ref())?;
    push_periodic_plugin_mappings(&mut mappings, "yearly", config.yearly.as_ref())?;
    Ok(mappings)
}

fn push_periodic_plugin_mappings(
    mappings: &mut Vec<ConfigImportMapping>,
    period_type: &str,
    config: Option<&ObsidianPeriodicNoteSettings>,
) -> Result<(), ConfigImportError> {
    let Some(config) = config else {
        return Ok(());
    };

    push_config_import_mapping(
        mappings,
        &format!("{period_type}.enabled"),
        &format!("periodic.{period_type}.enabled"),
        &config.enabled,
    )?;
    push_config_import_mapping(
        mappings,
        &format!("{period_type}.folder"),
        &format!("periodic.{period_type}.folder"),
        &normalize_optional_text(config.folder.clone()).map(normalize_periodic_folder),
    )?;
    push_config_import_mapping(
        mappings,
        &format!("{period_type}.format"),
        &format!("periodic.{period_type}.format"),
        &normalize_optional_text(config.format.clone()),
    )?;
    push_config_import_mapping(
        mappings,
        &format!("{period_type}.templatePath"),
        &format!("periodic.{period_type}.template"),
        &normalize_optional_text(config.template_path.clone()),
    )?;
    push_config_import_mapping(
        mappings,
        &format!("{period_type}.startOfWeek"),
        &format!("periodic.{period_type}.start_of_week"),
        &config.start_of_week,
    )?;

    Ok(())
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
            "expected .vulcan config file to contain a TOML table".to_string(),
        ))
    }
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
    if let Some(defaults) = obsidian.task_creation_defaults {
        apply_obsidian_tasknotes_creation_defaults(
            &mut config.tasknotes.task_creation_defaults,
            defaults,
        );
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
        if let Some(task_creation_defaults) = tasknotes.task_creation_defaults {
            config.tasknotes.task_creation_defaults = task_creation_defaults;
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

    if let Some(web) = overrides.web {
        if let Some(user_agent) = web.user_agent {
            config.web.user_agent = user_agent;
        }
        if let Some(search) = web.search {
            if let Some(backend) = search.backend {
                config.web.search.backend = backend;
            }
            if let Some(api_key_env) = search.api_key_env {
                config.web.search.api_key_env = api_key_env;
            }
            if let Some(base_url) = search.base_url {
                config.web.search.base_url = base_url;
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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const OBSIDIAN_APP_JSON: &str = r#"{
      "useMarkdownLinks": true,
      "newLinkFormat": "relative",
      "attachmentFolderPath": "/",
      "strictLineBreaks": true
    }"#;
    const OBSIDIAN_TYPES_JSON: &str = r#"{
      "status": "text",
      "priority": { "type": "number" }
    }"#;
    const OBSIDIAN_TEMPLATES_JSON: &str = r#"{
      "folder": "Shared Templates",
      "dateFormat": "dddd, MMMM Do YYYY",
      "timeFormat": "hh:mm A"
    }"#;
    const OBSIDIAN_DAILY_NOTES_JSON: &str = r#"{
      "folder": "Journal/Core Daily",
      "format": "YYYY-MM-DD",
      "template": "Daily Core"
    }"#;
    const OBSIDIAN_PERIODIC_NOTES_JSON: &str = r#"{
      "daily": {
        "enabled": true,
        "folder": "Journal/Daily",
        "format": "YYYY-MM-DD",
        "templatePath": "daily"
      },
      "weekly": {
        "enabled": true,
        "folder": "Journal/Weekly",
        "format": "YYYY-[W]ww",
        "templatePath": "weekly",
        "startOfWeek": "sunday"
      },
      "monthly": {
        "enabled": true,
        "folder": "Journal/Monthly",
        "format": "YYYY-MM",
        "templatePath": "monthly"
      }
    }"#;
    const OBSIDIAN_DATAVIEW_JSON: &str = r#"{
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
      "timezone": "+02:00",
      "maxRecursiveRenderDepth": 7,
      "tableIdColumnName": "Document",
      "tableGroupColumnName": "Bucket"
    }"#;
    const OBSIDIAN_KANBAN_JSON: &str = r##"{
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
    }"##;
    const VULCAN_OVERRIDE_APP_JSON: &str = r#"{
      "useMarkdownLinks": true,
      "newLinkFormat": "relative",
      "attachmentFolderPath": "attachments"
    }"#;
    const VULCAN_OVERRIDE_CONFIG_TOML: &str = r###"[scan]
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
timezone = "+02:00"
max_recursive_render_depth = 8
primary_column_name = "Document"
group_column_name = "Bucket"

[templates]
date_format = "DD/MM/YYYY"
time_format = "HH:mm:ss"
"###;
    const QUICKADD_OVERRIDE_CONFIG_TOML: &str = r#"[quickadd]
template_folder = "QuickAdd/Overrides"
global_variables = { Project = "[[Projects/Beta]]" }

[quickadd.ai]
show_assistant = false
"#;
    const TEMPLATER_PLUGIN_DEFAULTS_JSON: &str = r#"{
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
    }"#;
    const OBSIDIAN_QUICKADD_JSON: &str = r###"{
      "templateFolderPath": "QuickAdd/Templates",
      "globalVariables": {
        "Project": "[[Projects/Alpha]]",
        "agenda": "- {{VALUE:title}} due {{VDATE:due,YYYY-MM-DD}}"
      },
      "choices": [
        {
          "id": "capture-daily",
          "name": "Daily Capture",
          "type": "Capture",
          "captureTo": "Journal/Daily/{{DATE:YYYY-MM-DD}}",
          "captureToActiveFile": false,
          "createFileIfItDoesntExist": {
            "enabled": true,
            "createWithTemplate": true,
            "template": "Daily Template"
          },
          "format": {
            "enabled": true,
            "format": "- {{VALUE:title|case:slug}}"
          },
          "useSelectionAsCaptureValue": true,
          "prepend": true,
          "task": true,
          "insertAfter": {
            "enabled": true,
            "after": "## Log",
            "insertAtEnd": true,
            "considerSubsections": true,
            "createIfNotFound": true,
            "createIfNotFoundLocation": "bottom"
          },
          "openFile": true,
          "templater": {
            "afterCapture": "wholeFile"
          }
        },
        {
          "id": "template-note",
          "name": "Template Note",
          "type": "Template",
          "templatePath": "Templates/Project Template.md",
          "folder": {
            "enabled": true,
            "folders": ["Projects", " Areas/Research/ "],
            "chooseWhenCreatingNote": true,
            "chooseFromSubfolders": true
          },
          "fileNameFormat": {
            "enabled": true,
            "format": "{{VALUE:title|case:slug}}"
          },
          "openFile": true,
          "fileExistsBehavior": "increment"
        }
      ],
      "ai": {
        "defaultModel": "gpt-4o-mini",
        "defaultSystemPrompt": "Summarize briefly.",
        "promptTemplatesFolderPath": "QuickAdd/Prompts",
        "showAssistant": true,
        "providers": [
          {
            "name": "OpenAI",
            "endpoint": "https://api.openai.com/v1",
            "apiKeyRef": "OPENAI_API_KEY",
            "apiKey": "",
            "modelSource": "providerApi",
            "models": [
              { "name": "gpt-4o-mini", "maxTokens": 128000 }
            ]
          }
        ]
      }
    }"###;
    const TEMPLATER_PRECEDENCE_PLUGIN_JSON: &str = r#"{
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
    }"#;
    const SHARED_TEMPLATER_CONFIG_TOML: &str = r#"[templates]
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
"#;
    const LOCAL_TEMPLATER_CONFIG_TOML: &str = r#"[templates]
command_timeout = 20
templater_folder = "Device/Templates"
shell_path = "/bin/zsh"
user_scripts_folder = "Scripts/Device"
enabled_templates_hotkeys = ["Device Daily"]
startup_templates = ["Device Startup"]
intellisense_render = 2
"#;

    fn kanban_metadata_key_names(keys: &[KanbanMetadataKeyConfig]) -> Vec<String> {
        keys.iter()
            .map(|key| match key {
                KanbanMetadataKeyConfig::Detailed(field) => field.metadata_key.clone(),
                KanbanMetadataKeyConfig::Key(key) => key.clone(),
            })
            .collect()
    }

    fn write_test_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir should be created");
        }
        fs::write(path, contents).expect("test file should be written");
    }

    fn setup_obsidian_seed_vault(vault_root: &Path) {
        write_test_file(&vault_root.join(".obsidian/app.json"), OBSIDIAN_APP_JSON);
        write_test_file(
            &vault_root.join(".obsidian/types.json"),
            OBSIDIAN_TYPES_JSON,
        );
        write_test_file(
            &vault_root.join(".obsidian/templates.json"),
            OBSIDIAN_TEMPLATES_JSON,
        );
        write_test_file(
            &vault_root.join(".obsidian/daily-notes.json"),
            OBSIDIAN_DAILY_NOTES_JSON,
        );
        write_test_file(
            &vault_root.join(".obsidian/plugins/dataview/data.json"),
            OBSIDIAN_DATAVIEW_JSON,
        );
        write_test_file(
            &vault_root.join(".obsidian/plugins/quickadd/data.json"),
            OBSIDIAN_QUICKADD_JSON,
        );
        write_test_file(
            &vault_root.join(".obsidian/plugins/periodic-notes/data.json"),
            OBSIDIAN_PERIODIC_NOTES_JSON,
        );
        write_test_file(
            &vault_root.join(".obsidian/plugins/obsidian-kanban/data.json"),
            OBSIDIAN_KANBAN_JSON,
        );
    }

    fn assert_obsidian_seed_core_defaults(config: &VaultConfig) {
        assert_eq!(config.link_style, LinkStylePreference::Markdown);
        assert_eq!(config.link_resolution, LinkResolutionMode::Relative);
        assert_eq!(config.attachment_folder, PathBuf::from("."));
        assert!(config.strict_line_breaks);
        assert_eq!(config.scan.default_mode, AutoScanMode::Blocking);
        assert_eq!(config.scan.browse_mode, AutoScanMode::Background);
        assert_eq!(config.templates.date_format, "dddd, MMMM Do YYYY");
        assert_eq!(config.templates.time_format, "hh:mm A");
        assert_eq!(
            config.templates.obsidian_folder,
            Some(PathBuf::from("Shared Templates"))
        );
        assert_eq!(
            config.property_types.get("status"),
            Some(&"text".to_string())
        );
        assert_eq!(
            config.property_types.get("priority"),
            Some(&"number".to_string())
        );
    }

    fn assert_obsidian_seed_dataview_defaults(config: &VaultConfig) {
        assert_eq!(config.dataview.inline_query_prefix, "dv:");
        assert_eq!(config.dataview.inline_js_query_prefix, "$dv:");
        assert!(!config.dataview.enable_dataview_js);
        assert!(config.dataview.enable_inline_dataview_js);
        assert!(config.dataview.task_completion_tracking);
        assert!(config.dataview.task_completion_use_emoji_shorthand);
        assert_eq!(config.dataview.task_completion_text, "done-on");
        assert!(config.dataview.recursive_subtask_completion);
        assert!(!config.dataview.display_result_count);
        assert_eq!(config.dataview.default_date_format, "yyyy-MM-dd");
        assert_eq!(config.dataview.default_datetime_format, "yyyy-MM-dd HH:mm");
        assert_eq!(config.dataview.timezone.as_deref(), Some("+02:00"));
        assert_eq!(config.dataview.max_recursive_render_depth, 7);
        assert_eq!(config.dataview.primary_column_name, "Document");
        assert_eq!(config.dataview.group_column_name, "Bucket");
    }

    fn assert_obsidian_seed_quickadd_defaults(config: &VaultConfig) {
        assert_eq!(
            config.quickadd.template_folder,
            Some(PathBuf::from("QuickAdd/Templates"))
        );
        assert_eq!(
            config.quickadd.global_variables.get("Project"),
            Some(&"[[Projects/Alpha]]".to_string())
        );
        assert_eq!(config.quickadd.capture_choices.len(), 1);
        assert_eq!(config.quickadd.capture_choices[0].id, "capture-daily");
        assert_eq!(
            config.quickadd.capture_choices[0].capture_to.as_deref(),
            Some("Journal/Daily/{{DATE:YYYY-MM-DD}}")
        );
        assert_eq!(
            config.quickadd.capture_choices[0].format.as_deref(),
            Some("- {{VALUE:title|case:slug}}")
        );
        assert_eq!(
            config.quickadd.capture_choices[0]
                .insert_after
                .as_ref()
                .map(|insert_after| insert_after.heading.as_str()),
            Some("## Log")
        );
        assert_eq!(config.quickadd.template_choices.len(), 1);
        assert_eq!(config.quickadd.template_choices[0].id, "template-note");
        assert_eq!(
            config.quickadd.template_choices[0].template_path,
            Some(PathBuf::from("Templates/Project Template.md"))
        );
        assert_eq!(
            config.quickadd.template_choices[0].folder.folders,
            vec![PathBuf::from("Projects"), PathBuf::from("Areas/Research")]
        );
        let ai = config
            .quickadd
            .ai
            .as_ref()
            .expect("quickadd ai config should be present");
        assert_eq!(ai.default_model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(
            ai.default_system_prompt.as_deref(),
            Some("Summarize briefly.")
        );
        assert_eq!(
            ai.prompt_templates_folder,
            Some(PathBuf::from("QuickAdd/Prompts"))
        );
        assert!(ai.show_assistant);
        assert_eq!(ai.providers.len(), 1);
        assert_eq!(ai.providers[0].name, "OpenAI");
        assert_eq!(
            ai.providers[0].api_key_env.as_deref(),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(ai.providers[0].models, vec!["gpt-4o-mini".to_string()]);
    }

    fn assert_obsidian_seed_kanban_defaults(config: &VaultConfig) {
        assert_eq!(config.kanban.date_trigger, "DUE");
        assert_eq!(config.kanban.time_trigger, "AT");
        assert_eq!(config.kanban.date_format, "DD/MM/YYYY");
        assert_eq!(config.kanban.time_format, "HH:mm:ss");
        assert_eq!(
            config.kanban.date_display_format.as_deref(),
            Some("ddd DD MMM")
        );
        assert_eq!(
            config.kanban.date_time_display_format.as_deref(),
            Some("ddd DD MMM HH:mm:ss")
        );
        assert!(config.kanban.link_date_to_daily_note);
        assert_eq!(
            kanban_metadata_key_names(&config.kanban.metadata_keys),
            vec!["status".to_string(), "owner".to_string()]
        );
        assert_eq!(
            config.kanban.metadata_keys[0],
            KanbanMetadataKeyConfig::Detailed(KanbanMetadataFieldConfig {
                metadata_key: "status".to_string(),
                label: Some("Status".to_string()),
                should_hide_label: true,
                contains_markdown: true,
            })
        );
        assert!(config.kanban.archive_with_date);
        assert!(config.kanban.append_archive_date);
        assert_eq!(config.kanban.archive_date_format, "DD/MM/YYYY HH:mm:ss");
        assert_eq!(
            config.kanban.archive_date_separator.as_deref(),
            Some(" :: ")
        );
        assert_eq!(config.kanban.new_card_insertion_method, "prepend");
        assert_eq!(config.kanban.new_line_trigger.as_deref(), Some("enter"));
        assert_eq!(
            config.kanban.new_note_folder.as_deref(),
            Some("Cards/Ideas")
        );
        assert_eq!(
            config.kanban.new_note_template.as_deref(),
            Some("Kanban Card")
        );
        assert!(config.kanban.hide_card_count);
        assert!(config.kanban.hide_tags_in_title);
        assert!(config.kanban.hide_tags_display);
        assert_eq!(
            config.kanban.inline_metadata_position.as_deref(),
            Some("metadata-table")
        );
        assert_eq!(config.kanban.lane_width, Some(320));
        assert_eq!(config.kanban.full_list_lane_width, Some(true));
        assert_eq!(config.kanban.list_collapse, vec![true, false]);
        assert_eq!(config.kanban.max_archive_size, Some(50));
        assert!(config.kanban.show_checkboxes);
        assert_eq!(config.kanban.move_dates, Some(true));
        assert_eq!(config.kanban.move_tags, Some(false));
        assert_eq!(config.kanban.move_task_metadata, Some(true));
        assert_eq!(config.kanban.show_add_list, Some(false));
        assert_eq!(config.kanban.show_archive_all, Some(false));
        assert_eq!(config.kanban.show_board_settings, Some(false));
        assert_eq!(config.kanban.show_relative_date, Some(true));
        assert_eq!(config.kanban.show_search, Some(false));
        assert_eq!(config.kanban.show_set_view, Some(false));
        assert_eq!(config.kanban.show_view_as_markdown, Some(false));
        assert_eq!(config.kanban.date_picker_week_start, Some(1));
        assert_eq!(config.kanban.table_sizing.get("Title"), Some(&240));
        assert_eq!(config.kanban.tag_action.as_deref(), Some("kanban"));
        assert_eq!(
            config.kanban.tag_colors,
            vec![KanbanTagColorConfig {
                tag_key: "#urgent".to_string(),
                color: Some("#ffffff".to_string()),
                background_color: Some("#cc0000".to_string()),
            }]
        );
        assert_eq!(
            config.kanban.tag_sort,
            vec![KanbanTagSortConfig {
                tag: "#urgent".to_string()
            }]
        );
        assert_eq!(
            config.kanban.date_colors,
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

    fn assert_obsidian_seed_periodic_defaults(config: &VaultConfig) {
        assert_eq!(
            config
                .periodic
                .note("daily")
                .map(|note| note.folder.clone()),
            Some(PathBuf::from("Journal/Daily"))
        );
        assert_eq!(
            config
                .periodic
                .note("daily")
                .and_then(|note| note.template.clone()),
            Some("daily".to_string())
        );
        assert_eq!(
            config
                .periodic
                .note("weekly")
                .map(|note| note.start_of_week),
            Some(PeriodicStartOfWeek::Sunday)
        );
        assert_eq!(
            config.periodic.note("monthly").map(|note| note.enabled),
            Some(true)
        );
    }

    fn setup_override_vault(vault_root: &Path) {
        write_test_file(
            &vault_root.join(".obsidian/app.json"),
            VULCAN_OVERRIDE_APP_JSON,
        );
        write_test_file(
            &vault_root.join(".vulcan/config.toml"),
            VULCAN_OVERRIDE_CONFIG_TOML,
        );
    }

    fn assert_override_core_sections(config: &VaultConfig) {
        assert_eq!(config.scan.default_mode, AutoScanMode::Off);
        assert_eq!(config.scan.browse_mode, AutoScanMode::Blocking);
        assert_eq!(config.chunking.strategy, ChunkingStrategy::Fixed);
        assert_eq!(config.chunking.target_size, 512);
        assert_eq!(config.chunking.overlap, 64);
        assert_eq!(config.link_resolution, LinkResolutionMode::Absolute);
        assert_eq!(config.link_style, LinkStylePreference::Wikilink);
        assert_eq!(config.attachment_folder, PathBuf::from("assets"));
        assert_eq!(
            config
                .embedding
                .as_ref()
                .expect("embedding config should be present")
                .model,
            "nomic-embed-text"
        );
        assert_eq!(
            config
                .embedding
                .as_ref()
                .expect("embedding config should be present")
                .provider_name(),
            "openai-compatible"
        );
        assert_eq!(
            config
                .extraction
                .as_ref()
                .expect("extraction config should be present")
                .extensions,
            vec!["pdf".to_string(), "png".to_string()]
        );
        assert!(config.git.auto_commit);
        assert_eq!(config.git.trigger, GitTrigger::Scan);
        assert_eq!(config.git.message, "vault sync: {count}");
        assert_eq!(config.git.scope, GitScope::All);
        assert_eq!(
            config.git.exclude,
            vec![".obsidian/workspace.json".to_string()]
        );
        assert_eq!(config.inbox.path, "Capture/Inbox.md");
        assert_eq!(config.inbox.format, "* {datetime} {text}");
        assert!(!config.inbox.timestamp);
        assert_eq!(config.inbox.heading.as_deref(), Some("## Notes"));
    }

    fn assert_override_tasks_and_kanban(config: &VaultConfig) {
        assert_eq!(config.tasks.global_filter, Some("#work".to_string()));
        assert_eq!(config.tasks.global_query, Some("not done".to_string()));
        assert!(config.tasks.remove_global_filter);
        assert!(config.tasks.set_created_date);
        assert_eq!(
            config.tasks.recurrence_on_completion,
            Some("next-line".to_string())
        );
        assert_eq!(
            config.tasks.statuses.todo,
            vec![" ".to_string(), "!".to_string()]
        );
        assert_eq!(
            config.tasks.statuses.completed,
            vec!["x".to_string(), "v".to_string()]
        );
        assert_eq!(
            config.tasks.statuses.in_progress,
            vec!["/".to_string(), ">".to_string()]
        );
        assert_eq!(config.tasks.statuses.cancelled, vec!["-".to_string()]);
        assert!(config.tasks.statuses.non_task.is_empty());
        assert_eq!(config.kanban.date_trigger, "DUE");
        assert_eq!(config.kanban.time_trigger, "AT");
        assert_eq!(config.kanban.date_format, "DD/MM/YYYY");
        assert_eq!(config.kanban.time_format, "HH:mm:ss");
        assert_eq!(config.kanban.lane_width, Some(300));
        assert_eq!(config.kanban.max_archive_size, Some(42));
        assert_eq!(config.kanban.show_search, Some(false));
        assert_eq!(config.kanban.tag_action.as_deref(), Some("kanban"));
        assert_eq!(
            kanban_metadata_key_names(&config.kanban.metadata_keys),
            vec!["status".to_string(), "owner".to_string()]
        );
    }

    fn assert_override_dataview_and_templates(config: &VaultConfig) {
        assert_eq!(config.dataview.inline_query_prefix, "inline:");
        assert_eq!(config.dataview.inline_js_query_prefix, "$inline:");
        assert!(!config.dataview.enable_dataview_js);
        assert!(config.dataview.enable_inline_dataview_js);
        assert!(config.dataview.task_completion_tracking);
        assert!(config.dataview.task_completion_use_emoji_shorthand);
        assert_eq!(config.dataview.task_completion_text, "done-on");
        assert!(config.dataview.recursive_subtask_completion);
        assert!(!config.dataview.display_result_count);
        assert_eq!(config.dataview.default_date_format, "yyyy-MM-dd");
        assert_eq!(config.dataview.default_datetime_format, "yyyy-MM-dd HH:mm");
        assert_eq!(config.dataview.timezone.as_deref(), Some("+02:00"));
        assert_eq!(config.dataview.max_recursive_render_depth, 8);
        assert_eq!(config.dataview.primary_column_name, "Document");
        assert_eq!(config.dataview.group_column_name, "Bucket");
        assert_eq!(config.templates.date_format, "DD/MM/YYYY");
        assert_eq!(config.templates.time_format, "HH:mm:ss");
    }

    fn setup_templater_precedence_vault(vault_root: &Path) {
        write_test_file(
            &vault_root.join(".obsidian/plugins/templater-obsidian/data.json"),
            TEMPLATER_PRECEDENCE_PLUGIN_JSON,
        );
        write_test_file(
            &vault_root.join(".vulcan/config.toml"),
            SHARED_TEMPLATER_CONFIG_TOML,
        );
        write_test_file(
            &vault_root.join(".vulcan/config.local.toml"),
            LOCAL_TEMPLATER_CONFIG_TOML,
        );
    }

    fn assert_templater_precedence(config: &VaultConfig) {
        assert_eq!(
            config.templates.templater_folder,
            Some(PathBuf::from("Device/Templates"))
        );
        assert_eq!(config.templates.command_timeout, 20);
        assert_eq!(
            config.templates.templates_pairs,
            vec![TemplaterCommandPairConfig {
                name: "slugify".to_string(),
                command: "bun run slugify".to_string(),
            }]
        );
        assert!(config.templates.trigger_on_file_creation);
        assert!(config.templates.auto_jump_to_cursor);
        assert!(config.templates.enable_system_commands);
        assert_eq!(config.templates.shell_path, Some(PathBuf::from("/bin/zsh")));
        assert_eq!(
            config.templates.user_scripts_folder,
            Some(PathBuf::from("Scripts/Device"))
        );
        assert!(!config.templates.enable_folder_templates);
        assert_eq!(
            config.templates.folder_templates,
            vec![TemplaterFolderTemplateConfig {
                folder: PathBuf::from("Projects"),
                template: "Project Template".to_string(),
            }]
        );
        assert!(config.templates.enable_file_templates);
        assert_eq!(
            config.templates.file_templates,
            vec![TemplaterFileTemplateConfig {
                regex: "^Daily/.*\\.md$".to_string(),
                template: "Daily Template".to_string(),
            }]
        );
        assert!(!config.templates.syntax_highlighting);
        assert!(config.templates.syntax_highlighting_mobile);
        assert_eq!(
            config.templates.enabled_templates_hotkeys,
            vec!["Device Daily".to_string()]
        );
        assert_eq!(
            config.templates.startup_templates,
            vec!["Device Startup".to_string()]
        );
        assert_eq!(config.templates.intellisense_render, 2);
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
    fn vulcan_config_parses_custom_period_types() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        write_test_file(
            &vault_root.join(".vulcan/config.toml"),
            r#"
[periodic.sprint]
enabled = true
folder = "Journal/Sprints"
format = "YYYY-[Sprint]-MM-DD"
unit = "weeks"
interval = 2
anchor_date = "2026-01-05"
template = "Sprint"
"#,
        );

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        let sprint = loaded
            .config
            .periodic
            .note("sprint")
            .expect("custom period should be loaded");
        assert!(sprint.enabled);
        assert_eq!(sprint.folder, PathBuf::from("Journal/Sprints"));
        assert_eq!(sprint.format, "YYYY-[Sprint]-MM-DD");
        assert_eq!(sprint.unit, Some(PeriodicCadenceUnit::Weeks));
        assert_eq!(sprint.interval, 2);
        assert_eq!(sprint.anchor_date.as_deref(), Some("2026-01-05"));
        assert_eq!(sprint.template.as_deref(), Some("Sprint"));
    }

    #[test]
    fn obsidian_settings_seed_defaults_and_property_types() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        setup_obsidian_seed_vault(vault_root);

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        assert_obsidian_seed_core_defaults(&loaded.config);
        assert_obsidian_seed_dataview_defaults(&loaded.config);
        assert_obsidian_seed_quickadd_defaults(&loaded.config);
        assert_obsidian_seed_kanban_defaults(&loaded.config);
        assert_obsidian_seed_periodic_defaults(&loaded.config);
    }

    #[test]
    fn templater_plugin_settings_seed_defaults() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        write_test_file(
            &vault_root.join(".obsidian/plugins/templater-obsidian/data.json"),
            TEMPLATER_PLUGIN_DEFAULTS_JSON,
        );

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
    fn quickadd_settings_follow_vulcan_partial_override_precedence() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        write_test_file(
            &vault_root.join(".obsidian/plugins/quickadd/data.json"),
            OBSIDIAN_QUICKADD_JSON,
        );
        write_test_file(
            &vault_root.join(".vulcan/config.toml"),
            QUICKADD_OVERRIDE_CONFIG_TOML,
        );

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(
            loaded.config.quickadd.template_folder,
            Some(PathBuf::from("QuickAdd/Overrides"))
        );
        assert_eq!(
            loaded.config.quickadd.global_variables.get("Project"),
            Some(&"[[Projects/Beta]]".to_string())
        );
        assert_eq!(loaded.config.quickadd.capture_choices.len(), 1);
        assert_eq!(loaded.config.quickadd.template_choices.len(), 1);
        let ai = loaded
            .config
            .quickadd
            .ai
            .as_ref()
            .expect("quickadd ai config should be present");
        assert!(!ai.show_assistant);
        assert_eq!(ai.providers.len(), 1);
        assert_eq!(ai.default_model.as_deref(), Some("gpt-4o-mini"));
    }

    #[test]
    fn vulcan_config_overrides_obsidian_values() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        setup_override_vault(vault_root);

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        assert_override_core_sections(&loaded.config);
        assert_override_tasks_and_kanban(&loaded.config);
        assert_override_dataview_and_templates(&loaded.config);
    }

    #[test]
    fn templater_settings_follow_vulcan_and_local_precedence() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        setup_templater_precedence_vault(vault_root);

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        assert_templater_precedence(&loaded.config);
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
            r#"[scan]
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
"#,
        )
        .expect("shared config should be written");
        fs::write(
            vault_root.join(".vulcan/config.local.toml"),
            r#"[scan]
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
"#,
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
            r###"{
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
            }"###,
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
    fn tasknotes_plugin_settings_seed_tasknotes_config() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/tasknotes"))
            .expect("tasknotes plugin dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/tasknotes/data.json"),
            r##"{
              "tasksFolder": "Tasks",
              "archiveFolder": "Archive",
              "taskTag": "todo",
              "taskIdentificationMethod": "property",
              "taskPropertyName": "isTask",
              "taskPropertyValue": "yes",
              "excludedFolders": "Archive, Someday",
              "defaultTaskStatus": "in-progress",
              "defaultTaskPriority": "high",
              "fieldMapping": {
                "due": "deadline",
                "timeEstimate": "estimateMinutes",
                "archiveTag": "archived-task"
              },
              "customStatuses": [
                {
                  "id": "blocked",
                  "value": "blocked",
                  "label": "Blocked",
                  "color": "#ff8800",
                  "isCompleted": false,
                  "order": 4,
                  "autoArchive": false,
                  "autoArchiveDelay": 15
                }
              ],
              "customPriorities": [
                {
                  "id": "urgent",
                  "value": "urgent",
                  "label": "Urgent",
                  "color": "#ff0000",
                  "weight": 9
                }
              ],
              "userFields": [
                {
                  "id": "effort",
                  "displayName": "Effort",
                  "key": "effort",
                  "type": "number"
                }
              ],
              "enableNaturalLanguageInput": false,
              "nlpDefaultToScheduled": true,
              "nlpLanguage": "de",
              "nlpTriggers": {
                "triggers": [
                  { "propertyId": "contexts", "trigger": "context:", "enabled": true },
                  { "propertyId": "tags", "trigger": "#", "enabled": true }
                ]
              },
              "taskCreationDefaults": {
                "defaultContexts": "@office, @home",
                "defaultTags": "work, urgent",
                "defaultProjects": "[[Projects/Alpha]], [[Projects/Beta]]",
                "defaultTimeEstimate": 45,
                "defaultDueDate": "tomorrow",
                "defaultScheduledDate": "today",
                "defaultRecurrence": "weekly",
                "defaultReminders": [{ "id": "rem-1", "type": "relative" }]
              },
              "calendarViewSettings": { "defaultView": "month" },
              "pomodoroWorkDuration": 25,
              "enableTaskLinkOverlay": true,
              "uiLanguage": "de",
              "icsIntegration": { "enabled": true },
              "savedViews": [{ "id": "today", "name": "Today" }],
              "enableAPI": true,
              "webhooks": [{ "url": "https://example.test/hook" }],
              "enableBases": true,
              "commandFileMapping": { "open-tasks-view": "TaskNotes/Views/tasks.base" },
              "enableGoogleCalendar": true,
              "googleOAuthClientId": "google-client",
              "enableMicrosoftCalendar": true,
              "microsoftOAuthClientId": "microsoft-client"
            }"##,
        )
        .expect("tasknotes config should be written");

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(loaded.config.tasknotes.tasks_folder, "Tasks");
        assert_eq!(loaded.config.tasknotes.archive_folder, "Archive");
        assert_eq!(loaded.config.tasknotes.task_tag, "todo");
        assert_eq!(
            loaded.config.tasknotes.identification_method,
            TaskNotesIdentificationMethod::Property
        );
        assert_eq!(
            loaded.config.tasknotes.task_property_name.as_deref(),
            Some("isTask")
        );
        assert_eq!(
            loaded.config.tasknotes.task_property_value.as_deref(),
            Some("yes")
        );
        assert_eq!(
            loaded.config.tasknotes.excluded_folders,
            vec!["Archive".to_string(), "Someday".to_string()]
        );
        assert_eq!(loaded.config.tasknotes.default_status, "in-progress");
        assert_eq!(loaded.config.tasknotes.default_priority, "high");
        assert_eq!(loaded.config.tasknotes.field_mapping.due, "deadline");
        assert_eq!(
            loaded.config.tasknotes.field_mapping.time_estimate,
            "estimateMinutes"
        );
        assert_eq!(
            loaded.config.tasknotes.field_mapping.archive_tag,
            "archived-task"
        );
        assert_eq!(loaded.config.tasknotes.statuses.len(), 1);
        assert_eq!(loaded.config.tasknotes.statuses[0].value, "blocked");
        assert_eq!(loaded.config.tasknotes.priorities.len(), 1);
        assert_eq!(loaded.config.tasknotes.priorities[0].value, "urgent");
        assert_eq!(loaded.config.tasknotes.user_fields.len(), 1);
        assert_eq!(loaded.config.tasknotes.user_fields[0].key, "effort");
        assert!(!loaded.config.tasknotes.enable_natural_language_input);
        assert!(loaded.config.tasknotes.nlp_default_to_scheduled);
        assert_eq!(loaded.config.tasknotes.nlp_language, "de");
        assert_eq!(loaded.config.tasknotes.nlp_triggers.len(), 2);
        assert_eq!(
            loaded.config.tasknotes.nlp_triggers[0].property_id,
            "contexts"
        );
        assert_eq!(loaded.config.tasknotes.nlp_triggers[0].trigger, "context:");
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_contexts,
            vec!["@office".to_string(), "@home".to_string()]
        );
        assert_eq!(
            loaded.config.tasknotes.task_creation_defaults.default_tags,
            vec!["work".to_string(), "urgent".to_string()]
        );
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_projects,
            vec![
                "[[Projects/Alpha]]".to_string(),
                "[[Projects/Beta]]".to_string()
            ]
        );
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_time_estimate,
            Some(45)
        );
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_due_date,
            TaskNotesDateDefault::Tomorrow
        );
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_scheduled_date,
            TaskNotesDateDefault::Today
        );
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_recurrence,
            TaskNotesRecurrenceDefault::Weekly
        );
    }

    #[test]
    fn vulcan_overrides_replace_tasknotes_settings() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r##"[tasknotes]
tasks_folder = "Work/Tasks"
archive_folder = "Work/Archive"
task_tag = "work-task"
identification_method = "property"
task_property_name = "kind"
task_property_value = "task"
excluded_folders = ["Work/Archive"]
default_status = "blocked"
default_priority = "urgent"
enable_natural_language_input = false
nlp_default_to_scheduled = true
nlp_language = "fr"

[tasknotes.field_mapping]
due = "deadline"
time_entries = "tracked"

[[tasknotes.statuses]]
id = "blocked"
value = "blocked"
label = "Blocked"
color = "#ff8800"
isCompleted = false
order = 1
autoArchive = false
autoArchiveDelay = 30

[[tasknotes.priorities]]
id = "urgent"
value = "urgent"
label = "Urgent"
color = "#ff0000"
weight = 9

[[tasknotes.user_fields]]
id = "effort"
displayName = "Effort"
key = "effort"
type = "number"

[[tasknotes.nlp_triggers]]
property_id = "contexts"
trigger = "context:"
enabled = true

[tasknotes.task_creation_defaults]
default_contexts = ["@office"]
default_tags = ["work"]
default_projects = ["[[Projects/Alpha]]"]
default_time_estimate = 30
default_due_date = "today"
default_scheduled_date = "next-week"
default_recurrence = "monthly"
"##,
        )
        .expect("config should be written");

        let loaded = load_vault_config(&VaultPaths::new(vault_root));

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(loaded.config.tasknotes.tasks_folder, "Work/Tasks");
        assert_eq!(loaded.config.tasknotes.archive_folder, "Work/Archive");
        assert_eq!(loaded.config.tasknotes.task_tag, "work-task");
        assert_eq!(
            loaded.config.tasknotes.identification_method,
            TaskNotesIdentificationMethod::Property
        );
        assert_eq!(
            loaded.config.tasknotes.task_property_name.as_deref(),
            Some("kind")
        );
        assert_eq!(
            loaded.config.tasknotes.task_property_value.as_deref(),
            Some("task")
        );
        assert_eq!(
            loaded.config.tasknotes.excluded_folders,
            vec!["Work/Archive".to_string()]
        );
        assert_eq!(loaded.config.tasknotes.default_status, "blocked");
        assert_eq!(loaded.config.tasknotes.default_priority, "urgent");
        assert_eq!(loaded.config.tasknotes.field_mapping.due, "deadline");
        assert_eq!(
            loaded.config.tasknotes.field_mapping.time_entries,
            "tracked"
        );
        assert_eq!(loaded.config.tasknotes.statuses[0].auto_archive_delay, 30);
        assert_eq!(loaded.config.tasknotes.priorities[0].weight, 9);
        assert_eq!(loaded.config.tasknotes.user_fields[0].key, "effort");
        assert!(!loaded.config.tasknotes.enable_natural_language_input);
        assert!(loaded.config.tasknotes.nlp_default_to_scheduled);
        assert_eq!(loaded.config.tasknotes.nlp_language, "fr");
        assert_eq!(loaded.config.tasknotes.nlp_triggers.len(), 1);
        assert_eq!(loaded.config.tasknotes.nlp_triggers[0].trigger, "context:");
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_contexts,
            vec!["@office".to_string()]
        );
        assert_eq!(
            loaded.config.tasknotes.task_creation_defaults.default_tags,
            vec!["work".to_string()]
        );
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_projects,
            vec!["[[Projects/Alpha]]".to_string()]
        );
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_time_estimate,
            Some(30)
        );
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_due_date,
            TaskNotesDateDefault::Today
        );
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_scheduled_date,
            TaskNotesDateDefault::NextWeek
        );
        assert_eq!(
            loaded
                .config
                .tasknotes
                .task_creation_defaults
                .default_recurrence,
            TaskNotesRecurrenceDefault::Monthly
        );
    }

    #[test]
    fn vulcan_task_status_definitions_support_names_and_next_symbols() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[tasks.statuses]
todo = [" "]
completed = ["x"]

[[tasks.statuses.definitions]]
symbol = "!"
name = "Important"
type = "TODO"
next_symbol = "x"
"#,
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
            r#"[scan]
default_mode = "off"
"#,
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
    fn import_quickadd_plugin_config_preserves_existing_sections_and_is_idempotent() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/quickadd"))
            .expect("quickadd plugin dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/quickadd/data.json"),
            r###"{
              "templateFolderPath": "QuickAdd/Templates",
              "globalVariables": {
                "Project": "[[Projects/Alpha]]",
                "agenda": "- {{VALUE:title}} due {{VDATE:due,YYYY-MM-DD}}"
              },
              "choices": [
                {
                  "id": "capture-daily",
                  "name": "Daily Capture",
                  "type": "Capture",
                  "captureTo": "Journal/Daily/{{DATE:YYYY-MM-DD}}",
                  "captureToActiveFile": false,
                  "createFileIfItDoesntExist": {
                    "enabled": true,
                    "createWithTemplate": true,
                    "template": "Daily Template"
                  },
                  "format": {
                    "enabled": true,
                    "format": "- {{VALUE:title|case:slug}}"
                  },
                  "prepend": true,
                  "task": true,
                  "insertAfter": {
                    "enabled": true,
                    "after": "## Log",
                    "insertAtEnd": true,
                    "considerSubsections": true,
                    "createIfNotFound": true,
                    "createIfNotFoundLocation": "bottom"
                  },
                  "templater": {
                    "afterCapture": "wholeFile"
                  }
                },
                {
                  "id": "template-note",
                  "name": "Template Note",
                  "type": "Template",
                  "templatePath": "Templates/Project Template.md",
                  "folder": {
                    "enabled": true,
                    "folders": ["Projects", "Areas/Research"],
                    "chooseWhenCreatingNote": true,
                    "chooseFromSubfolders": true
                  },
                  "fileNameFormat": {
                    "enabled": true,
                    "format": "{{VALUE:title|case:slug}}"
                  },
                  "openFile": true,
                  "fileExistsBehavior": "increment"
                },
                {
                  "id": "macro-choice",
                  "name": "Macro Choice",
                  "type": "Macro"
                },
                {
                  "id": "multi-choice",
                  "name": "Multi Choice",
                  "type": "Multi"
                }
              ],
              "ai": {
                "defaultModel": "gpt-4o-mini",
                "defaultSystemPrompt": "Summarize briefly.",
                "promptTemplatesFolderPath": "QuickAdd/Prompts",
                "showAssistant": true,
                "providers": [
                  {
                    "name": "OpenAI",
                    "endpoint": "https://api.openai.com/v1",
                    "apiKey": "secret-token",
                    "modelSource": "providerApi",
                    "models": [
                      { "name": "gpt-4o-mini", "maxTokens": 128000 }
                    ]
                  }
                ]
              }
            }"###,
        )
        .expect("quickadd config should be written");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("existing config should be written");
        let paths = VaultPaths::new(vault_root);

        let report = import_quickadd_plugin_config(&paths).expect("import should succeed");

        assert_eq!(report.plugin, "quickadd");
        assert!(!report.created_config);
        assert!(report.updated);
        assert!(report
            .mappings
            .iter()
            .any(|mapping| mapping.target == "quickadd.template_folder"
                && mapping.value == Value::String("QuickAdd/Templates".to_string())));
        assert!(report.skipped.iter().any(|item| {
            item.source == "choices[2] (Macro Choice)"
                && item.reason.contains("`vulcan run --script`")
        }));
        assert!(report.skipped.iter().any(|item| {
            item.source == "choices[3] (Multi Choice)" && item.reason.contains("orchestration flow")
        }));
        assert!(report.skipped.iter().any(|item| {
            item.source == "ai.providers[0].apiKey" && item.reason.contains("OPENAI_API_KEY")
        }));

        let rendered = fs::read_to_string(paths.config_file()).expect("config should exist");
        assert!(rendered.contains("[git]"));
        assert!(rendered.contains("auto_commit = true"));
        assert!(rendered.contains("[quickadd]"));
        assert!(rendered.contains("template_folder = \"QuickAdd/Templates\""));
        assert!(rendered.contains("[quickadd.global_variables]"));
        assert!(rendered.contains("Project = \"[[Projects/Alpha]]\""));
        assert!(rendered.contains("[[quickadd.capture_choices]]"));
        assert!(rendered.contains("id = \"capture-daily\""));
        assert!(rendered.contains("capture_to = \"Journal/Daily/{{DATE:YYYY-MM-DD}}\""));
        assert!(rendered.contains("format = \"- {{VALUE:title|case:slug}}\""));
        assert!(rendered.contains("[quickadd.capture_choices.insert_after]"));
        assert!(rendered.contains("heading = \"## Log\""));
        assert!(rendered.contains("[[quickadd.template_choices]]"));
        assert!(rendered.contains("template_path = \"Templates/Project Template.md\""));
        assert!(rendered.contains("[quickadd.ai]"));
        assert!(rendered.contains("default_model = \"gpt-4o-mini\""));
        assert!(rendered.contains("[[quickadd.ai.providers]]"));
        assert!(rendered.contains("api_key_env = \"OPENAI_API_KEY\""));

        let second_report =
            import_quickadd_plugin_config(&paths).expect("second import should succeed");
        assert!(!second_report.updated);
    }

    #[test]
    fn import_quickadd_plugin_config_errors_when_source_is_missing() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        let error = import_quickadd_plugin_config(&paths).expect_err("import should fail");
        assert!(matches!(error, ConfigImportError::MissingSource(_)));
    }

    #[test]
    fn import_dataview_plugin_config_preserves_existing_sections_and_is_idempotent() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
            .expect("dataview plugin dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/dataview/data.json"),
            OBSIDIAN_DATAVIEW_JSON,
        )
        .expect("dataview config should be written");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("existing config should be written");
        let paths = VaultPaths::new(vault_root);

        let report = import_dataview_plugin_config(&paths).expect("import should succeed");

        assert_eq!(report.plugin, "dataview");
        assert!(!report.created_config);
        assert!(report.updated);
        assert!(report
            .mappings
            .iter()
            .any(|mapping| mapping.target == "dataview.inline_query_prefix"
                && mapping.value == Value::String("dv:".to_string())));

        let rendered = fs::read_to_string(paths.config_file()).expect("config should exist");
        assert!(rendered.contains("[git]"));
        assert!(rendered.contains("auto_commit = true"));
        assert!(rendered.contains("[dataview]"));
        assert!(rendered.contains("inline_query_prefix = \"dv:\""));
        assert!(rendered.contains("inline_js_query_prefix = \"$dv:\""));
        assert!(rendered.contains("enable_dataview_js = false"));
        assert!(rendered.contains("enable_inline_dataview_js = true"));
        assert!(rendered.contains("task_completion_tracking = true"));
        assert!(rendered.contains("task_completion_use_emoji_shorthand = true"));
        assert!(rendered.contains("task_completion_text = \"done-on\""));
        assert!(rendered.contains("recursive_subtask_completion = true"));
        assert!(rendered.contains("display_result_count = false"));
        assert!(rendered.contains("default_date_format = \"yyyy-MM-dd\""));
        assert!(rendered.contains("default_datetime_format = \"yyyy-MM-dd HH:mm\""));
        assert!(rendered.contains("timezone = \"+02:00\""));
        assert!(rendered.contains("max_recursive_render_depth = 7"));
        assert!(rendered.contains("primary_column_name = \"Document\""));
        assert!(rendered.contains("group_column_name = \"Bucket\""));

        let second_report =
            import_dataview_plugin_config(&paths).expect("second import should succeed");
        assert!(!second_report.updated);
    }

    #[test]
    fn import_dataview_plugin_config_errors_when_source_is_missing() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        let error = import_dataview_plugin_config(&paths).expect_err("import should fail");
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

    #[test]
    fn import_periodic_notes_plugin_config_preserves_existing_sections_and_is_idempotent() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/periodic-notes"))
            .expect("periodic plugin dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join(".obsidian/daily-notes.json"),
            OBSIDIAN_DAILY_NOTES_JSON,
        )
        .expect("daily notes config should be written");
        fs::write(
            vault_root.join(".obsidian/plugins/periodic-notes/data.json"),
            OBSIDIAN_PERIODIC_NOTES_JSON,
        )
        .expect("periodic plugin config should be written");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("existing config should be written");
        let paths = VaultPaths::new(vault_root);

        let report = import_periodic_notes_plugin_config(&paths).expect("import should succeed");

        assert_eq!(report.plugin, "periodic-notes");
        assert_eq!(report.source_paths.len(), 2);
        assert!(!report.created_config);
        assert!(report.updated);
        assert!(report.mappings.iter().any(|mapping| {
            mapping.target == "periodic.weekly.start_of_week"
                && mapping.value == Value::String("sunday".to_string())
        }));

        let rendered = fs::read_to_string(paths.config_file()).expect("config should exist");
        assert!(rendered.contains("[git]"));
        assert!(rendered.contains("auto_commit = true"));
        assert!(rendered.contains("[periodic.daily]"));
        assert!(rendered.contains("folder = \"Journal/Daily\""));
        assert!(rendered.contains("template = \"daily\""));
        assert!(rendered.contains("[periodic.weekly]"));
        assert!(rendered.contains("start_of_week = \"sunday\""));

        let second_report =
            import_periodic_notes_plugin_config(&paths).expect("second import should succeed");
        assert!(!second_report.updated);
    }

    #[test]
    fn import_periodic_notes_plugin_config_errors_when_sources_are_missing() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        let error = import_periodic_notes_plugin_config(&paths).expect_err("import should fail");
        assert!(matches!(error, ConfigImportError::MissingSource(_)));
    }

    #[test]
    fn import_tasknotes_plugin_config_preserves_existing_sections_and_is_idempotent() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/tasknotes"))
            .expect("tasknotes plugin dir should be created");
        fs::create_dir_all(vault_root.join("Views Source"))
            .expect("tasknotes view source dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join("Views Source/tasks-custom.base"),
            concat!(
                "# All Tasks\n\n",
                "views:\n",
                "  - type: tasknotesTaskList\n",
                "    name: \"All Tasks\"\n",
                "    order:\n",
                "      - note.status\n",
                "      - note.priority\n",
                "      - note.due\n",
            ),
        )
        .expect("task list base should be written");
        fs::write(
            vault_root.join("Views Source/kanban-custom.base"),
            concat!(
                "# Kanban\n\n",
                "views:\n",
                "  - type: tasknotesKanban\n",
                "    name: \"Kanban\"\n",
                "    order:\n",
                "      - note.status\n",
                "      - note.priority\n",
                "    groupBy:\n",
                "      property: note.status\n",
                "      direction: ASC\n",
            ),
        )
        .expect("kanban base should be written");
        fs::write(
            vault_root.join("Views Source/relationships-custom.base"),
            concat!(
                "# Relationships\n\n",
                "views:\n",
                "  - type: tasknotesTaskList\n",
                "    name: \"Projects\"\n",
                "    filters:\n",
                "      and:\n",
                "        - list(this.projects).contains(file.asLink())\n",
                "    order:\n",
                "      - note.projects\n",
            ),
        )
        .expect("relationships base should be written");
        fs::write(
            vault_root.join("Views Source/agenda-custom.base"),
            concat!(
                "# Agenda\n\n",
                "views:\n",
                "  - type: tasknotesCalendar\n",
                "    name: \"Agenda\"\n",
            ),
        )
        .expect("agenda base should be written");
        fs::write(
            vault_root.join(".obsidian/plugins/tasknotes/data.json"),
            r##"{
              "tasksFolder": "Tasks",
              "archiveFolder": "Archive",
              "taskTag": "todo",
              "taskIdentificationMethod": "property",
              "taskPropertyName": "isTask",
              "taskPropertyValue": "yes",
              "excludedFolders": "Archive, Someday",
              "defaultTaskStatus": "in-progress",
              "defaultTaskPriority": "high",
              "fieldMapping": {
                "due": "deadline",
                "timeEstimate": "estimateMinutes",
                "archiveTag": "archived-task"
              },
              "customStatuses": [
                {
                  "id": "blocked",
                  "value": "blocked",
                  "label": "Blocked",
                  "color": "#ff8800",
                  "isCompleted": false,
                  "order": 4,
                  "autoArchive": false,
                  "autoArchiveDelay": 15
                }
              ],
              "customPriorities": [
                {
                  "id": "urgent",
                  "value": "urgent",
                  "label": "Urgent",
                  "color": "#ff0000",
                  "weight": 9
                }
              ],
              "userFields": [
                {
                  "id": "effort",
                  "displayName": "Effort",
                  "key": "effort",
                  "type": "number"
                }
              ],
              "enableNaturalLanguageInput": false,
              "nlpDefaultToScheduled": true,
              "nlpLanguage": "de",
              "nlpTriggers": {
                "triggers": [
                  { "propertyId": "contexts", "trigger": "context:", "enabled": true },
                  { "propertyId": "tags", "trigger": "#", "enabled": true }
                ]
              },
              "taskCreationDefaults": {
                "defaultContexts": "@office, @home",
                "defaultTags": "work, urgent",
                "defaultProjects": "[[Projects/Alpha]], [[Projects/Beta]]",
                "defaultTimeEstimate": 45,
                "defaultDueDate": "tomorrow",
                "defaultScheduledDate": "today",
                "defaultRecurrence": "weekly"
              },
              "commandFileMapping": {
                "open-tasks-view": "Views Source/tasks-custom.base",
                "open-kanban-view": "Views Source/kanban-custom.base",
                "relationships": "Views Source/relationships-custom.base",
                "open-agenda-view": "Views Source/agenda-custom.base"
              }
            }"##,
        )
        .expect("tasknotes config should be written");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("existing config should be written");
        let paths = VaultPaths::new(vault_root);

        let report = import_tasknotes_plugin_config(&paths).expect("import should succeed");

        assert_eq!(report.plugin, "tasknotes");
        assert!(!report.created_config);
        assert!(report.updated);
        assert!(report
            .mappings
            .iter()
            .any(|mapping| mapping.target == "tasknotes.tasks_folder"
                && mapping.value == Value::String("Tasks".to_string())));
        assert!(report
            .mappings
            .iter()
            .any(|mapping| mapping.target == "tasknotes.field_mapping.due"
                && mapping.value == Value::String("deadline".to_string())));
        assert_eq!(report.migrated_files.len(), 3);
        assert!(report.migrated_files.iter().any(|file| {
            file.target == vault_root.join("TaskNotes/Views/tasks-default.base")
                && matches!(file.action, ImportMigratedFileAction::Copy)
        }));
        assert!(report.migrated_files.iter().any(|file| {
            file.target == vault_root.join("TaskNotes/Views/kanban-default.base")
                && matches!(file.action, ImportMigratedFileAction::Copy)
        }));
        assert!(report.migrated_files.iter().any(|file| {
            file.target == vault_root.join("TaskNotes/Views/relationships.base")
                && matches!(file.action, ImportMigratedFileAction::Copy)
        }));
        assert!(report.skipped.iter().any(|item| {
            item.source == "commandFileMapping.open-agenda-view"
                && item
                    .reason
                    .contains("unsupported view types: tasknotesCalendar")
        }));

        let rendered = fs::read_to_string(paths.config_file()).expect("config should exist");
        assert!(rendered.contains("[git]"));
        assert!(rendered.contains("auto_commit = true"));
        assert!(rendered.contains("[tasknotes]"));
        assert!(rendered.contains("tasks_folder = \"Tasks\""));
        assert!(rendered.contains("archive_folder = \"Archive\""));
        assert!(rendered.contains("task_tag = \"todo\""));
        assert!(rendered.contains("identification_method = \"property\""));
        assert!(rendered.contains("task_property_name = \"isTask\""));
        assert!(rendered.contains("task_property_value = \"yes\""));
        assert!(rendered.contains("excluded_folders"));
        assert!(rendered.contains("\"Archive\""));
        assert!(rendered.contains("\"Someday\""));
        assert!(rendered.contains("default_status = \"in-progress\""));
        assert!(rendered.contains("default_priority = \"high\""));
        assert!(rendered.contains("[tasknotes.field_mapping]"));
        assert!(rendered.contains("due = \"deadline\""));
        assert!(rendered.contains("time_estimate = \"estimateMinutes\""));
        assert!(rendered.contains("archive_tag = \"archived-task\""));
        assert!(rendered.contains("[[tasknotes.statuses]]"));
        assert!(rendered.contains("value = \"blocked\""));
        assert!(rendered.contains("[[tasknotes.priorities]]"));
        assert!(rendered.contains("value = \"urgent\""));
        assert!(rendered.contains("[[tasknotes.user_fields]]"));
        assert!(rendered.contains("displayName = \"Effort\""));
        assert!(rendered.contains("enable_natural_language_input = false"));
        assert!(rendered.contains("nlp_default_to_scheduled = true"));
        assert!(rendered.contains("nlp_language = \"de\""));
        assert!(rendered.contains("[[tasknotes.nlp_triggers]]"));
        assert!(rendered.contains("property_id = \"contexts\""));
        assert!(rendered.contains("[tasknotes.task_creation_defaults]"));
        assert!(rendered.contains("default_contexts"));
        assert!(rendered.contains("\"@office\""));
        assert!(rendered.contains("\"@home\""));
        assert!(rendered.contains("default_due_date = \"tomorrow\""));
        assert!(rendered.contains("default_recurrence = \"weekly\""));
        let migrated_tasks =
            fs::read_to_string(vault_root.join("TaskNotes/Views/tasks-default.base"))
                .expect("migrated tasks base should exist");
        assert!(migrated_tasks.starts_with("source: tasknotes\n\n# All Tasks\n"));
        let migrated_tasks_info = inspect_base_file(&paths, "TaskNotes/Views/tasks-default.base")
            .expect("migrated tasks base should parse");
        assert_eq!(migrated_tasks_info.source_type, "tasknotes");
        assert_eq!(migrated_tasks_info.views.len(), 1);
        assert_eq!(migrated_tasks_info.views[0].view_type, "tasknotesTaskList");

        let second_report =
            import_tasknotes_plugin_config(&paths).expect("second import should succeed");
        assert!(!second_report.updated);
        assert!(second_report
            .migrated_files
            .iter()
            .all(|file| { matches!(file.action, ImportMigratedFileAction::ValidateOnly) }));
    }

    #[test]
    fn import_tasknotes_plugin_config_errors_when_source_is_missing() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        let error = import_tasknotes_plugin_config(&paths).expect_err("import should fail");
        assert!(matches!(error, ConfigImportError::MissingSource(_)));
    }

    #[test]
    fn tasknotes_skipped_settings_report_unsupported_categories() {
        let raw = serde_json::json!({
            "calendarViewSettings": { "defaultView": "month" },
            "pomodoroWorkDuration": 25,
            "enableTaskLinkOverlay": true,
            "uiLanguage": "de",
            "icsIntegration": { "enabled": true },
            "savedViews": [{ "id": "today", "name": "Today" }],
            "enableAPI": true,
            "webhooks": [{ "url": "https://example.test/hook" }],
            "enableBases": true,
            "commandFileMapping": { "open-tasks-view": "TaskNotes/Views/tasks.base" },
            "enableGoogleCalendar": true,
            "googleOAuthClientId": "google-client",
            "enableMicrosoftCalendar": true,
            "microsoftOAuthClientId": "microsoft-client",
            "taskCreationDefaults": {
                "defaultReminders": [{ "id": "rem-1", "type": "relative" }]
            }
        });

        let skipped = tasknotes_skipped_settings(&raw);

        assert!(skipped.iter().any(|item| {
            item.source == "calendarViewSettings"
                && item.reason == "calendar view settings are not yet supported"
        }));
        assert!(skipped.iter().any(|item| {
            item.source == "taskCreationDefaults.defaultReminders"
                && item.reason == "default reminder settings are not yet supported"
        }));
        assert!(skipped.iter().any(|item| {
            item.reason == "Google Calendar integration settings are not yet supported"
        }));
        assert!(skipped.iter().any(|item| {
            item.reason == "Microsoft Calendar integration settings are not yet supported"
        }));
        assert!(skipped
            .iter()
            .any(|item| { item.reason == "API and webhook settings are not yet supported" }));
        assert!(skipped.iter().any(|item| {
            item.reason == "TaskNotes Bases integration settings are not yet supported"
        }));
    }

    #[test]
    fn tasknotes_view_migration_skips_conflicting_existing_target_files() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join("Views Source"))
            .expect("view source dir should be created");
        fs::create_dir_all(vault_root.join("TaskNotes/Views"))
            .expect("tasknotes views dir should be created");
        fs::write(
            vault_root.join("Views Source/tasks-custom.base"),
            "views:\n  - type: tasknotesTaskList\n    name: Tasks\n",
        )
        .expect("source base should be written");
        fs::write(
            vault_root.join("TaskNotes/Views/tasks-default.base"),
            "source: tasknotes\n\nviews:\n  - type: tasknotesTaskList\n    name: Existing\n",
        )
        .expect("existing target base should be written");

        let raw = serde_json::json!({
            "commandFileMapping": {
                "open-tasks-view": "Views Source/tasks-custom.base"
            }
        });

        let result = tasknotes_migrate_view_files(&VaultPaths::new(vault_root), &raw, false)
            .expect("view migration should succeed");

        assert!(result.migrated_files.is_empty());
        assert!(result.skipped.iter().any(|item| {
            item.source == "commandFileMapping.open-tasks-view"
                && item
                    .reason
                    .contains("already exists with different contents")
        }));
    }

    #[test]
    fn importer_registry_dispatches_existing_importers_in_priority_order() {
        let importer_names = all_importers()
            .into_iter()
            .map(|importer| importer.name().to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            importer_names,
            [
                "core",
                "dataview",
                "kanban",
                "periodic-notes",
                "quickadd",
                "tasknotes",
                "tasks",
                "templater"
            ]
        );
    }

    #[test]
    fn importer_dry_run_reports_changes_without_writing_files() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian/plugins/obsidian-tasks-plugin"))
            .expect("tasks plugin dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/obsidian-tasks-plugin/data.json"),
            r##"{
              "globalFilter": "#task",
              "globalQuery": "not done"
            }"##,
        )
        .expect("tasks config should be written");
        let paths = VaultPaths::new(vault_root);

        let report = TasksImporter
            .dry_run(&paths)
            .expect("dry run import should succeed");

        assert_eq!(report.plugin, "tasks");
        assert_eq!(report.target_file, paths.config_file());
        assert!(report.created_config);
        assert!(report.updated);
        assert!(report.dry_run);
        assert!(!paths.config_file().exists());
        assert!(!paths.gitignore_file().exists());
    }

    #[test]
    fn import_conflicts_are_reported_when_multiple_importers_touch_one_key() {
        let mut reports = vec![
            ConfigImportReport {
                plugin: "core".to_string(),
                source_path: PathBuf::from(".obsidian"),
                source_paths: vec![PathBuf::from(".obsidian/app.json")],
                config_path: PathBuf::from(".vulcan/config.toml"),
                target_file: PathBuf::from(".vulcan/config.toml"),
                created_config: false,
                updated: true,
                config_updated: true,
                dry_run: false,
                mappings: vec![ConfigImportMapping {
                    source: "app.json.useMarkdownLinks".to_string(),
                    target: "links.style".to_string(),
                    value: Value::String("wikilink".to_string()),
                }],
                migrated_files: Vec::new(),
                skipped: Vec::new(),
                conflicts: Vec::new(),
            },
            ConfigImportReport {
                plugin: "templater".to_string(),
                source_path: PathBuf::from(".obsidian/plugins/templater-obsidian/data.json"),
                source_paths: vec![PathBuf::from(
                    ".obsidian/plugins/templater-obsidian/data.json",
                )],
                config_path: PathBuf::from(".vulcan/config.toml"),
                target_file: PathBuf::from(".vulcan/config.toml"),
                created_config: false,
                updated: true,
                config_updated: true,
                dry_run: false,
                mappings: vec![ConfigImportMapping {
                    source: "templates_folder".to_string(),
                    target: "links.style".to_string(),
                    value: Value::String("markdown".to_string()),
                }],
                migrated_files: Vec::new(),
                skipped: Vec::new(),
                conflicts: Vec::new(),
            },
        ];

        annotate_import_conflicts(&mut reports);

        assert!(reports[0].conflicts.is_empty());
        assert_eq!(reports[1].conflicts.len(), 1);
        assert_eq!(reports[1].conflicts[0].key, "links.style");
        assert_eq!(reports[1].conflicts[0].sources, ["core", "templater"]);
        assert_eq!(
            reports[1].conflicts[0].kept_value,
            Value::String("markdown".to_string())
        );
    }

    #[test]
    fn import_core_plugin_config_writes_settings_from_all_supported_sources() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian")).expect("obsidian dir should be created");
        fs::write(
            vault_root.join(".obsidian/app.json"),
            r#"{
              "useMarkdownLinks": true,
              "newLinkFormat": "relative",
              "attachmentFolderPath": "Assets/Images",
              "strictLineBreaks": true
            }"#,
        )
        .expect("app config should be written");
        fs::write(
            vault_root.join(".obsidian/templates.json"),
            r#"{
              "dateFormat": "DD/MM/YYYY",
              "timeFormat": "HH:mm",
              "folder": "Templates/Core"
            }"#,
        )
        .expect("templates config should be written");
        fs::write(
            vault_root.join(".obsidian/types.json"),
            r#"{
              "status": "text",
              "reviewed": { "type": "checkbox" }
            }"#,
        )
        .expect("types config should be written");
        let paths = VaultPaths::new(vault_root);

        let report = import_core_plugin_config(&paths).expect("core import should succeed");

        assert_eq!(report.plugin, "core");
        assert_eq!(report.source_paths.len(), 3);
        assert_eq!(report.target_file, paths.config_file());
        assert!(!report.dry_run);
        let rendered = fs::read_to_string(paths.config_file()).expect("config should exist");
        assert!(rendered.contains("[links]"));
        assert!(rendered.contains("style = \"markdown\""));
        assert!(rendered.contains("resolution = \"relative\""));
        assert!(rendered.contains("attachment_folder = \"Assets/Images\""));
        assert!(rendered.contains("strict_line_breaks = true"));
        assert!(rendered.contains("[templates]"));
        assert!(rendered.contains("date_format = \"DD/MM/YYYY\""));
        assert!(rendered.contains("time_format = \"HH:mm\""));
        assert!(rendered.contains("obsidian_folder = \"Templates/Core\""));
        assert!(rendered.contains("[property_types]"));
        assert!(rendered.contains("status = \"text\""));
        assert!(rendered.contains("reviewed = \"checkbox\""));
    }

    #[test]
    fn core_importer_supports_partial_sources_and_local_target() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".obsidian")).expect("obsidian dir should be created");
        fs::write(
            vault_root.join(".obsidian/app.json"),
            r#"{
              "strictLineBreaks": true
            }"#,
        )
        .expect("app config should be written");
        let paths = VaultPaths::new(vault_root);

        let report = CoreImporter
            .import(&paths, ImportTarget::Local)
            .expect("core local import should succeed");

        assert_eq!(
            report.source_paths,
            vec![vault_root.join(".obsidian/app.json")]
        );
        assert_eq!(report.target_file, paths.local_config_file());
        assert!(paths.local_config_file().exists());
        assert!(!paths.config_file().exists());
        let rendered =
            fs::read_to_string(paths.local_config_file()).expect("local config should exist");
        assert!(rendered.contains("strict_line_breaks = true"));
        assert!(!rendered.contains("[templates]"));
        assert!(!rendered.contains("[property_types]"));
    }
}
