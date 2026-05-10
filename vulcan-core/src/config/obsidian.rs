#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianAppConfig {
    #[serde(rename = "useMarkdownLinks")]
    pub(super) use_markdown_links: Option<bool>,
    #[serde(rename = "newLinkFormat")]
    pub(super) new_link_format: Option<LinkResolutionMode>,
    #[serde(rename = "attachmentFolderPath")]
    pub(super) attachment_folder_path: Option<String>,
    #[serde(rename = "strictLineBreaks")]
    pub(super) strict_line_breaks: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianTemplatesConfig {
    #[serde(rename = "dateFormat")]
    pub(super) date_format: Option<String>,
    #[serde(rename = "timeFormat")]
    pub(super) time_format: Option<String>,
    #[serde(
        rename = "folder",
        alias = "templateFolder",
        alias = "folderPath",
        alias = "templateFolderPath"
    )]
    pub(super) folder: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianDailyNotesConfig {
    pub(super) folder: Option<String>,
    pub(super) format: Option<String>,
    pub(super) template: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianPeriodicNoteSettings {
    pub(super) enabled: Option<bool>,
    pub(super) folder: Option<String>,
    pub(super) format: Option<String>,
    #[serde(rename = "templatePath", alias = "template")]
    pub(super) template_path: Option<String>,
    #[serde(rename = "startOfWeek")]
    pub(super) start_of_week: Option<PeriodicStartOfWeek>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianPeriodicNotesConfig {
    pub(super) daily: Option<ObsidianPeriodicNoteSettings>,
    pub(super) weekly: Option<ObsidianPeriodicNoteSettings>,
    pub(super) monthly: Option<ObsidianPeriodicNoteSettings>,
    pub(super) quarterly: Option<ObsidianPeriodicNoteSettings>,
    pub(super) yearly: Option<ObsidianPeriodicNoteSettings>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianTemplaterConfig {
    pub(super) command_timeout: Option<usize>,
    pub(super) templates_folder: Option<String>,
    #[serde(default)]
    pub(super) templates_pairs: Vec<[String; 2]>,
    pub(super) trigger_on_file_creation: Option<bool>,
    pub(super) auto_jump_to_cursor: Option<bool>,
    pub(super) enable_system_commands: Option<bool>,
    pub(super) shell_path: Option<String>,
    pub(super) user_scripts_folder: Option<String>,
    pub(super) enable_folder_templates: Option<bool>,
    #[serde(default)]
    pub(super) folder_templates: Vec<ObsidianTemplaterFolderTemplateConfig>,
    pub(super) enable_file_templates: Option<bool>,
    #[serde(default)]
    pub(super) file_templates: Vec<TemplaterFileTemplateConfig>,
    pub(super) syntax_highlighting: Option<bool>,
    pub(super) syntax_highlighting_mobile: Option<bool>,
    #[serde(default)]
    pub(super) enabled_templates_hotkeys: Vec<String>,
    #[serde(default)]
    pub(super) startup_templates: Vec<String>,
    pub(super) intellisense_render: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianTemplaterFolderTemplateConfig {
    pub(super) folder: String,
    pub(super) template: String,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddConfig {
    #[serde(rename = "templateFolderPath")]
    pub(super) template_folder_path: Option<String>,
    #[serde(rename = "globalVariables", default)]
    pub(super) global_variables: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) choices: Vec<ObsidianQuickAddChoice>,
    pub(super) ai: Option<ObsidianQuickAddAiConfig>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddChoice {
    pub(super) id: Option<String>,
    pub(super) name: Option<String>,
    #[serde(rename = "type")]
    pub(super) choice_type: Option<String>,
    #[serde(rename = "captureTo")]
    pub(super) capture_to: Option<String>,
    #[serde(rename = "captureToActiveFile")]
    pub(super) capture_to_active_file: Option<bool>,
    #[serde(rename = "activeFileWritePosition")]
    pub(super) active_file_write_position: Option<String>,
    #[serde(rename = "createFileIfItDoesntExist")]
    pub(super) create_file_if_it_doesnt_exist: Option<ObsidianQuickAddCreateFileConfig>,
    pub(super) format: Option<ObsidianQuickAddFormatConfig>,
    #[serde(rename = "useSelectionAsCaptureValue")]
    pub(super) use_selection_as_capture_value: Option<bool>,
    pub(super) prepend: Option<bool>,
    pub(super) task: Option<bool>,
    #[serde(rename = "insertAfter")]
    pub(super) insert_after: Option<ObsidianQuickAddInsertAfterConfig>,
    #[serde(rename = "newLineCapture")]
    pub(super) new_line_capture: Option<ObsidianQuickAddNewLineCaptureConfig>,
    #[serde(rename = "openFile")]
    pub(super) open_file: Option<bool>,
    pub(super) templater: Option<ObsidianQuickAddTemplaterChoiceConfig>,
    #[serde(rename = "templatePath")]
    pub(super) template_path: Option<String>,
    pub(super) folder: Option<ObsidianQuickAddTemplateFolderConfig>,
    #[serde(rename = "fileNameFormat")]
    pub(super) file_name_format: Option<ObsidianQuickAddFormatConfig>,
    #[serde(rename = "fileExistsBehavior")]
    pub(super) file_exists_behavior: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddCreateFileConfig {
    pub(super) enabled: Option<bool>,
    #[serde(rename = "createWithTemplate")]
    pub(super) create_with_template: Option<bool>,
    pub(super) template: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddFormatConfig {
    pub(super) enabled: Option<bool>,
    pub(super) format: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddInsertAfterConfig {
    pub(super) enabled: Option<bool>,
    #[serde(rename = "after")]
    pub(super) heading: Option<String>,
    #[serde(rename = "insertAtEnd")]
    pub(super) insert_at_end: Option<bool>,
    #[serde(rename = "considerSubsections")]
    pub(super) consider_subsections: Option<bool>,
    #[serde(rename = "createIfNotFound")]
    pub(super) create_if_not_found: Option<bool>,
    #[serde(rename = "createIfNotFoundLocation")]
    pub(super) create_if_not_found_location: Option<String>,
    pub(super) inline: Option<bool>,
    #[serde(rename = "replaceExisting")]
    pub(super) replace_existing: Option<bool>,
    #[serde(rename = "blankLineAfterMatchMode")]
    pub(super) blank_line_after_match_mode: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddNewLineCaptureConfig {
    pub(super) enabled: Option<bool>,
    pub(super) direction: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddTemplaterChoiceConfig {
    #[serde(rename = "afterCapture")]
    pub(super) after_capture: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddTemplateFolderConfig {
    pub(super) enabled: Option<bool>,
    #[serde(default)]
    pub(super) folders: Vec<String>,
    #[serde(rename = "chooseWhenCreatingNote")]
    pub(super) choose_when_creating_note: Option<bool>,
    #[serde(rename = "createInSameFolderAsActiveFile")]
    pub(super) create_in_same_folder_as_active_file: Option<bool>,
    #[serde(rename = "chooseFromSubfolders")]
    pub(super) choose_from_subfolders: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddAiConfig {
    #[serde(rename = "defaultModel")]
    pub(super) default_model: Option<String>,
    #[serde(rename = "defaultSystemPrompt")]
    pub(super) default_system_prompt: Option<String>,
    #[serde(rename = "promptTemplatesFolderPath")]
    pub(super) prompt_templates_folder_path: Option<String>,
    #[serde(rename = "showAssistant")]
    pub(super) show_assistant: Option<bool>,
    #[serde(default)]
    pub(super) providers: Vec<ObsidianQuickAddAiProviderConfig>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddAiProviderConfig {
    pub(super) name: Option<String>,
    pub(super) endpoint: Option<String>,
    #[serde(rename = "apiKeyRef")]
    pub(super) api_key_ref: Option<String>,
    #[serde(rename = "apiKey")]
    pub(super) api_key: Option<String>,
    #[serde(default)]
    pub(super) models: Vec<ObsidianQuickAddAiModelConfig>,
    #[serde(rename = "autoSyncModels")]
    pub(super) auto_sync_models: Option<bool>,
    #[serde(rename = "modelSource")]
    pub(super) model_source: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianQuickAddAiModelConfig {
    pub(super) name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianDataviewConfig {
    #[serde(rename = "inlineQueryPrefix")]
    pub(super) inline_query_prefix: Option<String>,
    #[serde(rename = "inlineJsQueryPrefix")]
    pub(super) inline_js_query_prefix: Option<String>,
    #[serde(rename = "enableDataviewJs")]
    pub(super) enable_dataview_js: Option<bool>,
    #[serde(rename = "enableInlineDataviewJs")]
    pub(super) enable_inline_dataview_js: Option<bool>,
    #[serde(rename = "taskCompletionTracking")]
    pub(super) task_completion_tracking: Option<bool>,
    #[serde(rename = "taskCompletionUseEmojiShorthand")]
    pub(super) task_completion_use_emoji_shorthand: Option<bool>,
    #[serde(rename = "taskCompletionText")]
    pub(super) task_completion_text: Option<String>,
    #[serde(rename = "recursiveSubTaskCompletion")]
    pub(super) recursive_subtask_completion: Option<bool>,
    #[serde(rename = "displayResultCount", alias = "showResultCount")]
    pub(super) display_result_count: Option<bool>,
    #[serde(rename = "defaultDateFormat")]
    pub(super) default_date_format: Option<String>,
    #[serde(rename = "defaultDateTimeFormat")]
    pub(super) default_datetime_format: Option<String>,
    #[serde(rename = "timezone")]
    pub(super) timezone: Option<String>,
    #[serde(rename = "maxRecursiveRenderDepth")]
    pub(super) max_recursive_render_depth: Option<usize>,
    #[serde(rename = "primaryColumnName", alias = "tableIdColumnName")]
    pub(super) primary_column_name: Option<String>,
    #[serde(rename = "groupColumnName", alias = "tableGroupColumnName")]
    pub(super) group_column_name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianTasksConfig {
    #[serde(rename = "globalFilter")]
    pub(super) global_filter: Option<String>,
    #[serde(rename = "globalQuery")]
    pub(super) global_query: Option<String>,
    #[serde(rename = "removeGlobalFilter")]
    pub(super) remove_global_filter: Option<bool>,
    #[serde(rename = "setCreatedDate")]
    pub(super) set_created_date: Option<bool>,
    #[serde(rename = "recurrenceOnCompletion")]
    pub(super) recurrence_on_completion: Option<String>,
    #[serde(rename = "recurrenceOnNextLine")]
    pub(super) recurrence_on_next_line: Option<bool>,
    #[serde(rename = "statusSettings")]
    pub(super) status_settings: Option<ObsidianTasksStatusSettings>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianTasksStatusSettings {
    #[serde(rename = "coreStatuses", default)]
    pub(super) core_statuses: Vec<TaskStatusDefinition>,
    #[serde(rename = "customStatuses", default)]
    pub(super) custom_statuses: Vec<TaskStatusDefinition>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianTaskNotesConfig {
    #[serde(rename = "tasksFolder")]
    pub(super) tasks_folder: Option<String>,
    #[serde(rename = "archiveFolder")]
    pub(super) archive_folder: Option<String>,
    #[serde(rename = "taskTag")]
    pub(super) task_tag: Option<String>,
    #[serde(rename = "taskIdentificationMethod")]
    pub(super) task_identification_method: Option<TaskNotesIdentificationMethod>,
    #[serde(rename = "taskPropertyName")]
    pub(super) task_property_name: Option<String>,
    #[serde(rename = "taskPropertyValue")]
    pub(super) task_property_value: Option<String>,
    #[serde(rename = "excludedFolders")]
    pub(super) excluded_folders: Option<String>,
    #[serde(rename = "defaultTaskStatus")]
    pub(super) default_task_status: Option<String>,
    #[serde(rename = "defaultTaskPriority")]
    pub(super) default_task_priority: Option<String>,
    #[serde(rename = "fieldMapping")]
    pub(super) field_mapping: Option<ObsidianTaskNotesFieldMapping>,
    #[serde(rename = "customStatuses", default)]
    pub(super) custom_statuses: Vec<TaskNotesStatusConfig>,
    #[serde(rename = "customPriorities", default)]
    pub(super) custom_priorities: Vec<TaskNotesPriorityConfig>,
    #[serde(rename = "userFields", default)]
    pub(super) user_fields: Vec<TaskNotesUserFieldConfig>,
    #[serde(rename = "enableNaturalLanguageInput")]
    pub(super) enable_natural_language_input: Option<bool>,
    #[serde(rename = "nlpDefaultToScheduled")]
    pub(super) nlp_default_to_scheduled: Option<bool>,
    #[serde(rename = "nlpLanguage")]
    pub(super) nlp_language: Option<String>,
    #[serde(rename = "nlpTriggers")]
    pub(super) nlp_triggers: Option<ObsidianTaskNotesNlpTriggersConfig>,
    #[serde(rename = "pomodoroWorkDuration")]
    pub(super) pomodoro_work_duration: Option<usize>,
    #[serde(rename = "pomodoroShortBreakDuration")]
    pub(super) pomodoro_short_break_duration: Option<usize>,
    #[serde(rename = "pomodoroLongBreakDuration")]
    pub(super) pomodoro_long_break_duration: Option<usize>,
    #[serde(rename = "pomodoroLongBreakInterval")]
    pub(super) pomodoro_long_break_interval: Option<usize>,
    #[serde(rename = "pomodoroStorageLocation")]
    pub(super) pomodoro_storage_location: Option<TaskNotesPomodoroStorageLocation>,
    #[serde(rename = "taskCreationDefaults")]
    pub(super) task_creation_defaults: Option<ObsidianTaskNotesCreationDefaults>,
    #[serde(rename = "savedViews", default)]
    pub(super) saved_views: Vec<TaskNotesSavedViewConfig>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianTaskNotesFieldMapping {
    pub(super) title: Option<String>,
    pub(super) status: Option<String>,
    pub(super) priority: Option<String>,
    pub(super) due: Option<String>,
    pub(super) scheduled: Option<String>,
    pub(super) contexts: Option<String>,
    pub(super) projects: Option<String>,
    #[serde(rename = "timeEstimate")]
    pub(super) time_estimate: Option<String>,
    #[serde(rename = "completedDate")]
    pub(super) completed_date: Option<String>,
    #[serde(rename = "dateCreated")]
    pub(super) date_created: Option<String>,
    #[serde(rename = "dateModified")]
    pub(super) date_modified: Option<String>,
    pub(super) recurrence: Option<String>,
    #[serde(rename = "recurrenceAnchor")]
    pub(super) recurrence_anchor: Option<String>,
    #[serde(rename = "archiveTag")]
    pub(super) archive_tag: Option<String>,
    #[serde(rename = "timeEntries")]
    pub(super) time_entries: Option<String>,
    #[serde(rename = "completeInstances")]
    pub(super) complete_instances: Option<String>,
    #[serde(rename = "skippedInstances")]
    pub(super) skipped_instances: Option<String>,
    #[serde(rename = "blockedBy")]
    pub(super) blocked_by: Option<String>,
    pub(super) pomodoros: Option<String>,
    pub(super) reminders: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianTaskNotesNlpTriggersConfig {
    #[serde(default)]
    pub(super) triggers: Vec<TaskNotesNlpTriggerConfig>,
}

#[allow(clippy::struct_field_names)]
#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianTaskNotesCreationDefaults {
    #[serde(rename = "defaultContexts")]
    pub(super) default_contexts: Option<String>,
    #[serde(rename = "defaultTags")]
    pub(super) default_tags: Option<String>,
    #[serde(rename = "defaultProjects")]
    pub(super) default_projects: Option<String>,
    #[serde(rename = "defaultTimeEstimate")]
    pub(super) default_time_estimate: Option<usize>,
    #[serde(rename = "defaultDueDate")]
    pub(super) default_due_date: Option<TaskNotesDateDefault>,
    #[serde(rename = "defaultScheduledDate")]
    pub(super) default_scheduled_date: Option<TaskNotesDateDefault>,
    #[serde(rename = "defaultRecurrence")]
    pub(super) default_recurrence: Option<TaskNotesRecurrenceDefault>,
    #[serde(rename = "defaultReminders", default)]
    pub(super) default_reminders: Vec<ObsidianTaskNotesDefaultReminder>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianTaskNotesDefaultReminder {
    pub(super) id: Option<String>,
    #[serde(rename = "type")]
    pub(super) reminder_type: Option<TaskNotesDefaultReminderType>,
    #[serde(rename = "relatedTo")]
    pub(super) related_to: Option<TaskNotesReminderAnchor>,
    pub(super) offset: Option<i64>,
    pub(super) unit: Option<TaskNotesReminderUnit>,
    pub(super) direction: Option<TaskNotesReminderDirection>,
    #[serde(rename = "absoluteTime")]
    pub(super) absolute_time: Option<String>,
    #[serde(rename = "absoluteDate")]
    pub(super) absolute_date: Option<String>,
    pub(super) description: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ObsidianKanbanConfig {
    #[serde(rename = "date-trigger")]
    pub(super) date_trigger: Option<String>,
    #[serde(rename = "time-trigger")]
    pub(super) time_trigger: Option<String>,
    #[serde(rename = "date-format")]
    pub(super) date_format: Option<String>,
    #[serde(rename = "time-format")]
    pub(super) time_format: Option<String>,
    #[serde(rename = "date-display-format")]
    pub(super) date_display_format: Option<String>,
    #[serde(rename = "date-time-display-format")]
    pub(super) date_time_display_format: Option<String>,
    #[serde(rename = "link-date-to-daily-note")]
    pub(super) link_date_to_daily_note: Option<bool>,
    #[serde(rename = "metadata-keys")]
    pub(super) metadata_keys: Option<Vec<KanbanMetadataKeyConfig>>,
    #[serde(rename = "archive-with-date")]
    pub(super) archive_with_date: Option<bool>,
    #[serde(rename = "append-archive-date", alias = "prepend-archive-date")]
    pub(super) append_archive_date: Option<bool>,
    #[serde(rename = "archive-date-format")]
    pub(super) archive_date_format: Option<String>,
    #[serde(rename = "archive-date-separator")]
    pub(super) archive_date_separator: Option<String>,
    #[serde(rename = "new-card-insertion-method")]
    pub(super) new_card_insertion_method: Option<String>,
    #[serde(rename = "new-line-trigger")]
    pub(super) new_line_trigger: Option<String>,
    #[serde(rename = "new-note-folder")]
    pub(super) new_note_folder: Option<String>,
    #[serde(rename = "new-note-template")]
    pub(super) new_note_template: Option<String>,
    #[serde(rename = "hide-card-count")]
    pub(super) hide_card_count: Option<bool>,
    #[serde(rename = "hide-tags-in-title")]
    pub(super) hide_tags_in_title: Option<bool>,
    #[serde(rename = "hide-tags-display")]
    pub(super) hide_tags_display: Option<bool>,
    #[serde(rename = "inline-metadata-position")]
    pub(super) inline_metadata_position: Option<String>,
    #[serde(rename = "lane-width")]
    pub(super) lane_width: Option<usize>,
    #[serde(rename = "full-list-lane-width")]
    pub(super) full_list_lane_width: Option<bool>,
    #[serde(rename = "list-collapse")]
    pub(super) list_collapse: Option<Vec<bool>>,
    #[serde(rename = "max-archive-size")]
    pub(super) max_archive_size: Option<usize>,
    #[serde(rename = "show-checkboxes")]
    pub(super) show_checkboxes: Option<bool>,
    #[serde(rename = "move-dates")]
    pub(super) move_dates: Option<bool>,
    #[serde(rename = "move-tags")]
    pub(super) move_tags: Option<bool>,
    #[serde(rename = "move-task-metadata")]
    pub(super) move_task_metadata: Option<bool>,
    #[serde(rename = "show-add-list")]
    pub(super) show_add_list: Option<bool>,
    #[serde(rename = "show-archive-all")]
    pub(super) show_archive_all: Option<bool>,
    #[serde(rename = "show-board-settings")]
    pub(super) show_board_settings: Option<bool>,
    #[serde(rename = "show-relative-date")]
    pub(super) show_relative_date: Option<bool>,
    #[serde(rename = "show-search")]
    pub(super) show_search: Option<bool>,
    #[serde(rename = "show-set-view")]
    pub(super) show_set_view: Option<bool>,
    #[serde(rename = "show-view-as-markdown")]
    pub(super) show_view_as_markdown: Option<bool>,
    #[serde(rename = "date-picker-week-start")]
    pub(super) date_picker_week_start: Option<usize>,
    #[serde(rename = "table-sizing")]
    pub(super) table_sizing: Option<BTreeMap<String, usize>>,
    #[serde(rename = "tag-action")]
    pub(super) tag_action: Option<String>,
    #[serde(rename = "tag-colors")]
    pub(super) tag_colors: Option<Vec<KanbanTagColorConfig>>,
    #[serde(rename = "tag-sort")]
    pub(super) tag_sort: Option<Vec<KanbanTagSortConfig>>,
    #[serde(rename = "date-colors")]
    pub(super) date_colors: Option<Vec<KanbanDateColorConfig>>,
}
