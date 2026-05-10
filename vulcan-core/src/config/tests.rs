use super::*;
use proptest::prelude::*;
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
default_source = "tasknotes"
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

fn path_segment_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9_-]{1,8}")
        .expect("path segment regex should be valid")
}

fn path_permission_config_strategy() -> impl Strategy<Value = PathPermissionConfig> {
    prop_oneof![
        Just(PathPermissionConfig::Keyword(PathPermissionKeyword::All)),
        Just(PathPermissionConfig::Keyword(PathPermissionKeyword::None)),
        path_segment_strategy().prop_map(|folder| {
            PathPermissionConfig::Rules(PathPermissionRules {
                allow: vec![format!("folder:{folder}/**")],
                deny: vec![format!("note:{folder}/Secret.md")],
            })
        }),
    ]
}

fn permission_mode_strategy() -> impl Strategy<Value = PermissionMode> {
    prop_oneof![Just(PermissionMode::Allow), Just(PermissionMode::Deny)]
}

fn config_permission_mode_strategy() -> impl Strategy<Value = ConfigPermissionMode> {
    prop_oneof![
        Just(ConfigPermissionMode::Read),
        Just(ConfigPermissionMode::Write),
        Just(ConfigPermissionMode::None),
    ]
}

fn network_permission_config_strategy() -> impl Strategy<Value = NetworkPermissionConfig> {
    prop_oneof![
        permission_mode_strategy().prop_map(NetworkPermissionConfig::Mode),
        path_segment_strategy().prop_map(|domain| {
            NetworkPermissionConfig::Details(NetworkPermissionDetails {
                allow: true,
                domains: vec![format!("{domain}.example.com")],
            })
        }),
    ]
}

fn permission_limit_strategy() -> impl Strategy<Value = PermissionLimit> {
    prop_oneof![
        Just(PermissionLimit::Keyword(PermissionLimitKeyword::Unlimited)),
        (1_usize..10_000).prop_map(PermissionLimit::Value),
    ]
}

fn permission_profile_strategy() -> impl Strategy<Value = PermissionProfile> {
    (
        (
            path_permission_config_strategy(),
            path_permission_config_strategy(),
            path_permission_config_strategy(),
        ),
        (
            permission_mode_strategy(),
            network_permission_config_strategy(),
            permission_mode_strategy(),
            config_permission_mode_strategy(),
            permission_mode_strategy(),
            permission_mode_strategy(),
        ),
        (
            permission_limit_strategy(),
            permission_limit_strategy(),
            permission_limit_strategy(),
            prop::option::of(
                path_segment_strategy().prop_map(|name| PathBuf::from(format!("hooks/{name}.sh"))),
            ),
        ),
    )
        .prop_map(
            |(
                (read, write, refactor),
                (git, network, index, config, execute, shell),
                (cpu_limit_ms, memory_limit_mb, stack_limit_kb, policy_hook),
            )| PermissionProfile {
                read,
                write,
                refactor,
                git,
                network,
                index,
                config,
                execute,
                shell,
                cpu_limit_ms,
                memory_limit_mb,
                stack_limit_kb,
                policy_hook,
            },
        )
}

fn partial_permission_profile_strategy() -> impl Strategy<Value = PartialPermissionProfile> {
    (
        (
            prop::option::of(path_permission_config_strategy()),
            prop::option::of(path_permission_config_strategy()),
            prop::option::of(path_permission_config_strategy()),
        ),
        (
            prop::option::of(permission_mode_strategy()),
            prop::option::of(network_permission_config_strategy()),
            prop::option::of(permission_mode_strategy()),
            prop::option::of(config_permission_mode_strategy()),
            prop::option::of(permission_mode_strategy()),
            prop::option::of(permission_mode_strategy()),
        ),
        (
            prop::option::of(permission_limit_strategy()),
            prop::option::of(permission_limit_strategy()),
            prop::option::of(permission_limit_strategy()),
            prop::option::of(
                path_segment_strategy().prop_map(|name| PathBuf::from(format!("policy/{name}.sh"))),
            ),
        ),
    )
        .prop_map(
            |(
                (read, write, refactor),
                (git, network, index, config, execute, shell),
                (cpu_limit_ms, memory_limit_mb, stack_limit_kb, policy_hook),
            )| PartialPermissionProfile {
                read,
                write,
                refactor,
                git,
                network,
                index,
                config,
                execute,
                shell,
                cpu_limit_ms,
                memory_limit_mb,
                stack_limit_kb,
                policy_hook,
            },
        )
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
    assert_eq!(config.tasks.default_source, TasksDefaultSource::Tasknotes);
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
fn builtin_defaults_include_command_aliases() {
    let defaults = VaultConfig::default();

    assert_eq!(defaults.aliases.get("q"), Some(&"query".to_string()));
    assert_eq!(defaults.aliases.get("t"), Some(&"tasks list".to_string()));
    assert_eq!(
        defaults.aliases.get("today"),
        Some(&"daily today".to_string())
    );
    assert!(defaults.plugins.is_empty());
}

#[test]
fn builtin_defaults_include_assistant_paths() {
    let defaults = VaultConfig::default();

    assert_eq!(
        defaults.assistant.prompts_folder,
        PathBuf::from("AI/Prompts")
    );
    assert_eq!(
        defaults.assistant.skills_folder,
        PathBuf::from(".agents/skills")
    );
    assert_eq!(
        defaults.assistant.tools_folder,
        PathBuf::from(".agents/tools")
    );
}

#[test]
fn vulcan_config_can_override_assistant_settings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[assistant]
prompts_folder = "Shared/Prompts"
skills_folder = "Shared/Skills"
tools_folder = "Shared/Tools"
"#,
    )
    .expect("config should be written");

    let loaded = load_vault_config(&VaultPaths::new(vault_root));

    assert_eq!(
        loaded.config.assistant.prompts_folder,
        PathBuf::from("Shared/Prompts")
    );
    assert_eq!(
        loaded.config.assistant.skills_folder,
        PathBuf::from("Shared/Skills")
    );
    assert_eq!(
        loaded.config.assistant.tools_folder,
        PathBuf::from("Shared/Tools")
    );
}

