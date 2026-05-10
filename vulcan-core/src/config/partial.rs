#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialVulcanConfig {
    pub(super) scan: Option<PartialScanConfig>,
    pub(super) chunking: Option<PartialChunkingConfig>,
    pub(super) links: Option<PartialLinksConfig>,
    pub(super) embedding: Option<PartialEmbeddingProviderConfig>,
    pub(super) extraction: Option<PartialAttachmentExtractionConfig>,
    pub(super) git: Option<PartialGitConfig>,
    pub(super) inbox: Option<PartialInboxConfig>,
    pub(super) tasks: Option<PartialTasksConfig>,
    pub(super) tasknotes: Option<PartialTaskNotesConfig>,
    pub(super) kanban: Option<PartialKanbanConfig>,
    pub(super) dataview: Option<PartialDataviewConfig>,
    pub(super) js_runtime: Option<PartialJsRuntimeConfig>,
    pub(super) templates: Option<PartialTemplatesConfig>,
    pub(super) quickadd: Option<PartialQuickAddConfig>,
    pub(super) assistant: Option<PartialAssistantConfig>,
    pub(super) web: Option<PartialWebConfig>,
    pub(super) periodic: Option<PartialPeriodicConfig>,
    pub(super) export: Option<PartialExportConfig>,
    pub(super) site: Option<PartialSiteConfig>,
    pub(super) permissions: Option<PartialPermissionsConfig>,
    pub(super) plugins: Option<BTreeMap<String, PartialPluginRegistration>>,
    pub(super) aliases: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialScanConfig {
    pub(super) default_mode: Option<AutoScanMode>,
    pub(super) browse_mode: Option<AutoScanMode>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialChunkingConfig {
    pub(super) strategy: Option<ChunkingStrategy>,
    pub(super) target_size: Option<usize>,
    pub(super) overlap: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialLinksConfig {
    pub(super) resolution: Option<LinkResolutionMode>,
    pub(super) style: Option<LinkStylePreference>,
    pub(super) attachment_folder: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialEmbeddingProviderConfig {
    pub(super) provider: Option<String>,
    pub(super) base_url: Option<String>,
    pub(super) model: Option<String>,
    pub(super) api_key_env: Option<String>,
    pub(super) normalized: Option<bool>,
    pub(super) max_batch_size: Option<usize>,
    pub(super) max_input_tokens: Option<usize>,
    pub(super) max_concurrency: Option<usize>,
    pub(super) cache_key: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialAttachmentExtractionConfig {
    pub(super) command: Option<String>,
    pub(super) args: Option<Vec<String>>,
    pub(super) extensions: Option<Vec<String>>,
    pub(super) max_output_bytes: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialExportConfig {
    pub(super) profiles: Option<BTreeMap<String, ExportProfileConfig>>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialSiteConfig {
    pub(super) profiles: Option<BTreeMap<String, SiteProfileConfig>>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialGitConfig {
    pub(super) auto_commit: Option<bool>,
    pub(super) trigger: Option<GitTrigger>,
    pub(super) message: Option<String>,
    pub(super) scope: Option<GitScope>,
    pub(super) exclude: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialInboxConfig {
    pub(super) path: Option<String>,
    pub(super) format: Option<String>,
    pub(super) timestamp: Option<bool>,
    pub(super) heading: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialPluginRegistration {
    pub(super) enabled: Option<bool>,
    pub(super) path: Option<PathBuf>,
    pub(super) events: Option<Vec<PluginEvent>>,
    pub(super) sandbox: Option<JsRuntimeSandbox>,
    pub(super) permission_profile: Option<String>,
    pub(super) description: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialTemplatesConfig {
    pub(super) date_format: Option<String>,
    pub(super) time_format: Option<String>,
    pub(super) obsidian_folder: Option<PathBuf>,
    pub(super) templater_folder: Option<PathBuf>,
    pub(super) command_timeout: Option<usize>,
    pub(super) templates_pairs: Option<Vec<TemplaterCommandPairConfig>>,
    pub(super) trigger_on_file_creation: Option<bool>,
    pub(super) auto_jump_to_cursor: Option<bool>,
    pub(super) enable_system_commands: Option<bool>,
    pub(super) shell_path: Option<PathBuf>,
    pub(super) user_scripts_folder: Option<PathBuf>,
    pub(super) web_allowlist: Option<Vec<String>>,
    pub(super) enable_folder_templates: Option<bool>,
    pub(super) folder_templates: Option<Vec<TemplaterFolderTemplateConfig>>,
    pub(super) enable_file_templates: Option<bool>,
    pub(super) file_templates: Option<Vec<TemplaterFileTemplateConfig>>,
    pub(super) syntax_highlighting: Option<bool>,
    pub(super) syntax_highlighting_mobile: Option<bool>,
    pub(super) enabled_templates_hotkeys: Option<Vec<String>>,
    pub(super) startup_templates: Option<Vec<String>>,
    pub(super) intellisense_render: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialPermissionsConfig {
    pub(super) profiles: Option<BTreeMap<String, PartialPermissionProfile>>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialPermissionProfile {
    pub(super) read: Option<PathPermissionConfig>,
    pub(super) write: Option<PathPermissionConfig>,
    pub(super) refactor: Option<PathPermissionConfig>,
    pub(super) git: Option<PermissionMode>,
    pub(super) network: Option<NetworkPermissionConfig>,
    pub(super) index: Option<PermissionMode>,
    pub(super) config: Option<ConfigPermissionMode>,
    pub(super) execute: Option<PermissionMode>,
    pub(super) shell: Option<PermissionMode>,
    pub(super) cpu_limit_ms: Option<PermissionLimit>,
    pub(super) memory_limit_mb: Option<PermissionLimit>,
    pub(super) stack_limit_kb: Option<PermissionLimit>,
    pub(super) policy_hook: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialQuickAddConfig {
    pub(super) template_folder: Option<PathBuf>,
    pub(super) global_variables: Option<BTreeMap<String, String>>,
    pub(super) capture_choices: Option<Vec<QuickAddCaptureChoiceConfig>>,
    pub(super) template_choices: Option<Vec<QuickAddTemplateChoiceConfig>>,
    pub(super) ai: Option<PartialQuickAddAiConfig>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialQuickAddAiConfig {
    pub(super) default_model: Option<String>,
    pub(super) default_system_prompt: Option<String>,
    pub(super) prompt_templates_folder: Option<PathBuf>,
    pub(super) show_assistant: Option<bool>,
    pub(super) providers: Option<Vec<QuickAddAiProviderConfig>>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(clippy::struct_field_names)]
pub(super) struct PartialAssistantConfig {
    pub(super) prompts_folder: Option<PathBuf>,
    pub(super) skills_folder: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialWebConfig {
    pub(super) user_agent: Option<String>,
    pub(super) search: Option<PartialWebSearchConfig>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialWebSearchConfig {
    pub(super) backend: Option<SearchBackendKind>,
    pub(super) api_key_env: Option<String>,
    pub(super) base_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialPeriodicConfig {
    #[serde(flatten)]
    pub(super) notes: BTreeMap<String, PartialPeriodicNoteConfig>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialPeriodicNoteConfig {
    pub(super) enabled: Option<bool>,
    pub(super) folder: Option<PathBuf>,
    pub(super) format: Option<String>,
    pub(super) unit: Option<PeriodicCadenceUnit>,
    pub(super) interval: Option<usize>,
    pub(super) anchor_date: Option<String>,
    pub(super) template: Option<String>,
    pub(super) start_of_week: Option<PeriodicStartOfWeek>,
    pub(super) schedule_heading: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialTasksConfig {
    pub(super) default_source: Option<TasksDefaultSource>,
    pub(super) statuses: Option<PartialTaskStatusesConfig>,
    pub(super) global_filter: Option<String>,
    pub(super) global_query: Option<String>,
    pub(super) remove_global_filter: Option<bool>,
    pub(super) set_created_date: Option<bool>,
    pub(super) recurrence_on_completion: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialTaskNotesConfig {
    pub(super) tasks_folder: Option<String>,
    pub(super) archive_folder: Option<String>,
    pub(super) task_tag: Option<String>,
    pub(super) identification_method: Option<TaskNotesIdentificationMethod>,
    pub(super) task_property_name: Option<String>,
    pub(super) task_property_value: Option<String>,
    pub(super) excluded_folders: Option<Vec<String>>,
    pub(super) default_status: Option<String>,
    pub(super) default_priority: Option<String>,
    pub(super) field_mapping: Option<PartialTaskNotesFieldMapping>,
    pub(super) statuses: Option<Vec<TaskNotesStatusConfig>>,
    pub(super) priorities: Option<Vec<TaskNotesPriorityConfig>>,
    pub(super) user_fields: Option<Vec<TaskNotesUserFieldConfig>>,
    pub(super) enable_natural_language_input: Option<bool>,
    pub(super) nlp_default_to_scheduled: Option<bool>,
    pub(super) nlp_language: Option<String>,
    pub(super) nlp_triggers: Option<Vec<TaskNotesNlpTriggerConfig>>,
    pub(super) pomodoro: Option<TaskNotesPomodoroConfig>,
    pub(super) task_creation_defaults: Option<TaskNotesTaskCreationDefaults>,
    pub(super) saved_views: Option<Vec<TaskNotesSavedViewConfig>>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialTaskNotesFieldMapping {
    pub(super) title: Option<String>,
    pub(super) status: Option<String>,
    pub(super) priority: Option<String>,
    pub(super) due: Option<String>,
    pub(super) scheduled: Option<String>,
    pub(super) contexts: Option<String>,
    pub(super) projects: Option<String>,
    pub(super) time_estimate: Option<String>,
    pub(super) completed_date: Option<String>,
    pub(super) date_created: Option<String>,
    pub(super) date_modified: Option<String>,
    pub(super) recurrence: Option<String>,
    pub(super) recurrence_anchor: Option<String>,
    pub(super) archive_tag: Option<String>,
    pub(super) time_entries: Option<String>,
    pub(super) complete_instances: Option<String>,
    pub(super) skipped_instances: Option<String>,
    pub(super) blocked_by: Option<String>,
    pub(super) pomodoros: Option<String>,
    pub(super) reminders: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialKanbanConfig {
    pub(super) date_trigger: Option<String>,
    pub(super) time_trigger: Option<String>,
    pub(super) date_format: Option<String>,
    pub(super) time_format: Option<String>,
    pub(super) date_display_format: Option<String>,
    pub(super) date_time_display_format: Option<String>,
    pub(super) link_date_to_daily_note: Option<bool>,
    pub(super) metadata_keys: Option<Vec<KanbanMetadataKeyConfig>>,
    pub(super) archive_with_date: Option<bool>,
    pub(super) append_archive_date: Option<bool>,
    pub(super) archive_date_format: Option<String>,
    pub(super) archive_date_separator: Option<String>,
    pub(super) new_card_insertion_method: Option<String>,
    pub(super) new_line_trigger: Option<String>,
    pub(super) new_note_folder: Option<String>,
    pub(super) new_note_template: Option<String>,
    pub(super) hide_card_count: Option<bool>,
    pub(super) hide_tags_in_title: Option<bool>,
    pub(super) hide_tags_display: Option<bool>,
    pub(super) inline_metadata_position: Option<String>,
    pub(super) lane_width: Option<usize>,
    pub(super) full_list_lane_width: Option<bool>,
    pub(super) list_collapse: Option<Vec<bool>>,
    pub(super) max_archive_size: Option<usize>,
    pub(super) show_checkboxes: Option<bool>,
    pub(super) move_dates: Option<bool>,
    pub(super) move_tags: Option<bool>,
    pub(super) move_task_metadata: Option<bool>,
    pub(super) show_add_list: Option<bool>,
    pub(super) show_archive_all: Option<bool>,
    pub(super) show_board_settings: Option<bool>,
    pub(super) show_relative_date: Option<bool>,
    pub(super) show_search: Option<bool>,
    pub(super) show_set_view: Option<bool>,
    pub(super) show_view_as_markdown: Option<bool>,
    pub(super) date_picker_week_start: Option<usize>,
    pub(super) table_sizing: Option<BTreeMap<String, usize>>,
    pub(super) tag_action: Option<String>,
    pub(super) tag_colors: Option<Vec<KanbanTagColorConfig>>,
    pub(super) tag_sort: Option<Vec<KanbanTagSortConfig>>,
    pub(super) date_colors: Option<Vec<KanbanDateColorConfig>>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialTaskStatusesConfig {
    pub(super) todo: Option<Vec<String>>,
    pub(super) completed: Option<Vec<String>>,
    pub(super) in_progress: Option<Vec<String>>,
    pub(super) cancelled: Option<Vec<String>>,
    pub(super) non_task: Option<Vec<String>>,
    pub(super) definitions: Option<Vec<TaskStatusDefinition>>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialDataviewConfig {
    pub(super) inline_query_prefix: Option<String>,
    pub(super) inline_js_query_prefix: Option<String>,
    pub(super) enable_dataview_js: Option<bool>,
    pub(super) enable_inline_dataview_js: Option<bool>,
    pub(super) task_completion_tracking: Option<bool>,
    pub(super) task_completion_use_emoji_shorthand: Option<bool>,
    pub(super) task_completion_text: Option<String>,
    pub(super) recursive_subtask_completion: Option<bool>,
    pub(super) display_result_count: Option<bool>,
    pub(super) default_date_format: Option<String>,
    pub(super) default_datetime_format: Option<String>,
    pub(super) timezone: Option<String>,
    pub(super) max_recursive_render_depth: Option<usize>,
    pub(super) primary_column_name: Option<String>,
    pub(super) group_column_name: Option<String>,
    pub(super) js_timeout_seconds: Option<usize>,
    pub(super) js_memory_limit_bytes: Option<usize>,
    pub(super) js_max_stack_size_bytes: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct PartialJsRuntimeConfig {
    pub(super) memory_limit_mb: Option<usize>,
    pub(super) stack_limit_kb: Option<usize>,
    pub(super) default_timeout_seconds: Option<usize>,
    pub(super) default_sandbox: Option<JsRuntimeSandbox>,
    pub(super) scripts_folder: Option<PathBuf>,
}
