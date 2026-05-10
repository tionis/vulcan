#![allow(clippy::wildcard_imports)]

use super::*;

pub(super) fn default_attachment_extraction_extensions() -> Vec<String> {
    [
        "pdf", "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

pub(super) fn default_todo_task_statuses() -> Vec<String> {
    vec![" ".to_string()]
}

pub(super) fn default_completed_task_statuses() -> Vec<String> {
    vec!["x".to_string(), "X".to_string()]
}

pub(super) fn default_in_progress_task_statuses() -> Vec<String> {
    vec!["/".to_string()]
}

pub(super) fn default_cancelled_task_statuses() -> Vec<String> {
    vec!["-".to_string()]
}

pub(super) fn default_non_task_statuses() -> Vec<String> {
    Vec::new()
}

pub(super) fn default_tasknotes_auto_archive_delay() -> usize {
    5
}

pub(super) fn default_tasknotes_statuses() -> Vec<TaskNotesStatusConfig> {
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

pub(super) fn default_tasknotes_priorities() -> Vec<TaskNotesPriorityConfig> {
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

pub(super) fn default_tasknotes_pomodoro_work_duration() -> usize {
    25
}

pub(super) fn default_tasknotes_pomodoro_short_break() -> usize {
    5
}

pub(super) fn default_tasknotes_pomodoro_long_break() -> usize {
    15
}

pub(super) fn default_tasknotes_pomodoro_long_break_interval() -> usize {
    4
}

pub(super) fn default_tasknotes_nlp_language() -> String {
    "en".to_string()
}

pub(super) fn default_tasknotes_nlp_triggers() -> Vec<TaskNotesNlpTriggerConfig> {
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

pub(super) fn default_dataview_inline_query_prefix() -> String {
    "=".to_string()
}

pub(super) fn default_dataview_inline_js_query_prefix() -> String {
    "$=".to_string()
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn default_dataview_enable_dataview_js() -> bool {
    true
}

pub(super) fn default_dataview_enable_inline_dataview_js() -> bool {
    false
}

pub(super) fn default_dataview_task_completion_tracking() -> bool {
    false
}

pub(super) fn default_dataview_task_completion_use_emoji_shorthand() -> bool {
    false
}

pub(super) fn default_dataview_task_completion_text() -> String {
    "completion".to_string()
}

pub(super) fn default_dataview_recursive_subtask_completion() -> bool {
    false
}

pub(super) fn default_dataview_display_result_count() -> bool {
    true
}

pub(super) fn default_dataview_default_date_format() -> String {
    "MMMM dd, yyyy".to_string()
}

pub(super) fn default_dataview_default_datetime_format() -> String {
    "h:mm a - MMMM dd, yyyy".to_string()
}

pub(super) fn default_dataview_max_recursive_render_depth() -> usize {
    4
}

pub(super) fn default_dataview_primary_column_name() -> String {
    "File".to_string()
}

pub(super) fn default_dataview_group_column_name() -> String {
    "Group".to_string()
}

pub(super) fn default_dataview_js_timeout_seconds() -> usize {
    30
}

pub(super) fn default_dataview_js_memory_limit_bytes() -> usize {
    16 * 1024 * 1024
}

pub(super) fn default_dataview_js_max_stack_size_bytes() -> usize {
    256 * 1024
}

pub(super) fn default_js_runtime_memory_limit_mb() -> usize {
    64
}

pub(super) fn default_js_runtime_stack_limit_kb() -> usize {
    256
}

pub(super) fn default_js_runtime_default_timeout_seconds() -> usize {
    30
}

pub(super) fn default_js_runtime_scripts_folder() -> PathBuf {
    PathBuf::from(".vulcan/scripts")
}

pub(super) fn default_enabled_plugin_registration() -> bool {
    true
}

pub(super) fn bytes_to_megabytes_ceil(bytes: usize) -> usize {
    bytes.saturating_add((1024 * 1024) - 1) / (1024 * 1024)
}

pub(super) fn bytes_to_kilobytes_ceil(bytes: usize) -> usize {
    bytes.saturating_add(1024 - 1) / 1024
}