#[test]
fn vulcan_config_aliases_override_builtin_defaults() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[aliases]\ntoday = \"daily show\"\nship = \"query --where 'status = shipped'\"\n",
    )
    .expect("config should be written");

    let loaded = load_vault_config(&VaultPaths::new(vault_root));

    assert_eq!(
        loaded.config.aliases.get("today"),
        Some(&"daily show".to_string())
    );
    assert_eq!(
        loaded.config.aliases.get("ship"),
        Some(&"query --where 'status = shipped'".to_string())
    );
    assert_eq!(loaded.config.aliases.get("q"), Some(&"query".to_string()));
}

#[test]
fn vulcan_config_loads_plugin_registrations() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[plugins.lint]
events = ["on_note_write", "on_pre_commit", "on_note_write"]
sandbox = "strict"
permission_profile = "readonly"
description = "  Validate note writes  "
"#,
    )
    .expect("config should be written");

    let loaded = load_vault_config(&VaultPaths::new(vault_root));
    let plugin = loaded
        .config
        .plugins
        .get("lint")
        .expect("plugin should be loaded");

    assert!(plugin.enabled);
    assert_eq!(
        plugin.events,
        vec![PluginEvent::OnNoteWrite, PluginEvent::OnPreCommit]
    );
    assert_eq!(plugin.sandbox, Some(JsRuntimeSandbox::Strict));
    assert_eq!(plugin.permission_profile.as_deref(), Some("readonly"));
    assert_eq!(plugin.description.as_deref(), Some("Validate note writes"));
}

#[test]
fn local_config_can_override_plugin_registration() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[plugins.lint]
enabled = true
events = ["on_note_write"]
"#,
    )
    .expect("shared config should be written");
    fs::write(
        vault_root.join(".vulcan/config.local.toml"),
        r#"
[plugins.lint]
enabled = false
events = ["on_scan_complete"]
path = ".vulcan/plugins/custom-lint.js"
"#,
    )
    .expect("local config should be written");

    let loaded = load_vault_config(&VaultPaths::new(vault_root));
    let plugin = loaded
        .config
        .plugins
        .get("lint")
        .expect("plugin should be loaded");

    assert!(!plugin.enabled);
    assert_eq!(plugin.events, vec![PluginEvent::OnScanComplete]);
    assert_eq!(
        plugin.path.as_ref(),
        Some(&PathBuf::from(".vulcan/plugins/custom-lint.js"))
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn vulcan_config_loads_export_profiles() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[export.profiles.team_book]
format = "epub"
query = 'from notes where file.path starts_with "Guides/"'
path = "exports/team-book.epub"
title = "Team Book"
author = "Vulcan"
toc = "flat"
backlinks = true
frontmatter = true

[export.profiles.public_bundle]
format = "frontend-bundle"
path = "exports/public-bundle"
site_profile = "public"

[[export.profiles.team_book.content_transforms]]
exclude_callouts = ["secret gm", "internal"]
exclude_headings = ["Scratch"]
exclude_frontmatter_keys = ["email"]
exclude_inline_fields = ["owner"]
[[export.profiles.team_book.content_transforms.replace]]
pattern = "[[People/Bob]]"
replacement = "[[People/Alice]]"
[[export.profiles.team_book.content_transforms.replace]]
pattern = "[A-Za-z0-9._%+-]+@example\\.com"
replacement = "[redacted]"
regex = true
"#,
    )
    .expect("config should be written");

    let loaded = load_vault_config(&VaultPaths::new(vault_root));
    let profile = loaded
        .config
        .export
        .profiles
        .get("team_book")
        .expect("export profile should be loaded");
    let public_bundle = loaded
        .config
        .export
        .profiles
        .get("public_bundle")
        .expect("frontend bundle profile should be loaded");

    assert_eq!(profile.format, Some(ExportProfileFormat::Epub));
    assert_eq!(
        profile.query.as_deref(),
        Some(r#"from notes where file.path starts_with "Guides/""#)
    );
    assert_eq!(
        profile.path.as_ref(),
        Some(&PathBuf::from("exports/team-book.epub"))
    );
    assert_eq!(profile.title.as_deref(), Some("Team Book"));
    assert_eq!(profile.author.as_deref(), Some("Vulcan"));
    assert_eq!(profile.toc, Some(ExportEpubTocStyleConfig::Flat));
    assert_eq!(profile.backlinks, Some(true));
    assert_eq!(profile.frontmatter, Some(true));
    assert_eq!(
        public_bundle.format,
        Some(ExportProfileFormat::FrontendBundle)
    );
    assert_eq!(
        public_bundle.path.as_ref(),
        Some(&PathBuf::from("exports/public-bundle"))
    );
    assert_eq!(public_bundle.site_profile.as_deref(), Some("public"));
    assert_eq!(
        profile.content_transform_rules.as_ref().map(|rules| {
            rules
                .iter()
                .map(|rule| {
                    (
                        rule.query.clone(),
                        rule.transforms.exclude_callouts.clone(),
                        rule.transforms.exclude_headings.clone(),
                        rule.transforms.exclude_frontmatter_keys.clone(),
                        rule.transforms.exclude_inline_fields.clone(),
                        rule.transforms.replace.clone(),
                    )
                })
                .collect::<Vec<_>>()
        }),
        Some(vec![(
            None,
            vec!["secret gm".to_string(), "internal".to_string()],
            vec!["Scratch".to_string()],
            vec!["email".to_string()],
            vec!["owner".to_string()],
            vec![
                ContentReplacementRuleConfig {
                    pattern: "[[People/Bob]]".to_string(),
                    replacement: "[[People/Alice]]".to_string(),
                    regex: false,
                },
                ContentReplacementRuleConfig {
                    pattern: "[A-Za-z0-9._%+-]+@example\\.com".to_string(),
                    replacement: "[redacted]".to_string(),
                    regex: true,
                },
            ],
        )])
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn local_config_can_override_export_profile_fields() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[export.profiles.team_book]
format = "epub"
query = 'from notes where file.path starts_with "Guides/"'
path = "exports/team-book.epub"
title = "Team Book"
toc = "tree"
backlinks = true

[[export.profiles.team_book.content_transforms]]
exclude_callouts = ["secret gm"]
exclude_headings = ["Scratch"]
exclude_frontmatter_keys = ["email"]
exclude_inline_fields = ["owner"]
[[export.profiles.team_book.content_transforms.replace]]
pattern = "secret"
replacement = "public"
"#,
    )
    .expect("shared config should be written");
    fs::write(
        vault_root.join(".vulcan/config.local.toml"),
        r#"
[export.profiles.team_book]
path = "local/team-book.epub"
frontmatter = true
toc = "flat"

[[export.profiles.team_book.content_transforms]]
query = 'from notes where file.path matches "^People/"'
exclude_callouts = ["internal", "private"]
exclude_headings = ["Directory"]
exclude_frontmatter_keys = ["phone"]
exclude_inline_fields = ["manager"]
[[export.profiles.team_book.content_transforms.replace]]
pattern = "\\b[A-Z0-9]{32}\\b"
replacement = "[token]"
regex = true

[export.profiles.graph_dump]
format = "graph"
path = "exports/graph.dot"
graph_format = "dot"

[export.profiles.public_bundle]
format = "frontend-bundle"
path = "exports/public-bundle"
site_profile = "public-local"
"#,
    )
    .expect("local config should be written");

    let loaded = load_vault_config(&VaultPaths::new(vault_root));
    let team_book = loaded
        .config
        .export
        .profiles
        .get("team_book")
        .expect("merged export profile should be loaded");
    let graph_dump = loaded
        .config
        .export
        .profiles
        .get("graph_dump")
        .expect("local export profile should be loaded");
    let public_bundle = loaded
        .config
        .export
        .profiles
        .get("public_bundle")
        .expect("bundle export profile should be loaded");

    assert_eq!(team_book.format, Some(ExportProfileFormat::Epub));
    assert_eq!(
        team_book.query.as_deref(),
        Some(r#"from notes where file.path starts_with "Guides/""#)
    );
    assert_eq!(
        team_book.path.as_ref(),
        Some(&PathBuf::from("local/team-book.epub"))
    );
    assert_eq!(team_book.title.as_deref(), Some("Team Book"));
    assert_eq!(team_book.toc, Some(ExportEpubTocStyleConfig::Flat));
    assert_eq!(team_book.backlinks, Some(true));
    assert_eq!(team_book.frontmatter, Some(true));
    assert_eq!(
        team_book.content_transform_rules.as_ref().map(|rules| {
            rules
                .iter()
                .map(|rule| {
                    (
                        rule.query.clone(),
                        rule.transforms.exclude_callouts.clone(),
                        rule.transforms.exclude_headings.clone(),
                        rule.transforms.exclude_frontmatter_keys.clone(),
                        rule.transforms.exclude_inline_fields.clone(),
                        rule.transforms.replace.clone(),
                    )
                })
                .collect::<Vec<_>>()
        }),
        Some(vec![(
            Some(r#"from notes where file.path matches "^People/""#.to_string()),
            vec!["internal".to_string(), "private".to_string()],
            vec!["Directory".to_string()],
            vec!["phone".to_string()],
            vec!["manager".to_string()],
            vec![ContentReplacementRuleConfig {
                pattern: "\\b[A-Z0-9]{32}\\b".to_string(),
                replacement: "[token]".to_string(),
                regex: true,
            }],
        )])
    );

    assert_eq!(graph_dump.format, Some(ExportProfileFormat::Graph));
    assert_eq!(
        graph_dump.path.as_ref(),
        Some(&PathBuf::from("exports/graph.dot"))
    );
    assert_eq!(graph_dump.graph_format, Some(ExportGraphFormatConfig::Dot));
    assert_eq!(
        public_bundle.format,
        Some(ExportProfileFormat::FrontendBundle)
    );
    assert_eq!(
        public_bundle.path.as_ref(),
        Some(&PathBuf::from("exports/public-bundle"))
    );
    assert_eq!(public_bundle.site_profile.as_deref(), Some("public-local"));
}

#[test]
fn missing_files_use_builtin_permission_profiles() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(temp_dir.path());

    let loaded = load_permission_profiles(&paths);

    assert!(loaded.diagnostics.is_empty());
    assert_eq!(
        loaded.profiles.get("readonly"),
        Some(&PermissionProfile::readonly())
    );
    assert_eq!(
        loaded.profiles.get("daily-wiki-agent"),
        Some(&PermissionProfile::daily_wiki_agent())
    );
    assert_eq!(
        loaded.profiles.get("unrestricted"),
        Some(&PermissionProfile::unrestricted())
    );
}

#[test]
fn permission_profiles_merge_shared_and_local_overrides() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    write_test_file(
        &vault_root.join(".vulcan/config.toml"),
        r#"
[permissions.profiles.agent]
read = "all"
write = "none"
network = "allow"
"#,
    );
    write_test_file(
        &vault_root.join(".vulcan/config.local.toml"),
        r#"
[permissions.profiles.agent]
write = "all"
git = "allow"
"#,
    );

    let loaded = load_permission_profiles(&VaultPaths::new(vault_root));

    assert!(loaded.diagnostics.is_empty());
    let agent = loaded
        .profiles
        .get("agent")
        .expect("custom profile should be loaded");
    assert_eq!(
        agent.read,
        PathPermissionConfig::Keyword(PathPermissionKeyword::All)
    );
    assert_eq!(
        agent.write,
        PathPermissionConfig::Keyword(PathPermissionKeyword::All)
    );
    assert_eq!(
        agent.network,
        NetworkPermissionConfig::Mode(PermissionMode::Allow)
    );
    assert_eq!(agent.git, PermissionMode::Allow);
}

#[test]
fn load_vault_config_with_overrides_merges_obsidian_defaults_and_in_memory_overrides() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    write_test_file(&vault_root.join(".obsidian/app.json"), OBSIDIAN_APP_JSON);
    let paths = VaultPaths::new(vault_root);
    let shared_override = r#"
[periodic.daily]
folder = "Journal/Working"
"#
    .parse::<TomlValue>()
    .expect("shared override should parse");
    let local_override = r#"
[periodic.daily]
template = "Templates/Local"
"#
    .parse::<TomlValue>()
    .expect("local override should parse");

    let loaded =
        load_vault_config_with_overrides(&paths, Some(&shared_override), Some(&local_override));
    let daily = loaded
        .config
        .periodic
        .note("daily")
        .expect("daily periodic config should exist");

    assert!(loaded.diagnostics.is_empty());
    assert_eq!(loaded.config.link_style, LinkStylePreference::Markdown);
    assert_eq!(daily.folder, PathBuf::from("Journal/Working"));
    assert_eq!(daily.template.as_deref(), Some("Templates/Local"));
}

#[test]
fn load_permission_profiles_with_overrides_reads_in_memory_profile_tables() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(temp_dir.path());
    let shared_override = r#"
[permissions.profiles.agent]
git = "allow"
"#
    .parse::<TomlValue>()
    .expect("shared override should parse");
    let local_override = r#"
[permissions.profiles.agent]
write = "all"
"#
    .parse::<TomlValue>()
    .expect("local override should parse");

    let loaded = load_permission_profiles_with_overrides(
        &paths,
        Some(&shared_override),
        Some(&local_override),
    );

    assert!(loaded.diagnostics.is_empty());
    let agent = loaded
        .profiles
        .get("agent")
        .expect("custom profile should be available");
    assert_eq!(agent.git, PermissionMode::Allow);
    assert_eq!(
        agent.write,
        PathPermissionConfig::Keyword(PathPermissionKeyword::All)
    );
}

#[test]
fn permission_profiles_parse_scoped_paths_and_network_domains() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    write_test_file(
        &vault_root.join(".vulcan/config.toml"),
        r#"
[permissions.profiles.agent]
read = { allow = ["folder:Projects/**"], deny = ["folder:Archive/**"] }
write = "none"
network = { allow = true, domains = ["api.tavily.com"] }
cpu_limit_ms = 5000
"#,
    );

    let loaded = load_permission_profiles(&VaultPaths::new(vault_root));

    assert!(loaded.diagnostics.is_empty());
    let agent = loaded
        .profiles
        .get("agent")
        .expect("custom profile should be loaded");
    assert!(matches!(agent.read, PathPermissionConfig::Rules(_)));
    assert_eq!(
        agent.network,
        NetworkPermissionConfig::Details(NetworkPermissionDetails {
            allow: true,
            domains: vec!["api.tavily.com".to_string()],
        })
    );
    assert_eq!(agent.cpu_limit_ms, PermissionLimit::Value(5000));
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        max_shrink_iters: 0,
        ..ProptestConfig::default()
    })]

    #[test]
    fn partial_permission_profile_overrides_replace_only_selected_fields(
        base in permission_profile_strategy(),
        overrides in partial_permission_profile_strategy(),
    ) {
        let mut profile = base.clone();
        let expected_policy_hook = overrides
            .policy_hook
            .clone()
            .map(|path| normalize_filesystem_pathbuf(&path).unwrap_or(path))
            .or_else(|| base.policy_hook.clone());

        let expected_read = overrides.read.clone().unwrap_or_else(|| base.read.clone());
        let expected_write = overrides.write.clone().unwrap_or_else(|| base.write.clone());
        let expected_refactor = overrides
            .refactor
            .clone()
            .unwrap_or_else(|| base.refactor.clone());
        let expected_git = overrides.git.clone().unwrap_or_else(|| base.git.clone());
        let expected_network = overrides
            .network
            .clone()
            .unwrap_or_else(|| base.network.clone());
        let expected_index = overrides.index.clone().unwrap_or_else(|| base.index.clone());
        let expected_config = overrides
            .config
            .clone()
            .unwrap_or_else(|| base.config.clone());
        let expected_execute = overrides
            .execute
            .clone()
            .unwrap_or_else(|| base.execute.clone());
        let expected_shell = overrides
            .shell
            .clone()
            .unwrap_or_else(|| base.shell.clone());
        let expected_cpu_limit_ms = overrides
            .cpu_limit_ms
            .clone()
            .unwrap_or_else(|| base.cpu_limit_ms.clone());
        let expected_memory_limit_mb = overrides
            .memory_limit_mb
            .clone()
            .unwrap_or_else(|| base.memory_limit_mb.clone());
        let expected_stack_limit_kb = overrides
            .stack_limit_kb
            .clone()
            .unwrap_or_else(|| base.stack_limit_kb.clone());

        apply_partial_permission_profile(&mut profile, overrides);

        prop_assert_eq!(profile.read, expected_read);
        prop_assert_eq!(profile.write, expected_write);
        prop_assert_eq!(profile.refactor, expected_refactor);
        prop_assert_eq!(profile.git, expected_git);
        prop_assert_eq!(profile.network, expected_network);
        prop_assert_eq!(profile.index, expected_index);
        prop_assert_eq!(profile.config, expected_config);
        prop_assert_eq!(profile.execute, expected_execute);
        prop_assert_eq!(profile.shell, expected_shell);
        prop_assert_eq!(profile.cpu_limit_ms, expected_cpu_limit_ms);
        prop_assert_eq!(profile.memory_limit_mb, expected_memory_limit_mb);
        prop_assert_eq!(profile.stack_limit_kb, expected_stack_limit_kb);
        prop_assert_eq!(profile.policy_hook, expected_policy_hook);
    }
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
fn tasks_default_source_accepts_legacy_file_alias() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[tasks]\ndefault_source = \"file\"\n",
    )
    .expect("config should be written");

    let loaded = load_vault_config(&VaultPaths::new(vault_root));

    assert!(loaded.diagnostics.is_empty());
    assert_eq!(
        loaded.config.tasks.default_source,
        TasksDefaultSource::Tasknotes
    );
}

#[test]
#[allow(clippy::too_many_lines)]
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
                "defaultReminders": [
                  {
                    "id": "rem-relative",
                    "type": "relative",
                    "relatedTo": "due",
                    "offset": 15,
                    "unit": "minutes",
                    "direction": "before",
                    "description": "Before due"
                  },
                  {
                    "id": "rem-absolute",
                    "type": "absolute",
                    "absoluteDate": "2026-04-10",
                    "absoluteTime": "09:00",
                    "description": "Morning review"
                  }
                ]
              },
              "calendarViewSettings": { "defaultView": "month" },
              "pomodoroWorkDuration": 25,
              "pomodoroShortBreakDuration": 7,
              "pomodoroLongBreakDuration": 20,
              "pomodoroLongBreakInterval": 3,
              "pomodoroStorageLocation": "daily-notes",
              "enableTaskLinkOverlay": true,
              "uiLanguage": "de",
              "icsIntegration": { "enabled": true },
              "savedViews": [{
                "id": "today",
                "name": "Today",
                "query": {
                  "type": "group",
                  "id": "root",
                  "conjunction": "and",
                  "children": [
                    {
                      "type": "condition",
                      "id": "status-1",
                      "property": "status",
                      "operator": "is",
                      "value": "blocked"
                    }
                  ],
                  "sortKey": "due",
                  "sortDirection": "asc"
                }
              }],
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
    assert_eq!(
        loaded
            .config
            .tasknotes
            .task_creation_defaults
            .default_reminders
            .len(),
        2
    );
    assert_eq!(
        loaded
            .config
            .tasknotes
            .task_creation_defaults
            .default_reminders[0]
            .id,
        "rem-relative"
    );
    assert_eq!(loaded.config.tasknotes.pomodoro.work_duration, 25);
    assert_eq!(loaded.config.tasknotes.pomodoro.short_break, 7);
    assert_eq!(loaded.config.tasknotes.pomodoro.long_break, 20);
    assert_eq!(loaded.config.tasknotes.pomodoro.long_break_interval, 3);
    assert_eq!(
        loaded.config.tasknotes.pomodoro.storage_location,
        TaskNotesPomodoroStorageLocation::DailyNote
    );
    assert_eq!(loaded.config.tasknotes.saved_views.len(), 1);
    assert_eq!(loaded.config.tasknotes.saved_views[0].id, "today");
    assert_eq!(loaded.config.tasknotes.saved_views[0].name, "Today");
    assert_eq!(
        loaded.config.tasknotes.saved_views[0]
            .query
            .sort_key
            .as_deref(),
        Some("due")
    );
}

#[test]
#[allow(clippy::too_many_lines)]
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
pomodoros = "focusSessions"

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

[tasknotes.pomodoro]
work_duration = 30
short_break = 6
long_break = 18
long_break_interval = 5
storage_location = "daily-note"

[tasknotes.task_creation_defaults]
default_contexts = ["@office"]
default_tags = ["work"]
default_projects = ["[[Projects/Alpha]]"]
default_time_estimate = 30
default_due_date = "today"
default_scheduled_date = "next-week"
default_recurrence = "monthly"

[[tasknotes.task_creation_defaults.default_reminders]]
id = "default-reminder"
type = "relative"
related_to = "scheduled"
offset = 2
unit = "hours"
direction = "before"
description = "Prep"
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
    assert_eq!(
        loaded.config.tasknotes.field_mapping.pomodoros,
        "focusSessions"
    );
    assert_eq!(loaded.config.tasknotes.statuses[0].auto_archive_delay, 30);
    assert_eq!(loaded.config.tasknotes.priorities[0].weight, 9);
    assert_eq!(loaded.config.tasknotes.user_fields[0].key, "effort");
    assert!(!loaded.config.tasknotes.enable_natural_language_input);
    assert!(loaded.config.tasknotes.nlp_default_to_scheduled);
    assert_eq!(loaded.config.tasknotes.nlp_language, "fr");
    assert_eq!(loaded.config.tasknotes.nlp_triggers.len(), 1);
    assert_eq!(loaded.config.tasknotes.nlp_triggers[0].trigger, "context:");
    assert_eq!(loaded.config.tasknotes.pomodoro.work_duration, 30);
    assert_eq!(loaded.config.tasknotes.pomodoro.short_break, 6);
    assert_eq!(loaded.config.tasknotes.pomodoro.long_break, 18);
    assert_eq!(loaded.config.tasknotes.pomodoro.long_break_interval, 5);
    assert_eq!(
        loaded.config.tasknotes.pomodoro.storage_location,
        TaskNotesPomodoroStorageLocation::DailyNote
    );
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
    assert_eq!(
        loaded
            .config
            .tasknotes
            .task_creation_defaults
            .default_reminders
            .len(),
        1
    );
    assert_eq!(
        loaded
            .config
            .tasknotes
            .task_creation_defaults
            .default_reminders[0]
            .id,
        "default-reminder"
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
fn create_default_config_requires_existing_vulcan_dir() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(temp_dir.path());

    let error = create_default_config(&paths).expect_err("missing .vulcan should fail");
    assert!(
        error.to_string().contains("Run `vulcan init`"),
        "expected actionable init guidance: {error}"
    );
    assert!(!paths.config_file().exists());
}

#[test]
fn create_default_config_is_idempotent() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(temp_dir.path());
    crate::initialize_vulcan_dir(&paths).expect(".vulcan dir should be created");

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
#[allow(clippy::too_many_lines)]
fn vulcan_config_loads_site_profiles() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[site.profiles.public]
title = "Public Notes"
page_title_template = "{site} :: {page}"
base_url = "https://notes.example.com"
deploy_path = "/garden"
output_dir = ".vulcan/site/public"
home = "Home"
language = "en"
theme = "default"
search = true
graph = false
backlinks = true
rss = true
favicon = "site/favicon.png"
logo = "site/logo.svg"
extra_css = ["site/public.css"]
extra_js = ["site/public.js"]
include_query = 'from notes where file.path starts_with "Garden/"'
include_paths = ["Home.md"]
include_folders = ["Docs/**"]
exclude_paths = ["Private.md"]
exclude_folders = ["Templates/**"]
exclude_tags = ["private", "draft"]
link_policy = "render_plain_text"
dataview_js = "static"
raw_html = "sanitize"

[site.profiles.public.shell]
reader_mode = true
default_palette = "dark"
left_rail = true
right_rail = true

[site.profiles.public.navigation]
explorer = true
folder_click = "collapse"
default_folder_state = "open"
use_saved_state = false
show_graph = false

[site.profiles.public.modules]
toc = true
graph = false
backlinks = true
outgoing_links = false

[site.profiles.public.asset_policy]
mode = "error_on_missing"
include_folders = ["site/shared/**"]

[[site.profiles.public.content_transforms]]
exclude_callouts = ["internal"]
exclude_headings = ["Scratch"]
"#,
    )
    .expect("config should be written");

    let loaded = load_vault_config(&VaultPaths::new(vault_root));
    let profile = loaded
        .config
        .site
        .profiles
        .get("public")
        .expect("site profile should be loaded");

    assert_eq!(profile.title.as_deref(), Some("Public Notes"));
    assert_eq!(
        profile.page_title_template.as_deref(),
        Some("{site} :: {page}")
    );
    assert_eq!(
        profile.base_url.as_deref(),
        Some("https://notes.example.com")
    );
    assert_eq!(profile.deploy_path.as_deref(), Some("/garden"));
    assert_eq!(
        profile.output_dir.as_ref(),
        Some(&PathBuf::from(".vulcan/site/public"))
    );
    assert_eq!(profile.home.as_deref(), Some("Home"));
    assert_eq!(profile.language.as_deref(), Some("en"));
    assert_eq!(profile.theme.as_deref(), Some("default"));
    assert_eq!(profile.search, Some(true));
    assert_eq!(profile.graph, Some(false));
    assert_eq!(profile.backlinks, Some(true));
    assert_eq!(profile.rss, Some(true));
    assert_eq!(profile.shell.reader_mode, Some(true));
    assert_eq!(
        profile.shell.default_palette,
        Some(SitePaletteModeConfig::Dark)
    );
    assert_eq!(profile.shell.left_rail, Some(true));
    assert_eq!(profile.shell.right_rail, Some(true));
    assert_eq!(profile.navigation.explorer, Some(true));
    assert_eq!(
        profile.navigation.folder_click,
        Some(SiteFolderClickBehaviorConfig::Collapse)
    );
    assert_eq!(
        profile.navigation.default_folder_state,
        Some(SiteExplorerFolderStateConfig::Open)
    );
    assert_eq!(profile.navigation.use_saved_state, Some(false));
    assert_eq!(profile.navigation.show_graph, Some(false));
    assert_eq!(profile.modules.toc, Some(true));
    assert_eq!(profile.modules.graph, Some(false));
    assert_eq!(profile.modules.backlinks, Some(true));
    assert_eq!(profile.modules.outgoing_links, Some(false));
    assert_eq!(
        profile.favicon.as_ref(),
        Some(&PathBuf::from("site/favicon.png"))
    );
    assert_eq!(profile.logo.as_ref(), Some(&PathBuf::from("site/logo.svg")));
    assert_eq!(profile.extra_css, vec![PathBuf::from("site/public.css")]);
    assert_eq!(profile.extra_js, vec![PathBuf::from("site/public.js")]);
    assert_eq!(
        profile.include_query.as_deref(),
        Some(r#"from notes where file.path starts_with "Garden/""#)
    );
    assert_eq!(profile.include_paths, vec!["Home.md".to_string()]);
    assert_eq!(profile.include_folders, vec!["Docs/**".to_string()]);
    assert_eq!(profile.exclude_paths, vec!["Private.md".to_string()]);
    assert_eq!(profile.exclude_folders, vec!["Templates/**".to_string()]);
    assert_eq!(
        profile.exclude_tags,
        vec!["private".to_string(), "draft".to_string()]
    );
    assert_eq!(
        profile.link_policy,
        Some(SiteLinkPolicyConfig::RenderPlainText)
    );
    assert_eq!(
        profile.dataview_js,
        Some(SiteDataviewJsPolicyConfig::Static)
    );
    assert_eq!(profile.raw_html, Some(SiteRawHtmlPolicyConfig::Sanitize));
    assert_eq!(
        profile.asset_policy.mode,
        SiteAssetPolicyModeConfig::ErrorOnMissing
    );
    assert_eq!(
        profile.asset_policy.include_folders,
        vec!["site/shared/**".to_string()]
    );
    assert_eq!(
        profile.content_transform_rules.as_ref().map(|rules| {
            rules
                .iter()
                .map(|rule| {
                    (
                        rule.transforms.exclude_callouts.clone(),
                        rule.transforms.exclude_headings.clone(),
                    )
                })
                .collect::<Vec<_>>()
        }),
        Some(vec![(
            vec!["internal".to_string()],
            vec!["Scratch".to_string()],
        )])
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn local_config_can_override_site_profile_fields() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"
[site.profiles.public]
title = "Public Notes"
page_title_template = "{page} | {site}"
output_dir = ".vulcan/site/public"
search = true
link_policy = "warn"
extra_css = ["site/public.css"]

[site.profiles.public.navigation]
explorer = true
show_home = true

[site.profiles.public.asset_policy]
mode = "copy_referenced"

[[site.profiles.public.content_transforms]]
exclude_callouts = ["internal"]
"#,
    )
    .expect("shared config should be written");
    fs::write(
        vault_root.join(".vulcan/config.local.toml"),
        r#"
[site.profiles.public]
page_title_template = "{site} :: {page} [{profile}]"
base_url = "https://preview.example.test"
deploy_path = "/preview"
output_dir = ".vulcan/site/preview"
graph = true
link_policy = "render_plain_text"
dataview_js = "static"
raw_html = "strip"
extra_css = ["site/local.css"]

[site.profiles.public.shell]
default_palette = "light"
reader_mode = false

[site.profiles.public.navigation]
folder_click = "collapse"
show_home = false

[site.profiles.public.modules]
graph = false
outgoing_links = false

[site.profiles.public.asset_policy]
mode = "error_on_missing"
include_folders = ["site/shared/**"]

[[site.profiles.public.content_transforms]]
exclude_headings = ["Scratch"]

[site.profiles.docs]
title = "Project Docs"
output_dir = ".vulcan/site/docs"
include_paths = ["Docs/Intro.md"]
"#,
    )
    .expect("local config should be written");

    let loaded = load_vault_config(&VaultPaths::new(vault_root));
    let public = loaded
        .config
        .site
        .profiles
        .get("public")
        .expect("merged site profile should be loaded");
    let docs = loaded
        .config
        .site
        .profiles
        .get("docs")
        .expect("local site profile should be loaded");

    assert_eq!(public.title.as_deref(), Some("Public Notes"));
    assert_eq!(
        public.page_title_template.as_deref(),
        Some("{site} :: {page} [{profile}]")
    );
    assert_eq!(
        public.base_url.as_deref(),
        Some("https://preview.example.test")
    );
    assert_eq!(public.deploy_path.as_deref(), Some("/preview"));
    assert_eq!(
        public.output_dir.as_ref(),
        Some(&PathBuf::from(".vulcan/site/preview"))
    );
    assert_eq!(public.search, Some(true));
    assert_eq!(public.graph, Some(true));
    assert_eq!(
        public.shell.default_palette,
        Some(SitePaletteModeConfig::Light)
    );
    assert_eq!(public.shell.reader_mode, Some(false));
    assert_eq!(public.navigation.explorer, Some(true));
    assert_eq!(
        public.navigation.folder_click,
        Some(SiteFolderClickBehaviorConfig::Collapse)
    );
    assert_eq!(public.navigation.show_home, Some(false));
    assert_eq!(public.modules.graph, Some(false));
    assert_eq!(public.modules.outgoing_links, Some(false));
    assert_eq!(
        public.link_policy,
        Some(SiteLinkPolicyConfig::RenderPlainText)
    );
    assert_eq!(public.dataview_js, Some(SiteDataviewJsPolicyConfig::Static));
    assert_eq!(public.raw_html, Some(SiteRawHtmlPolicyConfig::Strip));
    assert_eq!(public.extra_css, vec![PathBuf::from("site/local.css")]);
    assert_eq!(
        public.asset_policy.mode,
        SiteAssetPolicyModeConfig::ErrorOnMissing
    );
    assert_eq!(
        public.asset_policy.include_folders,
        vec!["site/shared/**".to_string()]
    );
    assert_eq!(
        public.content_transform_rules.as_ref().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        public
            .content_transform_rules
            .as_ref()
            .and_then(|rules| rules.first())
            .map(|rule| rule.transforms.exclude_headings.clone()),
        Some(vec!["Scratch".to_string()])
    );

    assert_eq!(docs.title.as_deref(), Some("Project Docs"));
    assert_eq!(
        docs.output_dir.as_ref(),
        Some(&PathBuf::from(".vulcan/site/docs"))
    );
    assert_eq!(docs.include_paths, vec!["Docs/Intro.md".to_string()]);
}

#[test]
fn default_config_template_documents_site_profiles() {
    let template = default_config_template();

    assert!(template.contains("[site.profiles.public]"));
    assert!(template.contains("page_title_template = \"{page} | {site}\""));
    assert!(template.contains("deploy_path = \"/wiki\""));
    assert!(template.contains("output_dir = \".vulcan/site/public\""));
    assert!(template.contains("[site.profiles.public.shell]"));
    assert!(template.contains("default_palette = \"system\""));
    assert!(template.contains("[site.profiles.public.navigation]"));
    assert!(template.contains("folder_click = \"link\""));
    assert!(template.contains("[site.profiles.public.modules]"));
    assert!(template.contains("outgoing_links = true"));
    assert!(template.contains("link_policy = \"warn\""));
    assert!(template.contains("dataview_js = \"off\""));
    assert!(template.contains("# raw_html = \"sanitize\""));
    assert!(template.contains("mode = \"copy_referenced\""));
}

#[test]
fn web_search_defaults_to_duckduckgo_without_api_key_env() {
    let config = WebSearchConfig::default();

    assert_eq!(config.backend, SearchBackendKind::Duckduckgo);
    assert_eq!(config.effective_api_key_env(), None);
    assert_eq!(
        config.effective_base_url(),
        "https://html.duckduckgo.com/html/"
    );
}

#[test]
fn default_config_template_documents_web_search_backends() {
    let template = default_config_template();

    assert!(template.contains("[web.search]"));
    assert!(template.contains("backend = \"duckduckgo\""));
    assert!(template.contains("backend = \"disabled\""));
    assert!(template.contains("KAGI_API_KEY"));
    assert!(template.contains("OLLAMA_API_KEY"));
    assert!(template.contains("https://html.duckduckgo.com/html/"));
}

#[test]
fn default_config_template_documents_permission_profiles() {
    let template = default_config_template();

    assert!(template.contains("[permissions.profiles.agent]"));
    assert!(template.contains("[permissions.profiles.daily-wiki-agent]"));
    assert!(template.contains("[permissions.profiles.readonly]"));
    assert!(template.contains("write = { allow = [\"folder:Projects/**\""));
    assert!(template.contains("write = \"all\""));
    assert!(template.contains("network = { allow = true, domains = ["));
    assert!(template.contains("policy_hook = \".vulcan/plugins/agent-policy.js\""));
}

#[test]
fn default_config_template_documents_assistant_folders() {
    let template = default_config_template();

    assert!(template.contains("[assistant]"));
    assert!(template.contains("prompts_folder = \"AI/Prompts\""));
    assert!(template.contains("skills_folder = \".agents/skills\""));
    assert!(!template.contains("tools_folder = \".agents/tools\""));
    assert!(!template.contains("pi_binary = \"pi\""));
    assert!(!template.contains("session_export = \"on_exit\""));
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

    let second_report = import_tasks_plugin_config(&paths).expect("second import should succeed");
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
        r#"{
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
            }"#,
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
#[allow(clippy::too_many_lines)]
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
        item.source == "choices[2] (Macro Choice)" && item.reason.contains("`vulcan run --script`")
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

    let second_report = import_kanban_plugin_config(&paths).expect("second import should succeed");
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
#[allow(clippy::too_many_lines)]
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
                "defaultRecurrence": "weekly",
                "defaultReminders": [
                  {
                    "id": "rem-relative",
                    "type": "relative",
                    "relatedTo": "due",
                    "offset": 15,
                    "unit": "minutes",
                    "direction": "before",
                    "description": "Before due"
                  }
                ]
              },
              "pomodoroWorkDuration": 25,
              "pomodoroShortBreakDuration": 5,
              "pomodoroLongBreakDuration": 15,
              "pomodoroLongBreakInterval": 4,
              "pomodoroStorageLocation": "daily-notes",
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
    assert!(report.mappings.iter().any(|mapping| {
        mapping.target == "tasknotes.pomodoro.storage_location"
            && mapping.value == Value::String("daily-note".to_string())
    }));
    assert!(report.mappings.iter().any(|mapping| {
        mapping.target == "tasknotes.task_creation_defaults.default_reminders"
            && mapping
                .value
                .as_array()
                .is_some_and(|reminders| reminders.len() == 1)
    }));
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
    assert!(rendered.contains("[tasknotes.pomodoro]"));
    assert!(rendered.contains("storage_location = \"daily-note\""));
    assert!(rendered.contains("[tasknotes.task_creation_defaults]"));
    assert!(rendered.contains("default_contexts"));
    assert!(rendered.contains("\"@office\""));
    assert!(rendered.contains("\"@home\""));
    assert!(rendered.contains("default_due_date = \"tomorrow\""));
    assert!(rendered.contains("default_recurrence = \"weekly\""));
    assert!(rendered.contains("[[tasknotes.task_creation_defaults.default_reminders]]"));
    assert!(rendered.contains("id = \"rem-relative\""));
    let migrated_tasks = fs::read_to_string(vault_root.join("TaskNotes/Views/tasks-default.base"))
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
        "pomodoroNotifications": true,
        "enableTaskLinkOverlay": true,
        "uiLanguage": "de",
        "icsIntegration": { "enabled": true },
        "savedViews": [{
          "id": "today",
          "name": "Today",
          "query": {
            "type": "group",
            "id": "root",
            "conjunction": "and",
            "children": []
          }
        }],
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
        item.reason == "advanced pomodoro automation settings are not yet supported"
    }));
    assert!(skipped
        .iter()
        .all(|item| item.source != "taskCreationDefaults.defaultReminders"));
    assert!(skipped.iter().any(|item| {
        item.reason == "Google Calendar integration settings are not yet supported"
    }));
    assert!(skipped.iter().any(|item| {
        item.reason == "Microsoft Calendar integration settings are not yet supported"
    }));
    assert!(skipped
        .iter()
        .all(|item| item.reason != "saved views are not yet supported"));
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
    fs::create_dir_all(vault_root.join("Views Source")).expect("view source dir should be created");
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
fn tasknotes_view_migration_trims_command_paths_before_normalizing() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path();
    fs::create_dir_all(vault_root.join("Views Source")).expect("view source dir should be created");
    fs::write(
        vault_root.join("Views Source/tasks-custom.base"),
        "views:\n  - type: tasknotesTaskList\n    name: Tasks\n",
    )
    .expect("source base should be written");

    let raw = serde_json::json!({
        "commandFileMapping": {
            "open-tasks-view": "  ./Views Source/tasks-custom  "
        }
    });

    let result = tasknotes_migrate_view_files(&VaultPaths::new(vault_root), &raw, true)
        .expect("view migration should succeed");

    assert!(result.skipped.is_empty());
    assert_eq!(result.migrated_files.len(), 1);
    assert_eq!(
        result.migrated_files[0].source,
        vault_root.join("Views Source/tasks-custom.base")
    );
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
            previous_contents: None,
            rendered_contents: None,
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
            previous_contents: None,
            rendered_contents: None,
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
    fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
    fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
