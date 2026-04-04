use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command as ProcessCommand;
use std::thread;
use tempfile::TempDir;
use vulcan_core::{CacheDatabase, VaultPaths};

fn run_git_ok(vault_root: &Path, args: &[&str]) {
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(vault_root)
        .args(args)
        .status()
        .expect("git should launch");
    assert!(status.success(), "git command failed: {args:?}");
}

fn init_git_repo(vault_root: &Path) {
    run_git_ok(vault_root, &["init"]);
    run_git_ok(vault_root, &["config", "user.name", "Vulcan Test"]);
    run_git_ok(vault_root, &["config", "user.email", "vulcan@example.com"]);
}

fn commit_all(vault_root: &Path, message: &str) {
    run_git_ok(vault_root, &["add", "."]);
    run_git_ok(vault_root, &["commit", "-m", message]);
}

#[test]
fn help_mentions_global_flags_and_core_commands() {
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    command.arg("--help").assert().success().stdout(
        predicate::str::contains("--vault <VAULT>")
            .and(predicate::str::contains("--output <OUTPUT>"))
            .and(predicate::str::contains("--refresh <REFRESH>"))
            .and(predicate::str::contains("--verbose"))
            .and(predicate::str::contains("index"))
            .and(predicate::str::contains("graph"))
            .and(predicate::str::contains("notes"))
            .and(predicate::str::contains("ls"))
            .and(predicate::str::contains("dataview"))
            .and(predicate::str::contains("tasks"))
            .and(predicate::str::contains("kanban"))
            .and(predicate::str::contains("browse"))
            .and(predicate::str::contains("note"))
            .and(predicate::str::contains("bases"))
            .and(predicate::str::contains("search"))
            .and(predicate::str::contains("vectors"))
            .and(predicate::str::contains("cluster"))
            .and(predicate::str::contains("related"))
            .and(predicate::str::contains("edit"))
            .and(predicate::str::contains("doctor"))
            .and(predicate::str::contains("cache"))
            .and(predicate::str::contains("refactor"))
            .and(predicate::str::contains("saved"))
            .and(predicate::str::contains("checkpoint"))
            .and(predicate::str::contains("changes"))
            .and(predicate::str::contains("diff"))
            .and(predicate::str::contains("daily"))
            .and(predicate::str::contains("weekly"))
            .and(predicate::str::contains("monthly"))
            .and(predicate::str::contains("periodic"))
            .and(predicate::str::contains("git"))
            .and(predicate::str::contains("web"))
            .and(predicate::str::contains("inbox"))
            .and(predicate::str::contains("template"))
            .and(predicate::str::contains("batch"))
            .and(predicate::str::contains("export"))
            .and(predicate::str::contains("config"))
            .and(predicate::str::contains("automation"))
            .and(predicate::str::contains("run"))
            .and(predicate::str::contains("help"))
            .and(predicate::str::contains("describe"))
            .and(predicate::str::contains("completions"))
            .and(predicate::str::contains("open"))
            .and(predicate::str::contains("Initialize, scan, rebuild, repair, watch, and serve index state"))
            .and(predicate::str::contains("Search indexed note content"))
            .and(predicate::str::contains(
                "Generate shell completion scripts",
            ))
            .and(predicate::str::contains("Command Groups:"))
            .and(predicate::str::contains("Indexing: index"))
            .and(predicate::str::contains(
                "Graph and Query: note, graph, search, notes, ls, browse, query, dataview, tasks, kanban, bases, diff",
            ))
            .and(predicate::str::contains(
                "Journaling: daily, weekly, monthly, periodic, inbox, template",
            ))
            .and(predicate::str::contains(
                "Semantic: vectors, cluster, related",
            ))
            .and(predicate::str::contains(
                "Reports and Automation: saved, checkpoint, changes, batch, export, automation",
            ))
            .and(predicate::str::contains(
                "Mutations: edit, update, unset, refactor",
            ))
            .and(predicate::str::contains(
                "Maintenance: doctor, cache, config, git, run, web, open, help, describe, completions",
            ))
            .and(predicate::str::contains("User guide: docs/cli.md"))
            .and(predicate::str::contains(
                "Interactive help: vulcan edit --help and vulcan browse --help",
            ))
            .and(predicate::str::contains("Machine-readable schema: vulcan describe"))
            .and(predicate::str::contains(
                "Override automatic cache refresh with --refresh <off|blocking|background>",
            )),
        );
}

#[test]
fn config_import_templater_json_output_reports_mappings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian/plugins/templater-obsidian"))
        .expect("templater plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/plugins/templater-obsidian/data.json"),
        r#"{
          "command_timeout": 12,
          "templates_folder": "Templater/Templates",
          "templates_pairs": [["slugify", "bun run slugify"]],
          "trigger_on_file_creation": true,
          "enable_system_commands": true,
          "user_scripts_folder": "Scripts/User",
          "startup_templates": ["Startup"],
          "intellisense_render": 4
        }"#,
    )
    .expect("templater plugin config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "templater",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], "templater");
    assert_eq!(
        json["mappings"][0]["target"],
        Value::String("templates.templater_folder".to_string())
    );
    assert_eq!(
        json["mappings"][0]["value"],
        Value::String("Templater/Templates".to_string())
    );

    let rendered =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(rendered.contains("[templates]"));
    assert!(rendered.contains("templater_folder = \"Templater/Templates\""));
    assert!(rendered.contains("command_timeout = 12"));
    assert!(rendered.contains("[[templates.templates_pairs]]"));
    assert!(rendered.contains("name = \"slugify\""));
    assert!(rendered.contains("enable_system_commands = true"));
    assert!(rendered.contains("user_scripts_folder = \"Scripts/User\""));
    assert!(rendered.contains("startup_templates = [\"Startup\"]"));
    assert!(rendered.contains("intellisense_render = 4"));
}

#[test]
fn config_import_core_json_output_reports_sources_and_target_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian")).expect("obsidian dir should be created");
    fs::write(
        vault_root.join(".obsidian/app.json"),
        r#"{
          "useMarkdownLinks": true,
          "newLinkFormat": "shortest",
          "attachmentFolderPath": "Assets",
          "strictLineBreaks": true
        }"#,
    )
    .expect("app config should be written");
    fs::write(
        vault_root.join(".obsidian/templates.json"),
        r#"{
          "dateFormat": "YYYY-MM-DD",
          "timeFormat": "HH:mm",
          "folder": "Templates"
        }"#,
    )
    .expect("templates config should be written");
    fs::write(
        vault_root.join(".obsidian/types.json"),
        r#"{
          "effort": {"type": "number"},
          "reviewed": {"type": "checkbox"}
        }"#,
    )
    .expect("types config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "core",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], "core");
    assert_eq!(json["dry_run"], false);
    assert_eq!(json["target_file"], ".vulcan/config.toml");
    assert!(json["source_paths"].as_array().is_some_and(|paths| {
        paths.iter().any(|path| path == ".obsidian/app.json")
            && paths.iter().any(|path| path == ".obsidian/templates.json")
            && paths.iter().any(|path| path == ".obsidian/types.json")
    }));
    assert!(json["mappings"].as_array().is_some_and(|mappings| mappings
        .iter()
        .any(|mapping| mapping["target"] == "templates.obsidian_folder"
            && mapping["value"] == "Templates")));
    assert!(json["mappings"].as_array().is_some_and(|mappings| mappings
        .iter()
        .any(|mapping| mapping["target"] == "property_types.reviewed"
            && mapping["value"] == "checkbox")));

    let config =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(config.contains("[links]"));
    assert!(config.contains("style = \"markdown\""));
    assert!(config.contains("resolution = \"shortest\""));
    assert!(config.contains("attachment_folder = \"Assets\""));
    assert!(config.contains("strict_line_breaks = true"));
    assert!(config.contains("[templates]"));
    assert!(config.contains("obsidian_folder = \"Templates\""));
    assert!(config.contains("[property_types]"));
    assert!(config.contains("effort = \"number\""));
    assert!(config.contains("reviewed = \"checkbox\""));
}

#[test]
fn config_import_dataview_json_output_reports_mappings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
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
          "timezone": "+02:00",
          "maxRecursiveRenderDepth": 7,
          "tableIdColumnName": "Document",
          "tableGroupColumnName": "Bucket"
        }"#,
    )
    .expect("dataview config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "dataview",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], "dataview");
    assert_eq!(json["dry_run"], false);
    assert_eq!(json["target_file"], ".vulcan/config.toml");
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| mapping["target"]
            == "dataview.inline_query_prefix"
            && mapping["value"] == "dv:")));

    let config =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(config.contains("[dataview]"));
    assert!(config.contains("inline_query_prefix = \"dv:\""));
    assert!(config.contains("enable_dataview_js = false"));
    assert!(config.contains("group_column_name = \"Bucket\""));
}

#[test]
fn config_import_list_json_output_reports_detectable_sources() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
        .expect("dataview plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/app.json"),
        r#"{"useMarkdownLinks": true}"#,
    )
    .expect("app config should be written");
    fs::write(
        vault_root.join(".obsidian/plugins/dataview/data.json"),
        r#"{"inlineQueryPrefix":"dv:"}"#,
    )
    .expect("dataview config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "--list",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    let importers = json["importers"]
        .as_array()
        .expect("importers should be an array");
    assert!(importers.iter().any(|item| {
        item["plugin"] == "core"
            && item["detected"] == true
            && item["source_paths"]
                .as_array()
                .is_some_and(|paths| paths.iter().any(|path| path == ".obsidian/app.json"))
    }));
    assert!(importers
        .iter()
        .any(|item| item["plugin"] == "dataview" && item["detected"] == true));
    assert!(importers
        .iter()
        .any(|item| item["plugin"] == "templater" && item["detected"] == false));
}

#[test]
fn config_import_all_dry_run_aggregates_detected_sources() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
        .expect("dataview plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/app.json"),
        r#"{
          "useMarkdownLinks": true,
          "newLinkFormat": "shortest"
        }"#,
    )
    .expect("app config should be written");
    fs::write(
        vault_root.join(".obsidian/plugins/dataview/data.json"),
        r#"{"inlineQueryPrefix":"dv:","tableGroupColumnName":"Bucket"}"#,
    )
    .expect("dataview config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "--all",
            "--dry-run",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["dry_run"], true);
    assert_eq!(json["detected_count"], 2);
    assert_eq!(json["imported_count"], 2);
    assert!(json["reports"]
        .as_array()
        .is_some_and(|reports| reports.iter().any(|report| report["plugin"] == "core")));
    assert!(json["reports"]
        .as_array()
        .is_some_and(|reports| reports.iter().any(|report| report["plugin"] == "dataview")));
    assert!(!vault_root.join(".vulcan/config.toml").exists());
}

#[test]
fn config_import_kanban_json_output_reports_mappings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian/plugins/obsidian-kanban"))
        .expect("kanban plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/plugins/obsidian-kanban/data.json"),
        r#"{
          "date-trigger": "DUE",
          "time-trigger": "AT",
          "date-format": "DD/MM/YYYY",
          "time-format": "HH:mm:ss",
          "date-display-format": "ddd DD MMM",
          "metadata-keys": [
            {
              "metadataKey": "status",
              "label": "Status",
              "shouldHideLabel": true,
              "containsMarkdown": true
            }
          ],
          "archive-with-date": true,
          "archive-date-separator": " :: ",
          "new-card-insertion-method": "prepend",
          "show-search": false
        }"#,
    )
    .expect("kanban plugin config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "kanban",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], "kanban");
    assert_eq!(json["created_config"], true);
    assert_eq!(json["updated"], true);
    assert!(json["mappings"].as_array().is_some_and(|mappings| mappings
        .iter()
        .any(|mapping| mapping["target"] == "kanban.date_trigger" && mapping["value"] == "DUE")));

    let config =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(config.contains("[kanban]"));
    assert!(config.contains("date_trigger = \"DUE\""));
    assert!(config.contains("date_display_format = \"ddd DD MMM\""));
    assert!(config.contains("[[kanban.metadata_keys]]"));
    assert!(config.contains("metadata_key = \"status\""));
    assert!(config.contains("should_hide_label = true"));
    assert!(config.contains("contains_markdown = true"));
    assert!(config.contains("archive_date_separator = \" :: \""));
    assert!(config.contains("show_search = false"));
}

#[test]
fn config_import_periodic_notes_json_output_reports_mappings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian/plugins/periodic-notes"))
        .expect("periodic plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/daily-notes.json"),
        r#"{
          "folder": "Journal/Daily",
          "format": "YYYY-MM-DD",
          "template": "Templates/Daily.md"
        }"#,
    )
    .expect("daily notes config should be written");
    fs::write(
        vault_root.join(".obsidian/plugins/periodic-notes/data.json"),
        r#"{
          "weekly": {
            "enabled": true,
            "folder": "Journal/Weekly",
            "format": "GGGG-[W]WW",
            "templatePath": "Templates/Weekly.md"
          },
          "monthly": {
            "enabled": true,
            "folder": "Journal/Monthly",
            "format": "YYYY-MM",
            "templatePath": "Templates/Monthly.md"
          }
        }"#,
    )
    .expect("periodic notes config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "periodic-notes",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], "periodic-notes");
    assert!(json["source_paths"].as_array().is_some_and(|paths| {
        paths
            .iter()
            .any(|path| path == ".obsidian/daily-notes.json")
            && paths
                .iter()
                .any(|path| path == ".obsidian/plugins/periodic-notes/data.json")
    }));
    assert!(json["mappings"].as_array().is_some_and(|mappings| mappings
        .iter()
        .any(|mapping| mapping["target"] == "periodic.daily.folder"
            && mapping["value"] == "Journal/Daily")));
    assert!(json["mappings"].as_array().is_some_and(|mappings| mappings
        .iter()
        .any(|mapping| mapping["target"] == "periodic.weekly.format"
            && mapping["value"] == "GGGG-[W]WW")));

    let rendered =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(rendered.contains("[periodic.daily]"));
    assert!(rendered.contains("folder = \"Journal/Daily\""));
    assert!(rendered.contains("template = \"Templates/Daily.md\""));
    assert!(rendered.contains("[periodic.weekly]"));
    assert!(rendered.contains("format = \"GGGG-[W]WW\""));
    assert!(rendered.contains("[periodic.monthly]"));
}

#[test]
fn daily_today_creates_note_from_template_and_updates_cache() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template dir should be created");
    fs::write(
        vault_root.join(".vulcan/templates/daily.md"),
        "# {{title}}\n\n## Log\n",
    )
    .expect("daily template should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "daily",
            "today",
            "--no-edit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let path = json["path"]
        .as_str()
        .expect("path should be present")
        .to_string();
    let rendered = fs::read_to_string(vault_root.join(&path))
        .expect("daily note should be created")
        .replace("\r\n", "\n");

    assert!(json["created"].as_bool().is_some_and(|created| created));
    assert!(path.starts_with("Journal/Daily/"));
    assert!(rendered.contains("## Log"));

    let database =
        CacheDatabase::open(&VaultPaths::new(&vault_root)).expect("database should open");
    assert!(document_paths(&database)
        .iter()
        .any(|document_path| document_path == &path));
}

#[test]
fn daily_append_creates_note_and_appends_under_heading() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "daily",
            "append",
            "Called Alice",
            "--heading",
            "## Log",
            "--date",
            "2026-04-03",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let path = json["path"]
        .as_str()
        .expect("path should be present")
        .to_string();
    let rendered = fs::read_to_string(vault_root.join(&path))
        .expect("daily note should be readable")
        .replace("\r\n", "\n");

    assert_eq!(path, "Journal/Daily/2026-04-03.md");
    assert!(json["created"].as_bool().is_some_and(|created| created));
    assert!(json["appended"].as_bool().is_some_and(|appended| appended));
    assert!(rendered.contains("## Log\n\nCalled Alice\n"));
}

#[test]
fn note_get_json_output_supports_composable_selectors() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "get",
            "Dashboard",
            "--heading",
            "Tasks",
            "--match",
            "TODO",
            "--context",
            "1",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["frontmatter"]["status"], "active");
    assert_eq!(
        json["content"],
        "Before\nTODO first\nContext after\n### Nested\nTODO nested\n"
    );
    assert_eq!(json["metadata"]["heading"], "Tasks");
    assert_eq!(json["metadata"]["match_pattern"], "TODO");
    assert_eq!(json["metadata"]["match_count"], 2);
    assert_eq!(json["metadata"]["line_spans"][0]["start_line"], 10);
    assert_eq!(json["metadata"]["line_spans"][0]["end_line"], 14);
}

#[test]
fn note_get_human_output_adds_line_numbers_unless_raw() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "note",
            "get",
            "Dashboard",
            "--match",
            "TODO",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("11: TODO first").and(predicate::str::contains("--")));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "note",
            "get",
            "Dashboard",
            "--match",
            "TODO",
            "--raw",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("TODO first")
                .and(predicate::str::contains("TODO nested"))
                .and(predicate::str::contains("11:").not())
                .and(predicate::str::contains("--").not()),
        );
}

#[test]
fn note_set_preserves_frontmatter_and_reports_check_diagnostics() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "set",
            "Dashboard",
            "--no-frontmatter",
            "--check",
        ])
        .write_stdin("Replacement line\n\n[[Missing]]\n")
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Dashboard.md"))
        .expect("Dashboard.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["preserved_frontmatter"], true);
    assert_eq!(json["checked"], true);
    assert!(json["diagnostics"]
        .as_array()
        .is_some_and(
            |diagnostics| diagnostics.iter().any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("Unresolved link target")))
        ));
    assert!(rendered.starts_with("---\nstatus: active\ntags:\n  - project\n---\n"));
    assert!(rendered.contains("Replacement line"));
    assert!(!rendered.contains("Intro line"));
}

#[test]
fn note_create_uses_template_and_frontmatter_bindings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template directory should be created");
    fs::write(
        vault_root.join(".vulcan/templates/brief.md"),
        concat!(
            "---\n",
            "status: draft\n",
            "tags:\n",
            "  - seed\n",
            "---\n",
            "# {{title}}\n",
            "\n",
            "Template body\n",
        ),
    )
    .expect("template should be written");
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "create",
            "Inbox/Idea",
            "--template",
            "brief",
            "--frontmatter",
            "reviewed=true",
        ])
        .write_stdin("Extra details\n")
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Inbox/Idea.md"))
        .expect("created note should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Inbox/Idea.md");
    assert_eq!(json["template"], "brief");
    assert_eq!(json["engine"], "native");
    assert!(rendered.contains("status: draft"));
    assert!(rendered.contains("reviewed: true"));
    assert!(rendered.contains("# Idea"));
    assert!(rendered.contains("Template body\n\nExtra details\n"));

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "note", "create", "Inbox/Idea"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn note_append_under_heading_reports_check_diagnostics() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "append",
            "Dashboard",
            "-",
            "--heading",
            "## Done",
            "--check",
        ])
        .write_stdin("[[Missing]]\n")
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Dashboard.md"))
        .expect("Dashboard.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["heading"], "## Done");
    assert!(json["diagnostics"]
        .as_array()
        .is_some_and(
            |diagnostics| diagnostics.iter().any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("Unresolved link target")))
        ));
    assert!(rendered.contains("## Done\n\n[[Missing]]\n\nFinished line"));
}

#[test]
fn note_append_prepend_renders_quickadd_value_tokens() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "append",
            "Dashboard",
            "- {{VALUE:title|case:slug}}",
            "--prepend",
            "--var",
            "title=Release Planning",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Dashboard.md"))
        .expect("Dashboard.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["mode"], "prepend");
    assert!(rendered.starts_with(
        "---\nstatus: active\ntags:\n  - project\n---\n- release-planning\n\n# Dashboard\n"
    ));
}

#[test]
fn note_append_periodic_creates_note_and_renders_quickadd_tokens() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "append",
            "- {{VALUE:title|case:slug}} due {{VDATE:due,YYYY-MM-DD}}",
            "--periodic",
            "daily",
            "--date",
            "2026-04-03",
            "--var",
            "title=Release Planning",
            "--var",
            "due=tomorrow",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Journal/Daily/2026-04-03.md"))
        .expect("daily note should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Journal/Daily/2026-04-03.md");
    assert_eq!(json["mode"], "append");
    assert_eq!(json["period_type"], "daily");
    assert_eq!(json["reference_date"], "2026-04-03");
    assert!(json["created"].as_bool().is_some_and(|created| created));
    assert!(rendered.contains("- release-planning due 2026-04-05\n"));
}

#[test]
fn note_append_uses_quickadd_global_variables_from_config() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    write_note_crud_sample(&vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[quickadd]
global_variables = { agenda = "- {{VALUE:title|case:slug}} due {{VDATE:due,YYYY-MM-DD}}" }
"#,
    )
    .expect("quickadd config should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "note",
            "append",
            "Dashboard",
            "{{GLOBAL_VAR:AGENDA}}",
            "--var",
            "title=Release Planning",
            "--var",
            "due=tomorrow",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Dashboard.md"))
        .expect("Dashboard.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Dashboard.md");
    assert_eq!(json["mode"], "append");
    assert!(rendered.contains("- release-planning due 2026-04-05\n"));
}

#[test]
fn note_patch_enforces_match_safety_and_supports_regex_dry_runs() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should be created");
    fs::write(
        vault_root.join("Patch.md"),
        "TODO 2026-04-03\nTODO 2026-05-01\n",
    )
    .expect("Patch.md should be written");
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "note",
            "patch",
            "Patch",
            "--find",
            "TODO",
            "--replace",
            "DONE",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("rerun with --all"));

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "note",
            "patch",
            "Patch",
            "--find",
            "/2026-\\d{2}-\\d{2}/",
            "--replace",
            "DATE",
            "--all",
            "--dry-run",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(vault_root.join("Patch.md"))
        .expect("Patch.md should be readable")
        .replace("\r\n", "\n");

    assert_eq!(json["path"], "Patch.md");
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["regex"], true);
    assert_eq!(json["match_count"], 2);
    assert_eq!(json["changes"][0]["before"], "2026-04-03");
    assert_eq!(json["changes"][0]["after"], "DATE");
    assert_eq!(rendered, "TODO 2026-04-03\nTODO 2026-05-01\n");
}

#[test]
fn daily_list_json_includes_events_in_range() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::create_dir_all(vault_root.join("Journal/Daily")).expect("daily dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[periodic.daily]\nschedule_heading = \"Schedule\"\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-03.md"),
        "# 2026-04-03\n\n## Schedule\n- 09:00 Team standup\n- 14:00-15:30 Dentist #personal\n",
    )
    .expect("first daily note should be written");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-04.md"),
        "# 2026-04-04\n\n## Schedule\n- all-day Company offsite\n",
    )
    .expect("second daily note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "daily",
            "list",
            "--from",
            "2026-04-03",
            "--to",
            "2026-04-04",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["date"], "2026-04-03");
    assert_eq!(rows[0]["event_count"], 2);
    assert!(rows[0]["events"].as_array().is_some_and(|events| {
        events.iter().any(|event| event["title"] == "Team standup")
            && events.iter().any(|event| event["title"] == "Dentist")
    }));
    assert_eq!(rows[1]["date"], "2026-04-04");
    assert_eq!(rows[1]["event_count"], 1);
    assert_eq!(rows[1]["events"][0]["start_time"], "all-day");
}

#[test]
fn daily_export_ics_writes_calendar_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::create_dir_all(vault_root.join("Journal/Daily")).expect("daily dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[periodic.daily]\nschedule_heading = \"Schedule\"\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-03.md"),
        "# 2026-04-03\n\n## Schedule\n- 09:00-10:00 Team standup @location(Zoom)\n- 14:00 Dentist #personal\n",
    )
    .expect("first daily note should be written");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-04.md"),
        "# 2026-04-04\n\n## Schedule\n- all-day Company offsite\n",
    )
    .expect("second daily note should be written");
    run_scan(&vault_root);

    let calendar_path = vault_root.join("exports/journal.ics");
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "daily",
            "export-ics",
            "--from",
            "2026-04-03",
            "--to",
            "2026-04-04",
            "--path",
            calendar_path
                .to_str()
                .expect("calendar path should be valid utf-8"),
            "--calendar-name",
            "Journal",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let rendered = fs::read_to_string(&calendar_path).expect("calendar file should be written");

    assert_eq!(json["from"], "2026-04-03");
    assert_eq!(json["to"], "2026-04-04");
    assert_eq!(json["calendar_name"], "Journal");
    assert_eq!(json["note_count"], 2);
    assert_eq!(json["event_count"], 3);
    assert_eq!(json["path"], calendar_path.to_string_lossy().as_ref());
    assert!(rendered.contains("BEGIN:VCALENDAR\r\n"));
    assert!(rendered.contains("SUMMARY:Team standup\r\n"));
    assert!(rendered.contains("LOCATION:Zoom\r\n"));
    assert!(rendered.contains("DTSTART;VALUE=DATE:20260404\r\n"));
}

#[test]
fn git_status_json_output_lists_only_vault_changes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Home.md"), "home\n").expect("home note should be written");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache\n").expect("cache should be written");
    commit_all(&vault_root, "Initial");

    fs::write(vault_root.join("Home.md"), "home updated\n").expect("home note should update");
    run_git_ok(&vault_root, &["add", "Home.md"]);
    fs::write(vault_root.join("Draft.md"), "draft\n").expect("draft note should be written");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache2\n").expect("cache should update");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "git",
            "status",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["clean"], false);
    assert_eq!(json["staged"], serde_json::json!(["Home.md"]));
    assert_eq!(json["unstaged"], serde_json::json!([]));
    assert_eq!(json["untracked"], serde_json::json!(["Draft.md"]));
}

#[test]
fn git_log_json_output_lists_recent_commits() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Home.md"), "home\n").expect("home note should be written");
    commit_all(&vault_root, "Add home");
    fs::write(vault_root.join("Other.md"), "other\n").expect("other note should be written");
    commit_all(&vault_root, "Add other");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "git",
            "log",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["limit"], 2);
    assert_eq!(json["entries"][0]["summary"], "Add other");
    assert_eq!(json["entries"][1]["summary"], "Add home");
}

#[test]
fn git_diff_json_output_reports_changed_paths_and_filters_internal_state() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Home.md"), "home\n").expect("home note should be written");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache\n").expect("cache should be written");
    commit_all(&vault_root, "Initial");

    fs::write(vault_root.join("Home.md"), "home updated\n").expect("home note should update");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache2\n").expect("cache should update");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "git",
            "diff",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], Value::Null);
    assert_eq!(json["changed_paths"], serde_json::json!(["Home.md"]));
    assert!(json["diff"]
        .as_str()
        .is_some_and(|diff| diff.contains("Home.md") && !diff.contains(".vulcan/cache.db")));
}

#[test]
fn git_commit_json_output_stages_vault_files_but_skips_internal_state() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Home.md"), "home\n").expect("home note should be written");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache\n").expect("cache should be written");
    commit_all(&vault_root, "Initial");

    fs::write(vault_root.join("Home.md"), "home updated\n").expect("home note should update");
    fs::write(vault_root.join(".vulcan/cache.db"), "cache2\n").expect("cache should update");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "git",
            "commit",
            "-m",
            "Update home",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(&vault_root)
        .args(["status", "--short"])
        .output()
        .expect("git status should launch");

    assert_eq!(json["committed"], true);
    assert_eq!(json["message"], "Update home");
    assert_eq!(json["files"], serde_json::json!(["Home.md"]));
    assert!(json["sha"].as_str().is_some_and(|sha| !sha.is_empty()));
    let rendered_status =
        String::from_utf8(status.stdout).expect("git status output should be valid utf-8");
    assert!(rendered_status.contains(".vulcan/cache.db"));
    assert!(!rendered_status.contains("Home.md"));
}

#[test]
fn git_blame_json_output_returns_line_metadata() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    init_git_repo(&vault_root);
    fs::write(vault_root.join("Home.md"), "alpha\nbeta\n").expect("home note should be written");
    commit_all(&vault_root, "Initial");
    fs::write(vault_root.join("Home.md"), "alpha\nbeta updated\n")
        .expect("home note should update");
    run_git_ok(&vault_root, &["add", "Home.md"]);
    run_git_ok(&vault_root, &["commit", "-m", "Update beta"]);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "git",
            "blame",
            "Home.md",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], "Home.md");
    assert_eq!(json["lines"][0]["line_number"], 1);
    assert_eq!(json["lines"][0]["line"], "alpha");
    assert_eq!(json["lines"][1]["line_number"], 2);
    assert_eq!(json["lines"][1]["line"], "beta updated");
    assert_eq!(json["lines"][1]["summary"], "Update beta");
    assert_eq!(json["lines"][1]["author_name"], "Vulcan Test");
}

#[test]
fn git_help_documents_sandboxed_operations() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["git", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("status")
                .and(predicate::str::contains("log"))
                .and(predicate::str::contains("diff"))
                .and(predicate::str::contains("commit"))
                .and(predicate::str::contains("blame"))
                .and(predicate::str::contains("`.vulcan/`")),
        );
}

#[test]
fn web_search_json_output_uses_configured_backend_and_env_key() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[web.search]\nbackend = \"kagi\"\napi_key_env = \"TEST_KAGI_TOKEN\"\nbase_url = \"{}\"\n",
            server.url("/api/v0/search")
        ),
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("TEST_KAGI_TOKEN", "test-token")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "search",
            "release notes",
            "--limit",
            "2",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["backend"], "kagi");
    assert_eq!(json["query"], "release notes");
    assert_eq!(json["results"][0]["title"], "Release Notes");
    assert_eq!(json["results"][1]["url"], "https://example.com/status");
}

#[test]
fn web_fetch_markdown_json_output_extracts_article_content() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "fetch",
            &server.url("/article"),
            "--mode",
            "markdown",
            "--extract-article",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();

    assert_eq!(json["status"], 200);
    assert_eq!(json["content_type"], "text/html");
    assert_eq!(json["mode"], "markdown");
    assert!(json["content"]
        .as_str()
        .is_some_and(|content| content.contains("Release Summary")
            && content.contains("Shipped & stable.")
            && !content.contains("skip me")));
}

#[test]
fn web_fetch_raw_save_writes_response_body() {
    let server = MockWebServer::spawn();
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    let output_path = temp_dir.path().join("page.bin");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "web",
            "fetch",
            &server.url("/raw"),
            "--mode",
            "raw",
            "--save",
            output_path
                .to_str()
                .expect("output path should be valid utf-8"),
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    server.shutdown();
    let rendered = fs::read(&output_path).expect("saved output should be readable");

    assert_eq!(json["status"], 200);
    assert_eq!(json["saved"], output_path.to_string_lossy().as_ref());
    assert_eq!(rendered, b"raw-body");
}

#[test]
fn web_help_documents_modes_and_backends() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["web", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("search")
                .and(predicate::str::contains("fetch"))
                .and(predicate::str::contains("robots.txt"))
                .and(predicate::str::contains("[web.search]")),
        );
}

#[test]
fn periodic_list_and_gaps_report_expected_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Journal/Daily")).expect("daily dir should be created");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-01.md"),
        "# 2026-04-01\n",
    )
    .expect("first daily note should be written");
    fs::write(
        vault_root.join("Journal/Daily/2026-04-03.md"),
        "# 2026-04-03\n",
    )
    .expect("second daily note should be written");
    run_scan(&vault_root);

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "periodic",
            "list",
            "--type",
            "daily",
        ])
        .assert()
        .success();
    let list_rows = parse_stdout_json_lines(&list_assert);
    assert_eq!(list_rows.len(), 2);
    assert_eq!(list_rows[0]["path"], "Journal/Daily/2026-04-01.md");
    assert_eq!(list_rows[1]["path"], "Journal/Daily/2026-04-03.md");

    let gaps_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "periodic",
            "gaps",
            "--type",
            "daily",
            "--from",
            "2026-04-01",
            "--to",
            "2026-04-03",
        ])
        .assert()
        .success();
    let gap_rows = parse_stdout_json_lines(&gaps_assert);
    assert_eq!(gap_rows.len(), 1);
    assert_eq!(gap_rows[0]["date"], "2026-04-02");
    assert_eq!(gap_rows[0]["expected_path"], "Journal/Daily/2026-04-02.md");
}

#[test]
fn dataview_inline_json_output_evaluates_expressions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "inline",
            "Dashboard",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["file"], Value::String("Dashboard.md".to_string()));
    assert_eq!(json["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["results"][0]["expression"],
        Value::String("this.status".to_string())
    );
    assert_eq!(
        json["results"][0]["value"],
        Value::String("draft".to_string())
    );
    assert_eq!(json["results"][0]["error"], Value::Null);
}

#[test]
fn dataview_inline_json_output_reports_expression_errors() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(vault_root.join("Broken.md"), "`= (`\n").expect("note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "inline",
            "Broken",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["file"], Value::String("Broken.md".to_string()));
    assert_eq!(json["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["results"][0]["expression"],
        Value::String("(".to_string())
    );
    assert_eq!(json["results"][0]["value"], Value::Null);
    assert!(json["results"][0]["error"]
        .as_str()
        .is_some_and(|error| !error.is_empty()));
}

#[test]
fn dataview_query_json_output_evaluates_dql_strings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "query",
            r#"TABLE status, priority FROM "Projects" SORT file.name ASC"#,
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["query_type"], "table");
    assert_eq!(
        json["columns"],
        serde_json::json!(["File", "status", "priority"])
    );
    assert_eq!(json["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["rows"][0]["File"],
        Value::String("[[Projects/Alpha]]".to_string())
    );
    assert_eq!(
        json["rows"][0]["status"],
        Value::String("active".to_string())
    );
    assert_eq!(json["rows"][0]["priority"].as_f64(), Some(1.0));
    assert_eq!(
        json["rows"][1]["File"],
        Value::String("[[Projects/Beta]]".to_string())
    );
}

#[test]
fn dataview_query_json_output_surfaces_unsupported_dql_diagnostics() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "query",
            r#"TABLE status.slugify() AS slug, mystery(status) AS surprise FROM "Projects" SORT file.name ASC"#,
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["query_type"], "table");
    assert_eq!(json["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(json["rows"][0]["slug"], Value::Null);
    assert_eq!(json["rows"][0]["surprise"], Value::Null);
    assert!(json["diagnostics"]
        .as_array()
        .is_some_and(|diagnostics| diagnostics.len() >= 2));
    assert!(json["diagnostics"]
        .as_array()
        .is_some_and(
            |diagnostics| diagnostics.iter().any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("unknown method `slugify`")))
        ));
    assert!(json["diagnostics"]
        .as_array()
        .is_some_and(
            |diagnostics| diagnostics.iter().any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("unknown function `mystery`")))
        ));
}

#[test]
fn dataview_query_js_json_output_evaluates_snippets() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "query-js",
            r##"dv.list(dv.pages("#project").file.name.sort().array()); dv.execute('TABLE status FROM "Projects" SORT file.name ASC');"##,
            "--file",
            "Dashboard",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["outputs"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["outputs"][0],
        serde_json::json!({
            "kind": "list",
            "items": ["Alpha", "Beta", "Dashboard"]
        })
    );
    assert_eq!(json["outputs"][1]["kind"], "query");
    assert_eq!(json["outputs"][1]["result"]["query_type"], "table");
    assert_eq!(json["outputs"][1]["result"]["result_count"], 2);
    assert_eq!(
        json["value"]["query_type"],
        Value::String("table".to_string())
    );
}

#[test]
fn dataview_eval_json_output_evaluates_selected_block() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "eval",
            "Dashboard",
            "--block",
            "0",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["file"], Value::String("Dashboard.md".to_string()));
    assert_eq!(json["blocks"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["blocks"][0]["block_index"], Value::Number(0.into()));
    assert_eq!(
        json["blocks"][0]["language"],
        Value::String("dataview".to_string())
    );
    assert_eq!(json["blocks"][0]["error"], Value::Null);
    assert_eq!(json["blocks"][0]["result"]["engine"], "dql");
    assert_eq!(json["blocks"][0]["result"]["data"]["query_type"], "table");
    assert_eq!(
        json["blocks"][0]["result"]["data"]["columns"],
        serde_json::json!(["File", "status", "priority"])
    );
    assert_eq!(
        json["blocks"][0]["result"]["data"]["result_count"],
        Value::Number(2.into())
    );
    assert_eq!(
        json["blocks"][0]["result"]["data"]["rows"][0],
        serde_json::json!({
            "File": "[[Projects/Alpha]]",
            "status": "active",
            "priority": 1.0
        })
    );
    assert_eq!(
        json["blocks"][0]["result"]["data"]["rows"][1],
        serde_json::json!({
            "File": "[[Dashboard]]",
            "status": "draft",
            "priority": [2.0, 3.0]
        })
    );
}

#[test]
fn dataview_eval_json_output_defaults_to_all_indexed_blocks() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "dataview",
            "eval",
            "Dashboard",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["blocks"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["blocks"][0]["language"],
        Value::String("dataview".to_string())
    );
    assert_eq!(json["blocks"][0]["error"], Value::Null);
    assert_eq!(json["blocks"][0]["result"]["engine"], "dql");
    assert_eq!(
        json["blocks"][1]["language"],
        Value::String("dataviewjs".to_string())
    );
    assert_eq!(json["blocks"][1]["error"], Value::Null);
    assert_eq!(json["blocks"][1]["result"]["engine"], "js");
    assert_eq!(
        json["blocks"][1]["result"]["data"]["outputs"],
        serde_json::json!([
            {
                "kind": "table",
                "headers": ["Status"],
                "rows": [["draft"]]
            }
        ])
    );
}

#[test]
fn dataview_eval_human_output_keeps_empty_table_headers() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "eval",
            "Dashboard",
            "--block",
            "0",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");

    assert!(stdout.contains("File | status | priority"));
    assert!(stdout.contains("[[Projects/Alpha]] | active | 1"));
    assert!(stdout.contains("[[Dashboard]] | draft | [2.0,3.0]"));
    assert!(stdout.contains("2 result(s)"));
}

#[test]
fn dataview_eval_human_output_shows_unsupported_dql_diagnostics() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::write(
        vault_root.join("Unsupported.md"),
        concat!(
            "```dataview\n",
            "TABLE status.slugify() AS slug\n",
            "FROM \"Projects\"\n",
            "SORT file.name ASC\n",
            "```\n",
        ),
    )
    .expect("unsupported note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "eval",
            "Unsupported",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");

    assert!(stdout.contains("File | slug"));
    assert!(stdout.contains("[[Projects/Alpha]] | null"));
    assert!(stdout.contains("Diagnostics:"));
    assert!(stdout.contains("unknown method `slugify`"));
}

fn write_tasks_cli_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[tasks]\nglobal_filter = \"#task\"\nglobal_query = \"not done\"\nremove_global_filter = true\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Tasks.md"),
        concat!(
            "# Sprint\n\n",
            "- [ ] Write docs #task\n",
            "- [x] Ship release #task\n",
            "- [x] Archive misc #misc\n",
            "- [ ] Plan backlog #task\n",
        ),
    )
    .expect("tasks note should be written");
    fs::write(
        vault_root.join("Dashboard.md"),
        concat!(
            "```tasks\n",
            "done\n",
            "```\n\n",
            "```tasks\n",
            "path includes Tasks\n",
            "```\n",
        ),
    )
    .expect("dashboard note should be written");
}

fn write_tasks_dependency_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[tasks]\nglobal_filter = \"#task\"\nremove_global_filter = true\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Tasks.md"),
        concat!(
            "- [ ] Write docs #task 🆔 WRITE-1\n",
            "- [ ] Ship release #task 🆔 SHIP-1\n",
            "- [ ] Publish docs #task ⛔ SHIP-1\n",
            "- [ ] Prep launch #task ⛔ MISSING-1\n",
            "- [ ] Archive misc #misc ⛔ WRITE-1\n",
        ),
    )
    .expect("dependency note should be written");
}

fn write_tasks_recurrence_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[tasks]\nglobal_filter = \"#task\"\nremove_global_filter = true\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Recurring.md"),
        concat!(
            "- [ ] Review sprint #task ⏳ 2026-03-30 🔁 every 2 weeks\n",
            "- [ ] Close books #task ⏳ 2026-02-15 [repeat:: every month on the 15th]\n",
            "- [ ] Publish notes #task ⏳ 2026-03-26 [repeat:: RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TH]\n",
            "- [ ] Ignore misc #misc ⏳ 2026-03-30 🔁 every 2 weeks\n",
        ),
    )
    .expect("recurring note should be written");
}

fn write_tasks_import_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".obsidian/plugins/obsidian-tasks-plugin"))
        .expect("tasks plugin dir should exist");
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
    .expect("tasks plugin config should be written");
}

fn write_tasknotes_import_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join(".obsidian/plugins/tasknotes"))
        .expect("tasknotes plugin dir should exist");
    fs::create_dir_all(vault_root.join("Views Source"))
        .expect("tasknotes view source dir should exist");
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
          "taskTag": "task",
          "taskIdentificationMethod": "tag",
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
          "calendarViewSettings": { "defaultView": "month" },
          "pomodoroWorkDuration": 25,
          "pomodoroShortBreakDuration": 5,
          "pomodoroLongBreakDuration": 15,
          "pomodoroLongBreakInterval": 4,
          "pomodoroStorageLocation": "daily-notes",
          "pomodoroNotifications": true,
          "enableTaskLinkOverlay": true,
          "uiLanguage": "de",
          "icsIntegration": { "enabled": true },
          "savedViews": [{ "id": "today", "name": "Today" }],
          "enableAPI": true,
          "webhooks": [{ "url": "https://example.test/hook" }],
          "enableBases": true,
          "commandFileMapping": {
            "open-tasks-view": "Views Source/tasks-custom.base",
            "open-kanban-view": "Views Source/kanban-custom.base",
            "relationships": "Views Source/relationships-custom.base",
            "open-agenda-view": "Views Source/agenda-custom.base"
          },
          "enableGoogleCalendar": true,
          "googleOAuthClientId": "google-client",
          "enableMicrosoftCalendar": true,
          "microsoftOAuthClientId": "microsoft-client"
        }"##,
    )
    .expect("tasknotes plugin config should be written");
}

fn write_kanban_cli_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        "---\nstatus: active\nowner: Ops\n---\n# Alpha\n",
    )
    .expect("linked note should be written");
    fs::write(
        vault_root.join("Board.md"),
        concat!(
            "---\n",
            "kanban-plugin: board\n",
            "date-trigger: DUE\n",
            "time-trigger: AT\n",
            "---\n\n",
            "## Todo\n\n",
            "- Release DUE{2026-04-01} AT{09:30} #ship [[Projects/Alpha]] [priority:: high]\n",
            "- [/] Waiting on review [owner:: Ops]\n\n",
            "## Done\n\n",
            "- Shipped DUE{2026-04-03}\n",
        ),
    )
    .expect("board should be written");
}

fn write_kanban_archive_fixture(vault_root: &Path) {
    fs::write(
        vault_root.join("Board.md"),
        concat!(
            "---\n",
            "kanban-plugin: board\n",
            "---\n\n",
            "## Todo\n\n",
            "- Build release\n\n",
            "## Done\n\n",
            "- Shipped\n\n",
            "***\n\n",
            "## Archive\n\n",
            "- Old card\n",
        ),
    )
    .expect("board should be written");
}

#[test]
fn tasks_query_json_output_evaluates_tasks_dsl() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "query",
            "done",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(1.into()));
    assert_eq!(json["tasks"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["tasks"][0]["text"],
        Value::String("Ship release".to_string())
    );
    assert_eq!(json["tasks"][0]["tags"], Value::Array(Vec::new()));
}

#[test]
fn tasks_eval_json_output_evaluates_selected_block_with_defaults() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "eval",
            "Dashboard",
            "--block",
            "1",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["file"], Value::String("Dashboard.md".to_string()));
    assert_eq!(json["blocks"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["blocks"][0]["block_index"], Value::Number(1.into()));
    assert_eq!(
        json["blocks"][0]["source"],
        Value::String("path includes Tasks".to_string())
    );
    assert_eq!(
        json["blocks"][0]["effective_source"],
        Value::String("tag includes #task\nnot done\npath includes Tasks".to_string())
    );
    assert_eq!(
        json["blocks"][0]["result"]["result_count"],
        Value::Number(2.into())
    );
    assert_eq!(
        json["blocks"][0]["result"]["tasks"][0]["text"],
        Value::String("Write docs".to_string())
    );
    assert_eq!(
        json["blocks"][0]["result"]["tasks"][1]["text"],
        Value::String("Plan backlog".to_string())
    );
}

#[test]
fn tasks_list_json_output_accepts_tasks_dsl_filters() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--filter",
            "not done",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(2.into()));
    assert_eq!(json["tasks"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["tasks"][0]["text"],
        Value::String("Write docs".to_string())
    );
    assert_eq!(
        json["tasks"][1]["text"],
        Value::String("Plan backlog".to_string())
    );
}

#[test]
fn tasks_list_json_output_accepts_dataview_expression_filters() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--filter",
            "completed",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(1.into()));
    assert_eq!(
        json["tasks"][0]["text"],
        Value::String("Ship release".to_string())
    );
}

#[test]
fn tasks_next_json_output_lists_upcoming_recurring_instances() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_recurrence_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "next",
            "4",
            "--from",
            "2026-03-29",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["reference_date"],
        Value::String("2026-03-29".to_string())
    );
    assert_eq!(json["result_count"], Value::Number(4.into()));
    assert_eq!(json["occurrences"].as_array().map(Vec::len), Some(4));
    assert_eq!(
        json["occurrences"][0]["date"],
        Value::String("2026-03-30".to_string())
    );
    assert_eq!(
        json["occurrences"][0]["task"]["recurrenceRule"],
        Value::String("FREQ=WEEKLY;INTERVAL=2".to_string())
    );
    assert_eq!(
        json["occurrences"][1]["date"],
        Value::String("2026-04-09".to_string())
    );
    assert_eq!(
        json["occurrences"][1]["task"]["recurrenceRule"],
        Value::String("FREQ=WEEKLY;INTERVAL=2;BYDAY=TH".to_string())
    );
    assert_eq!(
        json["occurrences"][2]["date"],
        Value::String("2026-04-13".to_string())
    );
    assert_eq!(json["occurrences"][2]["sequence"], Value::Number(2.into()));
    assert_eq!(
        json["occurrences"][3]["date"],
        Value::String("2026-04-15".to_string())
    );
    assert_eq!(
        json["occurrences"][3]["task"]["recurrence"],
        Value::String("every month on the 15th".to_string())
    );
    assert_eq!(
        json["occurrences"][3]["task"]["recurrenceMonthDay"],
        Value::Number(15.into())
    );
}

#[test]
fn config_import_tasks_json_output_writes_config_and_reports_mapping() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_import_fixture(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "tasks",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], Value::String("tasks".to_string()));
    assert_eq!(json["created_config"], Value::Bool(true));
    assert_eq!(json["updated"], Value::Bool(true));
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| {
            mapping["target"] == "tasks.global_filter" && mapping["value"] == "#task"
        })));

    let rendered =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(rendered.contains("[tasks]"));
    assert!(rendered.contains("global_filter = \"#task\""));
    assert!(rendered.contains("global_query = \"not done\""));
    assert!(rendered.contains("remove_global_filter = true"));
    assert!(rendered.contains("[[tasks.statuses.definitions]]"));
    assert!(rendered.contains("name = \"Waiting\""));
}

#[test]
fn config_import_tasknotes_json_output_writes_config_and_reports_mapping() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    write_tasknotes_import_fixture(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "config",
            "import",
            "tasknotes",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["plugin"], Value::String("tasknotes".to_string()));
    assert_eq!(json["created_config"], Value::Bool(true));
    assert_eq!(json["updated"], Value::Bool(true));
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| {
            mapping["target"] == "tasknotes.tasks_folder" && mapping["value"] == "Tasks"
        })));
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| {
            mapping["target"] == "tasknotes.field_mapping.due" && mapping["value"] == "deadline"
        })));
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| {
            mapping["target"] == "tasknotes.pomodoro.storage_location"
                && mapping["value"] == "daily-note"
        })));
    assert!(json["mappings"]
        .as_array()
        .is_some_and(|mappings| mappings.iter().any(|mapping| {
            mapping["target"] == "tasknotes.task_creation_defaults.default_reminders"
                && mapping["value"]
                    .as_array()
                    .is_some_and(|reminders| reminders.len() == 1)
        })));
    assert!(json["skipped"]
        .as_array()
        .is_some_and(|skipped| skipped.iter().any(|item| {
            item["source"] == "calendarViewSettings"
                && item["reason"] == "calendar view settings are not yet supported"
        })));
    assert!(json["skipped"]
        .as_array()
        .is_some_and(|skipped| skipped.iter().any(|item| {
            item["reason"] == "advanced pomodoro automation settings are not yet supported"
        })));
    assert!(json["migrated_files"]
        .as_array()
        .is_some_and(|files| files.iter().any(|item| {
            item["source"] == "Views Source/tasks-custom.base"
                && item["target"] == "TaskNotes/Views/tasks-default.base"
                && item["action"] == "copy"
        })));
    assert!(json["migrated_files"]
        .as_array()
        .is_some_and(|files| files.iter().any(|item| {
            item["source"] == "Views Source/kanban-custom.base"
                && item["target"] == "TaskNotes/Views/kanban-default.base"
                && item["action"] == "copy"
        })));
    assert!(json["migrated_files"]
        .as_array()
        .is_some_and(|files| files.iter().any(|item| {
            item["source"] == "Views Source/relationships-custom.base"
                && item["target"] == "TaskNotes/Views/relationships.base"
                && item["action"] == "copy"
        })));
    assert!(json["skipped"]
        .as_array()
        .is_some_and(|skipped| skipped.iter().any(|item| {
            item["source"] == "commandFileMapping.open-agenda-view"
                && item["reason"].as_str().is_some_and(|reason| {
                    reason.contains("unsupported view types: tasknotesCalendar")
                })
        })));

    let rendered =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(rendered.contains("[tasknotes]"));
    assert!(rendered.contains("tasks_folder = \"Tasks\""));
    assert!(rendered.contains("archive_folder = \"Archive\""));
    assert!(rendered.contains("task_tag = \"task\""));
    assert!(rendered.contains("task_property_name = \"isTask\""));
    assert!(rendered.contains("[tasknotes.field_mapping]"));
    assert!(rendered.contains("due = \"deadline\""));
    assert!(rendered.contains("[[tasknotes.statuses]]"));
    assert!(rendered.contains("value = \"blocked\""));
    assert!(rendered.contains("[[tasknotes.priorities]]"));
    assert!(rendered.contains("value = \"urgent\""));
    assert!(rendered.contains("[[tasknotes.user_fields]]"));
    assert!(rendered.contains("displayName = \"Effort\""));
    assert!(rendered.contains("[tasknotes.pomodoro]"));
    assert!(rendered.contains("storage_location = \"daily-note\""));
    assert!(rendered.contains("[tasknotes.task_creation_defaults]"));
    assert!(rendered.contains("default_due_date = \"tomorrow\""));
    assert!(rendered.contains("[[tasknotes.task_creation_defaults.default_reminders]]"));
    assert!(rendered.contains("id = \"rem-relative\""));
    let migrated_tasks = fs::read_to_string(vault_root.join("TaskNotes/Views/tasks-default.base"))
        .expect("migrated task list base should exist");
    assert!(migrated_tasks.starts_with("source: tasknotes\n\n# All Tasks\n"));
    assert!(!vault_root
        .join("TaskNotes/Views/agenda-default.base")
        .exists());

    run_scan(&vault_root);
    let view_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "view",
            "show",
            "tasks-default",
        ])
        .assert()
        .success();
    let view_json = parse_stdout_json(&view_assert);
    assert_eq!(view_json["file"], "TaskNotes/Views/tasks-default.base");
    assert_eq!(view_json["views"][0]["view_type"], "tasknotesTaskList");
    assert!(view_json["views"][0]["rows"]
        .as_array()
        .is_some_and(|rows| rows
            .iter()
            .any(|row| row["document_path"] == "TaskNotes/Tasks/Write Docs.md")));
}

#[test]
fn tasks_blocked_json_output_lists_blockers() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_dependency_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "blocked",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["tasks"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["tasks"][0]["task"]["text"],
        Value::String("Publish docs ⛔ SHIP-1".to_string())
    );
    assert_eq!(
        json["tasks"][0]["blockers"][0]["blocker_id"],
        Value::String("SHIP-1".to_string())
    );
    assert_eq!(
        json["tasks"][0]["blockers"][0]["blocker_completed"],
        Value::Bool(false)
    );
    assert_eq!(
        json["tasks"][1]["task"]["text"],
        Value::String("Prep launch ⛔ MISSING-1".to_string())
    );
    assert_eq!(
        json["tasks"][1]["blockers"][0]["resolved"],
        Value::Bool(false)
    );
}

#[test]
fn tasks_graph_json_output_lists_nodes_and_edges() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_tasks_dependency_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "graph",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["nodes"].as_array().map(Vec::len), Some(4));
    assert_eq!(json["edges"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["edges"][0]["blocker_id"],
        Value::String("SHIP-1".to_string())
    );
    assert_eq!(json["edges"][0]["resolved"], Value::Bool(true));
    assert_eq!(
        json["edges"][1]["blocker_id"],
        Value::String("MISSING-1".to_string())
    );
    assert_eq!(json["edges"][1]["resolved"], Value::Bool(false));
}

#[test]
fn tasks_list_json_output_includes_tasknotes_file_tasks() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--filter",
            "status.type is in_progress",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(1.into()));
    assert_eq!(
        json["tasks"][0]["text"],
        Value::String("Write docs".to_string())
    );
    assert_eq!(
        json["tasks"][0]["id"],
        Value::String("[[TaskNotes/Tasks/Write Docs]]".to_string())
    );
}

#[test]
fn tasks_list_json_output_supports_source_filters_and_archived_toggle() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("Inbox.md"),
        concat!(
            "- [ ] Inline follow-up #ops 🗓️ 2026-04-09\n",
            "- [x] Inline shipped #ops\n"
        ),
    )
    .expect("inline task fixture should be written");
    fs::write(
        vault_root.join("TaskNotes/Tasks/Archived Flag.md"),
        concat!(
            "---\n",
            "title: \"Archived flag\"\n",
            "status: \"done\"\n",
            "priority: \"low\"\n",
            "tags: [\"task\", \"archived\"]\n",
            "---\n"
        ),
    )
    .expect("archived task fixture should be written");
    run_scan(&vault_root);

    let file_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "file",
            "--status",
            "in-progress",
            "--priority",
            "high",
            "--due-before",
            "2026-04-11",
            "--project",
            "[[Projects/Website]]",
            "--context",
            "@desk",
            "--sort-by",
            "due",
            "--group-by",
            "source",
        ])
        .assert()
        .success();
    let file_json = parse_stdout_json(&file_assert);

    assert_eq!(file_json["result_count"], Value::Number(1.into()));
    assert_eq!(
        file_json["tasks"][0]["text"],
        Value::String("Write docs".to_string())
    );
    assert_eq!(
        file_json["tasks"][0]["taskSource"],
        Value::String("file".to_string())
    );
    assert_eq!(
        file_json["groups"][0]["key"],
        Value::String("file".to_string())
    );

    let inline_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "inline",
        ])
        .assert()
        .success();
    let inline_json = parse_stdout_json(&inline_assert);

    assert_eq!(inline_json["result_count"], Value::Number(2.into()));
    assert!(inline_json["tasks"]
        .as_array()
        .expect("tasks should be an array")
        .iter()
        .all(|task| task["taskSource"] == Value::String("inline".to_string())));

    let archived_default_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "file",
        ])
        .assert()
        .success();
    let archived_default_json = parse_stdout_json(&archived_default_assert);
    assert_eq!(
        archived_default_json["result_count"],
        Value::Number(2.into())
    );

    let archived_all_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "file",
            "--include-archived",
        ])
        .assert()
        .success();
    let archived_all_json = parse_stdout_json(&archived_all_assert);
    assert_eq!(archived_all_json["result_count"], Value::Number(3.into()));
    assert!(archived_all_json["tasks"]
        .as_array()
        .expect("tasks should be an array")
        .iter()
        .any(|task| task["text"] == Value::String("Archived flag".to_string())));
}

#[test]
fn tasks_list_dql_filter_keeps_sort_and_group_options() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("Inbox.md"),
        concat!(
            "- [ ] Inline follow-up #ops 🗓️ 2026-04-09\n",
            "- [x] Inline shipped #ops\n"
        ),
    )
    .expect("inline task fixture should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--filter",
            "completed && taskSource = \"inline\"",
            "--sort-by",
            "source",
            "--group-by",
            "source",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["result_count"], Value::Number(1.into()));
    assert_eq!(
        json["tasks"][0]["text"],
        Value::String("Inline shipped #ops".to_string())
    );
    assert_eq!(json["groups"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["groups"][0]["field"],
        Value::String("source".to_string())
    );
    assert_eq!(
        json["groups"][0]["key"],
        Value::String("inline".to_string())
    );
}

#[test]
fn tasks_next_and_graph_json_output_support_tasknotes_recurrence_and_dependencies() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let next_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "next",
            "2",
            "--from",
            "2026-04-04",
        ])
        .assert()
        .success();
    let next_json = parse_stdout_json(&next_assert);

    assert_eq!(next_json["result_count"], Value::Number(2.into()));
    assert_eq!(
        next_json["occurrences"][0]["date"],
        Value::String("2026-04-10".to_string())
    );
    assert_eq!(
        next_json["occurrences"][0]["task"]["id"],
        Value::String("[[TaskNotes/Tasks/Write Docs]]".to_string())
    );

    let graph_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "graph",
        ])
        .assert()
        .success();
    let graph_json = parse_stdout_json(&graph_assert);

    assert_eq!(graph_json["nodes"].as_array().map(Vec::len), Some(2));
    assert_eq!(graph_json["edges"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        graph_json["edges"][0]["blocker_id"],
        Value::String("[[TaskNotes/Tasks/Prep Outline]]".to_string())
    );
    assert_eq!(
        graph_json["edges"][0]["relation_type"],
        Value::String("FINISHTOSTART".to_string())
    );
    assert_eq!(
        graph_json["edges"][0]["gap"],
        Value::String("P1D".to_string())
    );
    assert_eq!(graph_json["edges"][0]["resolved"], Value::Bool(true));
}

#[test]
fn tasks_view_list_json_output_reports_available_tasknotes_views() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    write_tasknotes_views_fixture(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "view",
            "list",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    let views = json["views"].as_array().expect("views should be an array");
    assert!(views.iter().any(|view| {
        view["file"] == "TaskNotes/Views/tasks-default.base"
            && view["view_name"] == "Tasks"
            && view["view_type"] == "tasknotesTaskList"
            && view["supported"] == true
    }));
    assert!(views.iter().any(|view| {
        view["file"] == "TaskNotes/Views/kanban-default.base"
            && view["view_name"] == "Kanban Board"
            && view["view_type"] == "tasknotesKanban"
            && view["supported"] == true
    }));
}

#[test]
fn tasks_view_list_json_output_includes_saved_view_aliases() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[tasknotes]

[[tasknotes.saved_views]]
id = "blocked"
name = "Blocked Tasks"

[tasknotes.saved_views.query]
type = "group"
id = "root"
conjunction = "and"
sortKey = "due"
sortDirection = "asc"

[[tasknotes.saved_views.query.children]]
type = "condition"
id = "status-filter"
property = "status"
operator = "is"
value = "in-progress"
"#,
    )
    .expect("config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "view",
            "list",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    let views = json["views"].as_array().expect("views should be an array");
    assert!(views.iter().any(|view| {
        view["file"] == "config.tasknotes.saved_views.blocked"
            && view["file_stem"] == "blocked"
            && view["view_name"] == "Blocked Tasks"
            && view["view_type"] == "tasknotesTaskList"
            && view["supported"] == true
    }));
}

#[test]
fn tasks_view_json_output_evaluates_named_tasknotes_views() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    write_tasknotes_views_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "view",
            "show",
            "Tasks",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["file"],
        Value::String("TaskNotes/Views/tasks-default.base".to_string())
    );
    assert_eq!(json["views"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["views"][0]["name"], Value::String("Tasks".to_string()));
    assert_eq!(
        json["views"][0]["view_type"],
        Value::String("tasknotesTaskList".to_string())
    );
    assert_eq!(json["views"][0]["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["views"][0]["rows"][0]["document_path"],
        Value::String("TaskNotes/Tasks/Prep Outline.md".to_string())
    );
    assert_eq!(
        json["views"][0]["rows"][1]["cells"]["efficiencyRatio"],
        Value::Number(67.into())
    );
}

#[test]
fn tasks_view_json_output_evaluates_saved_view_aliases() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[tasknotes]

[[tasknotes.saved_views]]
id = "blocked"
name = "Blocked Tasks"

[tasknotes.saved_views.query]
type = "group"
id = "root"
conjunction = "and"
sortKey = "due"
sortDirection = "asc"

[[tasknotes.saved_views.query.children]]
type = "condition"
id = "status-filter"
property = "status"
operator = "is"
value = "in-progress"
"#,
    )
    .expect("config should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "view",
            "show",
            "blocked",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["file"],
        Value::String("config.tasknotes.saved_views.blocked".to_string())
    );
    assert_eq!(json["views"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["views"][0]["name"],
        Value::String("Blocked Tasks".to_string())
    );
    assert_eq!(json["diagnostics"].as_array().map(Vec::len), Some(0));
    assert_eq!(json["views"][0]["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        json["views"][0]["rows"][0]["document_path"],
        Value::String("TaskNotes/Tasks/Write Docs.md".to_string())
    );
}

#[test]
fn tasks_show_json_output_reports_tasknote_details() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], "TaskNotes/Tasks/Write Docs.md");
    assert_eq!(json["title"], "Write docs");
    assert_eq!(json["status"], "in-progress");
    assert_eq!(json["status_type"], "IN_PROGRESS");
    assert_eq!(json["archived"], false);
    assert_eq!(json["priority"], "high");
    assert_eq!(json["due"], "2026-04-10");
    assert_eq!(json["contexts"], serde_json::json!(["@desk", "@work"]));
    assert_eq!(
        json["projects"],
        serde_json::json!(["[[Projects/Website]]"])
    );
    assert_eq!(json["blocked_by"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["reminders"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["time_entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["total_time_minutes"], serde_json::json!(60));
    assert_eq!(json["active_time_minutes"], serde_json::json!(0));
    assert_eq!(json["estimate_remaining_minutes"], serde_json::json!(30));
    assert_eq!(json["efficiency_ratio"], serde_json::json!(67));
    assert_eq!(json["custom_fields"]["effort"], serde_json::json!(3.0));
    assert_eq!(json["frontmatter"]["title"], "Write docs");
    assert_eq!(json["body"], "Write the docs body.\n");
}

#[test]
fn tasks_track_start_stop_and_status_json_output_manage_time_entries() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let start_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "track",
            "start",
            "Prep Outline",
            "--description",
            "Deep work",
            "--no-commit",
        ])
        .assert()
        .success();
    let start_json = parse_stdout_json(&start_assert);

    assert_eq!(start_json["action"], "start");
    assert_eq!(start_json["path"], "TaskNotes/Tasks/Prep Outline.md");
    assert_eq!(start_json["session"]["description"], "Deep work");
    assert_eq!(start_json["session"]["active"], true);

    let status_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "track",
            "status",
        ])
        .assert()
        .success();
    let status_json = parse_stdout_json(&status_assert);
    assert_eq!(status_json["total_active_sessions"], serde_json::json!(1));
    assert_eq!(
        status_json["active_sessions"][0]["path"],
        "TaskNotes/Tasks/Prep Outline.md"
    );

    let stop_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "track",
            "stop",
            "--no-commit",
        ])
        .assert()
        .success();
    let stop_json = parse_stdout_json(&stop_assert);

    assert_eq!(stop_json["action"], "stop");
    assert_eq!(stop_json["path"], "TaskNotes/Tasks/Prep Outline.md");
    assert_eq!(stop_json["session"]["active"], false);
    assert!(stop_json["session"]["end_time"].is_string());

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Prep Outline",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["time_entries"].as_array().map(Vec::len), Some(1));
    assert!(show_json["time_entries"][0]["endTime"].is_string());
}

#[test]
fn tasks_track_log_and_summary_json_output_report_totals() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let log_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "track",
            "log",
            "Write Docs",
        ])
        .assert()
        .success();
    let log_json = parse_stdout_json(&log_assert);
    assert_eq!(log_json["path"], "TaskNotes/Tasks/Write Docs.md");
    assert_eq!(log_json["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(log_json["total_time_minutes"], serde_json::json!(60));
    assert_eq!(
        log_json["estimate_remaining_minutes"],
        serde_json::json!(30)
    );
    assert_eq!(log_json["efficiency_ratio"], serde_json::json!(67));

    let summary_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "track",
            "summary",
            "--period",
            "all",
        ])
        .assert()
        .success();
    let summary_json = parse_stdout_json(&summary_assert);
    assert_eq!(summary_json["period"], "all");
    assert_eq!(summary_json["total_minutes"], serde_json::json!(60));
    assert_eq!(summary_json["tasks_with_time"], serde_json::json!(1));
    assert_eq!(
        summary_json["top_tasks"][0]["path"],
        "TaskNotes/Tasks/Write Docs.md"
    );
    assert_eq!(
        summary_json["top_projects"][0]["project"],
        "[[Projects/Website]]"
    );
}

#[test]
fn tasks_due_json_output_lists_due_tasknotes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("TaskNotes/Tasks/Old Task.md"),
        concat!(
            "---\n",
            "title: \"Old task\"\n",
            "status: \"open\"\n",
            "priority: \"low\"\n",
            "due: \"2000-01-01\"\n",
            "tags: [\"task\"]\n",
            "dateCreated: \"1999-12-31T08:00:00Z\"\n",
            "dateModified: \"1999-12-31T08:00:00Z\"\n",
            "---\n",
        ),
    )
    .expect("old task should be written");
    run_scan(&vault_root);

    let due_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "due",
            "--within",
            "2000y",
        ])
        .assert()
        .success();
    let due_json = parse_stdout_json(&due_assert);
    let tasks = due_json["tasks"]
        .as_array()
        .expect("tasks should be an array");

    assert!(tasks
        .iter()
        .any(|task| task["path"] == "TaskNotes/Tasks/Write Docs.md" && task["overdue"] == false));
    assert!(tasks
        .iter()
        .any(|task| task["path"] == "TaskNotes/Tasks/Old Task.md" && task["overdue"] == true));
}

#[test]
fn tasks_reminders_json_output_evaluates_relative_and_absolute_reminders() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("TaskNotes/Tasks/Future Reminder.md"),
        concat!(
            "---\n",
            "title: \"Future reminder\"\n",
            "status: \"open\"\n",
            "priority: \"normal\"\n",
            "tags: [\"task\"]\n",
            "reminders:\n",
            "  - id: \"abs-1\"\n",
            "    type: \"absolute\"\n",
            "    absoluteTime: \"2999-01-01T09:00:00Z\"\n",
            "    description: \"Far future\"\n",
            "dateCreated: \"2026-04-01T08:00:00Z\"\n",
            "dateModified: \"2026-04-01T08:00:00Z\"\n",
            "---\n",
        ),
    )
    .expect("future reminder task should be written");
    run_scan(&vault_root);

    let reminders_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "reminders",
            "--upcoming",
            "2000y",
        ])
        .assert()
        .success();
    let reminders_json = parse_stdout_json(&reminders_assert);
    let reminders = reminders_json["reminders"]
        .as_array()
        .expect("reminders should be an array");

    assert!(reminders.iter().any(|reminder| {
        reminder["path"] == "TaskNotes/Tasks/Write Docs.md"
            && reminder["reminder_id"] == "rem-1"
            && reminder["notify_at"] == "2026-04-09T23:45:00Z"
    }));
    assert!(reminders.iter().any(|reminder| {
        reminder["path"] == "TaskNotes/Tasks/Future Reminder.md"
            && reminder["reminder_id"] == "abs-1"
            && reminder["reminder_type"] == "absolute"
            && reminder["notify_at"] == "2999-01-01T09:00:00Z"
    }));
}

#[test]
fn tasks_set_json_output_updates_tasknote_frontmatter_and_rescans() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let set_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "set",
            "Write Docs",
            "due",
            "2026-04-12",
            "--no-commit",
        ])
        .assert()
        .success();
    let set_json = parse_stdout_json(&set_assert);

    assert_eq!(set_json["action"], "set");
    assert_eq!(set_json["path"], "TaskNotes/Tasks/Write Docs.md");

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["due"], "2026-04-12");
}

#[test]
fn tasks_complete_json_output_marks_non_recurring_task_done() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("TaskNotes/Tasks/Ship Release.md"),
        concat!(
            "---\n",
            "title: \"Ship release\"\n",
            "status: \"open\"\n",
            "priority: \"normal\"\n",
            "tags: [\"task\", \"release\"]\n",
            "dateCreated: \"2026-04-01T08:00:00Z\"\n",
            "dateModified: \"2026-04-01T08:00:00Z\"\n",
            "---\n",
            "\n",
            "Ship the release checklist.\n",
        ),
    )
    .expect("tasknote fixture should be written");
    run_scan(&vault_root);

    let complete_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "complete",
            "Ship Release",
            "--no-commit",
        ])
        .assert()
        .success();
    let complete_json = parse_stdout_json(&complete_assert);

    assert_eq!(complete_json["action"], "complete");
    assert_eq!(complete_json["path"], "TaskNotes/Tasks/Ship Release.md");

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Ship Release",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["status"], "done");
    assert_eq!(show_json["completed"], true);
    assert!(show_json["completed_date"]
        .as_str()
        .is_some_and(|value| value.len() == 10));
}

#[test]
fn tasks_complete_json_output_records_recurring_instance_completion() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let complete_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "complete",
            "Write Docs",
            "--date",
            "2026-04-10",
            "--no-commit",
        ])
        .assert()
        .success();
    let complete_json = parse_stdout_json(&complete_assert);

    assert_eq!(complete_json["action"], "complete");

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert!(show_json["complete_instances"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item == "2026-04-10")));

    let next_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "next",
            "2",
            "--from",
            "2026-04-04",
        ])
        .assert()
        .success();
    let next_json = parse_stdout_json(&next_assert);
    assert_eq!(next_json["occurrences"][0]["date"], "2026-04-17");
    assert_eq!(next_json["occurrences"][1]["date"], "2026-04-24");
}

#[test]
fn tasks_complete_json_output_marks_inline_tasks_done() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("Inbox.md"),
        "- [ ] Ship docs #ops\n- [ ] Follow up later\n",
    )
    .expect("inline task fixture should be written");
    run_scan(&vault_root);

    let complete_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "complete",
            "Ship docs #ops",
            "--date",
            "2026-04-09",
            "--no-commit",
        ])
        .assert()
        .success();
    let complete_json = parse_stdout_json(&complete_assert);

    assert_eq!(complete_json["action"], "complete");
    assert_eq!(complete_json["path"], "Inbox.md");
    assert_eq!(
        complete_json["changes"][0]["before"],
        Value::String("- [ ] Ship docs #ops".to_string())
    );
    assert_eq!(
        complete_json["changes"][0]["after"],
        Value::String("- [x] Ship docs #ops ✅ 2026-04-09".to_string())
    );

    let updated = fs::read_to_string(vault_root.join("Inbox.md"))
        .expect("updated inline task note should be readable");
    assert!(updated.contains("- [x] Ship docs #ops ✅ 2026-04-09"));

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "inline",
            "--filter",
            "status is done and description includes \"Ship docs\"",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);

    assert_eq!(list_json["result_count"], Value::Number(1.into()));
    assert_eq!(
        list_json["tasks"][0]["text"],
        Value::String("Ship docs #ops ✅ 2026-04-09".to_string())
    );
}

#[test]
fn tasks_convert_json_output_turns_existing_note_into_tasknote() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should be created");
    fs::write(
        vault_root.join("Notes/Idea.md"),
        concat!(
            "---\n",
            "owner: Alice\n",
            "tags:\n",
            "  - research\n",
            "---\n",
            "\n",
            "# Follow up\n",
            "\n",
            "Need task details.\n",
        ),
    )
    .expect("source note should be written");
    run_scan(&vault_root);

    let convert_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "convert",
            "Notes/Idea.md",
            "--no-commit",
        ])
        .assert()
        .success();
    let convert_json = parse_stdout_json(&convert_assert);

    assert_eq!(convert_json["action"], "convert");
    assert_eq!(convert_json["mode"], "note");
    assert_eq!(convert_json["source_path"], "Notes/Idea.md");
    assert_eq!(convert_json["target_path"], "Notes/Idea.md");
    assert_eq!(convert_json["title"], "Idea");
    assert_eq!(convert_json["created"], Value::Bool(false));
    assert_eq!(convert_json["frontmatter"]["owner"], "Alice");
    assert_eq!(convert_json["frontmatter"]["title"], "Idea");
    assert_eq!(convert_json["frontmatter"]["status"], "open");
    assert_eq!(convert_json["frontmatter"]["priority"], "normal");
    assert!(convert_json["frontmatter"]["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "research")));
    assert!(convert_json["frontmatter"]["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "task")));

    let updated =
        fs::read_to_string(vault_root.join("Notes/Idea.md")).expect("updated note should exist");
    assert!(updated.contains("owner: Alice"));
    assert!(updated.contains("title: Idea"));
    assert!(updated.contains("status: open"));
    assert!(updated.contains("priority: normal"));
    assert!(updated.contains("- research"));
    assert!(updated.contains("- task"));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Notes/Idea.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);

    assert_eq!(show_json["title"], "Idea");
    assert_eq!(show_json["status"], "open");
    assert_eq!(show_json["priority"], "normal");
    assert_eq!(show_json["body"], "# Follow up\n\nNeed task details.\n");
}

#[test]
fn tasks_convert_json_output_converts_checkbox_line_into_task_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should be created");
    fs::write(
        vault_root.join("Notes/Inbox.md"),
        concat!(
            "# Inbox\n",
            "\n",
            "- [ ] Ship docs due 2026-04-10 @desk #ops\n",
            "- [ ] Leave alone\n",
        ),
    )
    .expect("source note should be written");
    run_scan(&vault_root);

    let convert_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "convert",
            "Notes/Inbox.md",
            "--line",
            "3",
            "--no-commit",
        ])
        .assert()
        .success();
    let convert_json = parse_stdout_json(&convert_assert);

    assert_eq!(convert_json["action"], "convert");
    assert_eq!(convert_json["mode"], "line");
    assert_eq!(convert_json["source_path"], "Notes/Inbox.md");
    assert_eq!(convert_json["target_path"], "TaskNotes/Tasks/Ship docs.md");
    assert_eq!(convert_json["line_number"], Value::Number(3.into()));
    assert_eq!(convert_json["title"], "Ship docs");
    assert_eq!(convert_json["created"], Value::Bool(true));
    assert_eq!(
        convert_json["source_changes"][0]["before"],
        Value::String("- [ ] Ship docs due 2026-04-10 @desk #ops".to_string())
    );
    assert_eq!(
        convert_json["source_changes"][0]["after"],
        Value::String("- [[TaskNotes/Tasks/Ship docs]]".to_string())
    );
    assert_eq!(convert_json["frontmatter"]["due"], "2026-04-10");
    assert_eq!(
        convert_json["frontmatter"]["contexts"],
        serde_json::json!(["@desk"])
    );
    assert!(convert_json["frontmatter"]["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "ops")));
    assert!(convert_json["frontmatter"]["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "task")));

    let updated =
        fs::read_to_string(vault_root.join("Notes/Inbox.md")).expect("updated note should exist");
    assert!(updated.contains("- [[TaskNotes/Tasks/Ship docs]]"));
    assert!(updated.contains("- [ ] Leave alone"));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Tasks/Ship docs.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);

    assert_eq!(show_json["title"], "Ship docs");
    assert_eq!(show_json["due"], "2026-04-10");
    assert_eq!(show_json["contexts"], serde_json::json!(["@desk"]));
    assert_eq!(show_json["body"], "");
}

#[test]
fn tasks_convert_json_output_converts_heading_section_into_task_file() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should be created");
    fs::write(
        vault_root.join("Notes/Plan.md"),
        concat!(
            "# Plan\n",
            "\n",
            "## Ship release\n",
            "\n",
            "Coordinate docs.\n",
            "- [ ] Notify team\n",
            "\n",
            "## Later\n",
            "\n",
            "Other notes.\n",
        ),
    )
    .expect("source note should be written");
    run_scan(&vault_root);

    let convert_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "convert",
            "Notes/Plan.md",
            "--line",
            "3",
            "--no-commit",
        ])
        .assert()
        .success();
    let convert_json = parse_stdout_json(&convert_assert);

    assert_eq!(convert_json["mode"], "line");
    assert_eq!(
        convert_json["target_path"],
        "TaskNotes/Tasks/Ship release.md"
    );
    assert_eq!(convert_json["title"], "Ship release");
    assert!(convert_json["source_changes"][0]["before"]
        .as_str()
        .is_some_and(|before| before.contains("## Ship release")));
    assert_eq!(
        convert_json["source_changes"][0]["after"],
        Value::String("- [[TaskNotes/Tasks/Ship release]]".to_string())
    );
    assert_eq!(
        convert_json["body"],
        "Coordinate docs.\n- [ ] Notify team\n"
    );

    let updated =
        fs::read_to_string(vault_root.join("Notes/Plan.md")).expect("updated note should exist");
    assert!(updated.contains("- [[TaskNotes/Tasks/Ship release]]"));
    assert!(updated.contains("## Later"));
    assert!(!updated.contains("## Ship release"));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Tasks/Ship release.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);

    assert_eq!(show_json["title"], "Ship release");
    assert_eq!(show_json["body"], "Coordinate docs.\n- [ ] Notify team\n");
}

#[test]
fn tasks_create_json_output_appends_inline_task_to_default_inbox_note() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        concat!(
            "[tasks]\n",
            "global_filter = \"#task\"\n",
            "remove_global_filter = true\n",
            "set_created_date = true\n",
        ),
    )
    .expect("config should be written");

    let create_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "create",
            "Review release tomorrow @desk #ops high priority",
            "--no-commit",
        ])
        .assert()
        .success();
    let create_json = parse_stdout_json(&create_assert);

    assert_eq!(create_json["action"], "create");
    assert_eq!(create_json["path"], "Inbox.md");
    assert_eq!(create_json["task"], "Inbox.md:1");
    assert_eq!(create_json["created_note"], true);
    assert_eq!(create_json["used_nlp"], true);
    assert_eq!(create_json["due"], "2026-04-05");
    assert_eq!(create_json["priority"], "high");
    assert_eq!(create_json["contexts"], serde_json::json!(["@desk"]));
    assert_eq!(create_json["tags"], serde_json::json!(["ops", "task"]));

    let updated = fs::read_to_string(vault_root.join("Inbox.md")).expect("inbox note should exist");
    assert_eq!(
        updated,
        "- [ ] Review release @desk #ops #task 🗓️ 2026-04-05 ➕ 2026-04-04 🔺\n"
    );

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "inline",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);
    assert_eq!(list_json["result_count"], Value::Number(1.into()));
    assert_eq!(list_json["tasks"][0]["path"], "Inbox.md");
    assert_eq!(
        list_json["tasks"][0]["text"],
        "Review release @desk #ops 🗓️ 2026-04-05 ➕ 2026-04-04 🔺"
    );
}

#[test]
fn tasks_create_json_output_honors_explicit_target_and_flags() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let create_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "create",
            "Ship checklist",
            "--in",
            "Website",
            "--due",
            "2026-04-12",
            "--priority",
            "low",
            "--no-commit",
        ])
        .assert()
        .success();
    let create_json = parse_stdout_json(&create_assert);

    assert_eq!(create_json["path"], "Projects/Website.md");
    assert_eq!(create_json["task"], "Projects/Website.md:3");
    assert_eq!(create_json["created_note"], false);
    assert_eq!(create_json["due"], "2026-04-12");
    assert_eq!(create_json["priority"], "low");
    assert_eq!(create_json["line"], "- [ ] Ship checklist 🗓️ 2026-04-12 🔽");

    let updated = fs::read_to_string(vault_root.join("Projects/Website.md"))
        .expect("project note should exist");
    assert_eq!(
        updated,
        "# Website\n\n- [ ] Ship checklist 🗓️ 2026-04-12 🔽\n"
    );

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "inline",
            "--filter",
            "path includes Website",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);
    assert_eq!(list_json["result_count"], Value::Number(1.into()));
    assert_eq!(list_json["tasks"][0]["due"], "2026-04-12");
    assert_eq!(list_json["tasks"][0]["priority"], "low");
}

#[test]
fn tasks_reschedule_json_output_updates_tasknote_due_date() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let reschedule_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "reschedule",
            "Write Docs",
            "--due",
            "2026-04-12",
            "--no-commit",
        ])
        .assert()
        .success();
    let reschedule_json = parse_stdout_json(&reschedule_assert);

    assert_eq!(reschedule_json["action"], "reschedule");
    assert_eq!(reschedule_json["path"], "TaskNotes/Tasks/Write Docs.md");

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["due"], "2026-04-12");
}

#[test]
fn tasks_reschedule_json_output_replaces_inline_due_marker() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::write(
        vault_root.join("Inbox.md"),
        "- [ ] Review release #ops 🗓️ 2026-04-09\n",
    )
    .expect("inline task note should be written");
    run_scan(&vault_root);

    let reschedule_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "reschedule",
            "Inbox.md:1",
            "--due",
            "2026-04-11",
            "--no-commit",
        ])
        .assert()
        .success();
    let reschedule_json = parse_stdout_json(&reschedule_assert);

    assert_eq!(reschedule_json["action"], "reschedule");
    assert_eq!(reschedule_json["path"], "Inbox.md");
    assert_eq!(
        reschedule_json["changes"][0]["after"],
        "- [ ] Review release #ops 🗓️ 2026-04-11"
    );

    let updated =
        fs::read_to_string(vault_root.join("Inbox.md")).expect("updated note should exist");
    assert_eq!(updated, "- [ ] Review release #ops 🗓️ 2026-04-11\n");

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "inline",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);
    assert_eq!(list_json["result_count"], Value::Number(1.into()));
    assert_eq!(list_json["tasks"][0]["due"], "2026-04-11");
}

#[test]
fn tasks_archive_json_output_moves_completed_task_into_archive_folder() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);

    let archive_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "archive",
            "Prep Outline",
            "--no-commit",
        ])
        .assert()
        .success();
    let archive_json = parse_stdout_json(&archive_assert);

    assert_eq!(archive_json["action"], "archive");
    assert_eq!(
        archive_json["moved_from"],
        "TaskNotes/Tasks/Prep Outline.md"
    );
    assert_eq!(
        archive_json["moved_to"],
        "TaskNotes/Archive/Prep Outline.md"
    );
    assert!(!vault_root.join("TaskNotes/Tasks/Prep Outline.md").exists());
    assert!(vault_root
        .join("TaskNotes/Archive/Prep Outline.md")
        .exists());

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Archive/Prep Outline.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["archived"], true);
    assert!(show_json["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "archived")));
}

#[test]
fn task_commands_process_due_tasknotes_auto_archive() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        concat!(
            "[tasknotes]\n",
            "archive_folder = \"TaskNotes/Archive\"\n",
            "\n",
            "[[tasknotes.statuses]]\n",
            "id = \"open\"\n",
            "value = \"open\"\n",
            "label = \"Open\"\n",
            "color = \"#808080\"\n",
            "isCompleted = false\n",
            "order = 1\n",
            "autoArchive = false\n",
            "autoArchiveDelay = 5\n",
            "\n",
            "[[tasknotes.statuses]]\n",
            "id = \"done\"\n",
            "value = \"done\"\n",
            "label = \"Done\"\n",
            "color = \"#00aa00\"\n",
            "isCompleted = true\n",
            "order = 2\n",
            "autoArchive = true\n",
            "autoArchiveDelay = 5\n",
        ),
    )
    .expect("tasknotes config should be written");
    fs::write(
        vault_root.join("TaskNotes/Tasks/Old Done.md"),
        concat!(
            "---\n",
            "title: Old Done\n",
            "status: done\n",
            "priority: normal\n",
            "completedDate: 2026-04-03T09:00:00Z\n",
            "tags:\n",
            "  - task\n",
            "---\n",
            "\n",
            "Archived soon.\n",
        ),
    )
    .expect("completed task should be written");

    let list_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "list",
            "--source",
            "file",
        ])
        .assert()
        .success();
    let list_json = parse_stdout_json(&list_assert);

    assert!(!vault_root.join("TaskNotes/Tasks/Old Done.md").exists());
    assert!(vault_root.join("TaskNotes/Archive/Old Done.md").exists());
    assert!(list_json["tasks"].as_array().is_some_and(|tasks| tasks
        .iter()
        .all(|task| task["path"] != "TaskNotes/Tasks/Old Done.md")));
}

#[test]
fn tasks_edit_json_output_opens_editor_and_rescans_tasknote() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    run_scan(&vault_root);
    let editor = write_test_editor(
        temp_dir.path(),
        concat!(
            "---\n",
            "title: \"Write docs updated\"\n",
            "status: \"in-progress\"\n",
            "priority: \"high\"\n",
            "tags: [\"task\", \"docs\"]\n",
            "dateCreated: \"2026-04-02T08:00:00Z\"\n",
            "dateModified: \"2026-04-03T11:00:00Z\"\n",
            "---\n",
            "\n",
            "Updated task body.\n",
        ),
    );

    let edit_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("EDITOR", editor)
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "edit",
            "Write Docs",
            "--no-commit",
        ])
        .assert()
        .success();
    let edit_json = parse_stdout_json(&edit_assert);

    assert_eq!(edit_json["path"], "TaskNotes/Tasks/Write Docs.md");
    assert_eq!(edit_json["created"], false);
    assert_eq!(edit_json["rescanned"], true);

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "Write Docs",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["title"], "Write docs updated");
    assert_eq!(show_json["body"], "Updated task body.\n");
}

#[test]
fn tasks_add_json_output_creates_tasknote_from_natural_language() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);

    let add_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "add",
            "Buy groceries 2026-04-10 at 3pm @home #errands high priority",
            "--no-commit",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);

    assert_eq!(add_json["action"], "add");
    assert_eq!(add_json["used_nlp"], true);
    assert_eq!(add_json["path"], "TaskNotes/Tasks/Buy groceries.md");
    assert_eq!(add_json["title"], "Buy groceries");
    assert_eq!(add_json["priority"], "high");
    assert_eq!(add_json["due"], "2026-04-10T15:00:00");
    assert_eq!(add_json["contexts"], serde_json::json!(["@home"]));
    assert_eq!(add_json["tags"], serde_json::json!(["task", "errands"]));
    assert!(vault_root.join("TaskNotes/Tasks/Buy groceries.md").exists());

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Tasks/Buy groceries.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["title"], "Buy groceries");
    assert_eq!(show_json["due"], "2026-04-10T15:00:00");
    assert_eq!(show_json["contexts"], serde_json::json!(["@home"]));
    assert!(show_json["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag == "errands")));
}

#[test]
fn tasks_add_json_output_honors_explicit_flags_and_no_nlp() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);

    let add_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "add",
            "Literal @home #tag",
            "--no-nlp",
            "--status",
            "in-progress",
            "--priority",
            "high",
            "--due",
            "2026-04-12",
            "--scheduled",
            "2026-04-11",
            "--context",
            "@desk",
            "--project",
            "Projects/Website",
            "--tag",
            "docs",
            "--no-commit",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);

    assert_eq!(add_json["used_nlp"], false);
    assert_eq!(add_json["title"], "Literal @home #tag");
    assert_eq!(add_json["status"], "in-progress");
    assert_eq!(add_json["priority"], "high");
    assert_eq!(add_json["due"], "2026-04-12");
    assert_eq!(add_json["scheduled"], "2026-04-11");
    assert_eq!(add_json["contexts"], serde_json::json!(["@desk"]));
    assert_eq!(
        add_json["projects"],
        serde_json::json!(["[[Projects/Website]]"])
    );
    assert_eq!(add_json["tags"], serde_json::json!(["task", "docs"]));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Tasks/Literal @home #tag.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["title"], "Literal @home #tag");
    assert_eq!(show_json["status"], "in-progress");
    assert_eq!(show_json["due"], "2026-04-12");
    assert_eq!(show_json["contexts"], serde_json::json!(["@desk"]));
}

#[test]
fn tasks_add_json_output_applies_template_frontmatter_and_body() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[templates]\nobsidian_folder = \"Templates\"\n",
    )
    .expect("template config should be written");
    fs::create_dir_all(vault_root.join("Templates")).expect("templates dir should exist");
    fs::write(
        vault_root.join("Templates/Task.md"),
        concat!(
            "---\n",
            "owner: ops\n",
            "tags: [templated]\n",
            "---\n",
            "\n",
            "Template body.\n",
        ),
    )
    .expect("task template should be written");

    let add_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "add",
            "Template demo",
            "--template",
            "Task",
            "--no-commit",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);

    assert_eq!(add_json["template"], "Task");
    assert_eq!(add_json["frontmatter"]["owner"], "ops");
    assert_eq!(add_json["body"], "Template body.\n");
    assert_eq!(add_json["tags"], serde_json::json!(["task"]));

    let source = fs::read_to_string(vault_root.join("TaskNotes/Tasks/Template demo.md"))
        .expect("created task should exist");
    assert!(source.contains("owner: ops"));
    assert!(source.contains("Template body."));
}

#[test]
fn tasks_add_json_output_applies_default_reminders_from_config() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        concat!(
            "[tasknotes.task_creation_defaults]\n",
            "\n",
            "[[tasknotes.task_creation_defaults.default_reminders]]\n",
            "id = \"rem-relative\"\n",
            "type = \"relative\"\n",
            "related_to = \"due\"\n",
            "offset = 15\n",
            "unit = \"minutes\"\n",
            "direction = \"before\"\n",
            "description = \"Before due\"\n",
        ),
    )
    .expect("tasknotes config should be written");

    let add_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "add",
            "Prep launch tomorrow",
            "--no-commit",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);

    assert_eq!(add_json["due"], "2026-04-05");
    assert_eq!(
        add_json["frontmatter"]["reminders"][0]["id"],
        Value::String("rem-relative".to_string())
    );
    assert_eq!(
        add_json["frontmatter"]["reminders"][0]["offset"],
        Value::String("-PT15M".to_string())
    );

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "show",
            "TaskNotes/Tasks/Prep launch.md",
        ])
        .assert()
        .success();
    let show_json = parse_stdout_json(&show_assert);
    assert_eq!(show_json["reminders"][0]["relatedTo"], "due");
    assert_eq!(show_json["reminders"][0]["offset"], "-PT15M");
}

#[test]
fn tasks_add_json_output_respects_configured_nlp_language() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("tasknotes", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[tasknotes]\nnlp_language = \"de\"\n",
    )
    .expect("tasknotes config should be written");

    let add_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "tasks",
            "add",
            "Bericht morgen @arbeit dringend",
            "--no-commit",
        ])
        .assert()
        .success();
    let add_json = parse_stdout_json(&add_assert);

    assert_eq!(add_json["title"], "Bericht");
    assert_eq!(add_json["due"], "2026-04-05");
    assert_eq!(add_json["priority"], "high");
    assert_eq!(add_json["contexts"], serde_json::json!(["@arbeit"]));
}

#[test]
fn kanban_list_json_output_lists_indexed_boards() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "list",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(json_lines[0]["path"], Value::String("Board.md".to_string()));
    assert_eq!(json_lines[0]["title"], Value::String("Board".to_string()));
    assert_eq!(json_lines[0]["format"], Value::String("board".to_string()));
    assert_eq!(json_lines[0]["column_count"], Value::Number(2.into()));
    assert_eq!(json_lines[0]["card_count"], Value::Number(3.into()));
}

#[test]
fn kanban_show_json_output_returns_columns_and_verbose_cards() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--verbose",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], Value::String("Board.md".to_string()));
    assert_eq!(json["title"], Value::String("Board".to_string()));
    assert_eq!(json["date_trigger"], Value::String("DUE".to_string()));
    assert_eq!(json["time_trigger"], Value::String("AT".to_string()));
    assert_eq!(json["columns"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        json["columns"][0]["name"],
        Value::String("Todo".to_string())
    );
    assert_eq!(
        json["columns"][0]["cards"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        json["columns"][0]["cards"][0]["date"],
        Value::String("2026-04-01".to_string())
    );
    assert_eq!(
        json["columns"][0]["cards"][0]["time"],
        Value::String("09:30".to_string())
    );
    assert_eq!(
        json["columns"][0]["cards"][1]["task"]["status_type"],
        Value::String("IN_PROGRESS".to_string())
    );
}

#[test]
fn kanban_show_json_output_inherits_linked_page_metadata_for_wikilink_cards() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        concat!(
            "---\n",
            "status: active\n",
            "owner: Ops\n",
            "tags:\n",
            "  - client\n",
            "---\n\n",
            "# Alpha\n",
        ),
    )
    .expect("linked note should be written");
    fs::write(
        vault_root.join("Board.md"),
        concat!(
            "---\n",
            "kanban-plugin: board\n",
            "---\n\n",
            "## Todo\n\n",
            "- [[Projects/Alpha]]\n",
        ),
    )
    .expect("board should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--verbose",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["columns"][0]["cards"][0]["metadata"]["status"],
        Value::String("active".to_string())
    );
    assert_eq!(
        json["columns"][0]["cards"][0]["metadata"]["owner"],
        Value::String("Ops".to_string())
    );
    assert_eq!(
        json["columns"][0]["cards"][0]["metadata"]["file"]["path"],
        Value::String("Projects/Alpha.md".to_string())
    );
    assert!(json["columns"][0]["cards"][0]["metadata"]["file"]["tags"]
        .as_array()
        .is_some_and(|tags| tags.contains(&Value::String("client".to_string()))));
}

#[test]
fn kanban_show_json_output_includes_archive_when_requested() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_archive_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--include-archive",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["columns"].as_array().map(Vec::len), Some(3));
    assert_eq!(
        json["columns"][2]["name"],
        Value::String("Archive".to_string())
    );
    assert_eq!(
        json["columns"][2]["cards"][0]["text"],
        Value::String("Old card".to_string())
    );
}

#[test]
fn kanban_cards_json_output_filters_by_column_and_status() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_cli_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "cards",
            "Board",
            "--column",
            "Todo",
            "--status",
            "IN_PROGRESS",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(
        json_lines[0]["board_path"],
        Value::String("Board.md".to_string())
    );
    assert_eq!(
        json_lines[0]["column_filter"],
        Value::String("Todo".to_string())
    );
    assert_eq!(
        json_lines[0]["status_filter"],
        Value::String("IN_PROGRESS".to_string())
    );
    assert_eq!(json_lines[0]["column"], Value::String("Todo".to_string()));
    assert_eq!(
        json_lines[0]["text"],
        Value::String("Waiting on review [owner:: Ops]".to_string())
    );
    assert_eq!(
        json_lines[0]["task_status_type"],
        Value::String("IN_PROGRESS".to_string())
    );
    assert_eq!(
        json_lines[0]["inline_fields"]["owner"],
        Value::String("Ops".to_string())
    );
}

#[test]
fn kanban_archive_json_output_moves_cards_into_archive() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_archive_fixture(&vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "archive",
            "Board",
            "Build release",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], Value::String("Board.md".to_string()));
    assert_eq!(json["source_column"], Value::String("Todo".to_string()));
    assert_eq!(json["archive_column"], Value::String("Archive".to_string()));
    assert_eq!(
        json["card_text"],
        Value::String("Build release".to_string())
    );
    assert_eq!(json["created_archive_column"], Value::Bool(false));
    assert_eq!(json["dry_run"], Value::Bool(false));
    assert_eq!(json["rescanned"], Value::Bool(true));

    let source = fs::read_to_string(vault_root.join("Board.md")).expect("board should be readable");
    assert!(!source.contains("## Todo\n\n- Build release\n"));
    assert!(source.contains("## Archive\n\n- Old card\n- Build release\n"));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--include-archive",
        ])
        .assert()
        .success();
    let board = parse_stdout_json(&show_assert);

    assert_eq!(board["columns"].as_array().map(Vec::len), Some(3));
    assert_eq!(
        board["columns"][0]["cards"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        board["columns"][2]["cards"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn kanban_archive_dry_run_json_output_leaves_board_unchanged() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    write_kanban_archive_fixture(&vault_root);
    run_scan(&vault_root);

    let original =
        fs::read_to_string(vault_root.join("Board.md")).expect("board should be readable");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "archive",
            "Board",
            "Build release",
            "--dry-run",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["dry_run"], Value::Bool(true));
    assert_eq!(json["rescanned"], Value::Bool(false));

    let after = fs::read_to_string(vault_root.join("Board.md")).expect("board should be readable");
    assert_eq!(after, original);
}

#[test]
fn kanban_move_json_output_moves_cards_between_columns() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("Board.md"),
        concat!(
            "---\n",
            "kanban-plugin: board\n",
            "---\n\n",
            "## Todo\n\n",
            "- Build release\n\n",
            "## Doing\n\n",
            "- Review QA\n\n",
            "## Done\n\n",
            "- Shipped\n",
        ),
    )
    .expect("board should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "move",
            "Board",
            "Build release",
            "Done",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], Value::String("Board.md".to_string()));
    assert_eq!(json["source_column"], Value::String("Todo".to_string()));
    assert_eq!(json["target_column"], Value::String("Done".to_string()));
    assert_eq!(
        json["card_text"],
        Value::String("Build release".to_string())
    );
    assert_eq!(json["dry_run"], Value::Bool(false));
    assert_eq!(json["rescanned"], Value::Bool(true));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--verbose",
        ])
        .assert()
        .success();
    let board = parse_stdout_json(&show_assert);

    assert_eq!(
        board["columns"][0]["cards"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        board["columns"][2]["cards"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        board["columns"][2]["cards"][1]["text"],
        Value::String("Build release".to_string())
    );
}

#[test]
fn kanban_add_json_output_inserts_cards_using_column_ordering() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("Board.md"),
        concat!(
            "---\n",
            "kanban-plugin: board\n",
            "---\n\n",
            "## Todo\n\n",
            "- Existing card\n\n",
            "## Done\n\n",
            "- Shipped\n",
        ),
    )
    .expect("board should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "add",
            "Board",
            "Todo",
            "Build release",
            "--no-commit",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["path"], Value::String("Board.md".to_string()));
    assert_eq!(json["column"], Value::String("Todo".to_string()));
    assert_eq!(
        json["card_text"],
        Value::String("Build release".to_string())
    );
    assert_eq!(json["dry_run"], Value::Bool(false));
    assert_eq!(json["rescanned"], Value::Bool(true));

    let show_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "kanban",
            "show",
            "Board",
            "--verbose",
        ])
        .assert()
        .success();
    let board = parse_stdout_json(&show_assert);

    assert_eq!(
        board["columns"][0]["cards"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        board["columns"][0]["cards"][1]["text"],
        Value::String("Build release".to_string())
    );
}

#[test]
fn dataview_query_human_output_respects_display_result_count_setting() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[dataview]\ndisplay_result_count = false\n",
    )
    .expect("config should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "query",
            r#"TABLE status FROM "Projects" SORT file.name ASC"#,
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");

    assert!(stdout.contains("File | status"));
    assert!(stdout.contains("[[Projects/Alpha]] | active"));
    assert!(stdout.contains("[[Projects/Beta]] | backlog"));
    assert!(!stdout.contains("result(s)"));
}

#[test]
fn dataview_query_human_output_omits_empty_list_and_task_messages() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let empty_list = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "query",
            r#"LIST FROM "Projects" WHERE priority > 99"#,
        ])
        .assert()
        .success();
    let empty_list_stdout = String::from_utf8(empty_list.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");
    assert_eq!(empty_list_stdout, "");

    let empty_task = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "query",
            r#"TASK FROM "Projects" WHERE file.name = "Alpha" AND completed"#,
        ])
        .assert()
        .success();
    let empty_task_stdout = String::from_utf8(empty_task.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");
    assert_eq!(empty_task_stdout, "");
}

#[test]
fn dataview_plugin_display_settings_affect_human_output() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
        .expect("plugin dir should exist");
    fs::write(
        vault_root.join(".obsidian/plugins/dataview/data.json"),
        r#"{
          "displayResultCount": false,
          "primaryColumnName": "Document",
          "groupColumnName": "Bucket"
        }"#,
    )
    .expect("plugin settings should be written");
    run_scan(&vault_root);

    let table_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "query",
            r#"TABLE status FROM "Projects" SORT file.name ASC"#,
        ])
        .assert()
        .success();
    let table_stdout = String::from_utf8(table_assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");
    assert!(table_stdout.contains("Document | status"));
    assert!(!table_stdout.contains("result(s)"));

    let grouped_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "dataview",
            "query",
            r#"TABLE length(rows) AS count FROM "Projects" GROUP BY status SORT key ASC"#,
        ])
        .assert()
        .success();
    let grouped_stdout = String::from_utf8(grouped_assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8");
    assert!(grouped_stdout.contains("Bucket | count"));
    assert!(!grouped_stdout.contains("result(s)"));
}

#[test]
fn notes_json_output_includes_evaluated_inline_expressions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,inline_expressions",
            "notes",
            "--where",
            "status = draft",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(json_lines[0]["document_path"], "Dashboard.md");
    assert_eq!(
        json_lines[0]["inline_expressions"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        json_lines[0]["inline_expressions"][0]["expression"],
        Value::String("this.status".to_string())
    );
    assert_eq!(
        json_lines[0]["inline_expressions"][0]["value"],
        Value::String("draft".to_string())
    );
    assert_eq!(json_lines[0]["inline_expressions"][0]["error"], Value::Null);
}

#[test]
fn notes_help_documents_filter_syntax() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["notes", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Filter syntax:")
                .and(predicate::str::contains(
                    "Repeat --where to combine filters with AND.",
                ))
                .and(predicate::str::contains(
                    "file.path | file.name | file.ext | file.mtime",
                ))
                .and(predicate::str::contains(
                    "contains      list properties only",
                ))
                .and(predicate::str::contains(
                    "vulcan notes --where 'status = done'",
                )),
        );
}

#[test]
fn search_help_documents_query_and_filter_syntax() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Search query syntax:")
                .and(predicate::str::contains(
                    "plain terms are ANDed: dashboard status",
                ))
                .and(predicate::str::contains("tag:index"))
                .and(predicate::str::contains("[status:done]"))
                .and(predicate::str::contains("/\\d{4}-\\d{2}-\\d{2}/"))
                .and(predicate::str::contains("section:(dog cat)"))
                .and(predicate::str::contains("task:docs"))
                .and(predicate::str::contains("task-todo:followup"))
                .and(predicate::str::contains("ignore-case:Bob"))
                .and(predicate::str::contains("--match-case"))
                .and(predicate::str::contains("--sort <SORT>"))
                .and(predicate::str::contains("vulcan search Bob --match-case"))
                .and(predicate::str::contains(
                    "vulcan search dashboard --sort path-desc",
                ))
                .and(predicate::str::contains(
                    "Use --raw-query to pass SQLite FTS5 syntax through unchanged.",
                ))
                .and(predicate::str::contains("Filter syntax:"))
                .and(predicate::str::contains(
                    "vulcan search dashboard --where 'reviewed = true'",
                )),
        );
}

#[test]
fn browse_help_documents_modes_and_actions() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["browse", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Browse modes:")
                .and(predicate::str::contains("Ctrl-F"))
                .and(predicate::str::contains("Ctrl-T"))
                .and(predicate::str::contains("background"))
                .and(predicate::str::contains(
                    "Printable characters always extend the active query or prompt",
                ))
                .and(predicate::str::contains("Ctrl-S"))
                .and(predicate::str::contains("Alt-C"))
                .and(predicate::str::contains(
                    "vulcan --refresh background browse",
                ))
                .and(predicate::str::contains("vulcan browse --no-commit")),
        );
}

#[test]
fn edit_help_documents_picker_and_rescan_behavior() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["edit", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Behavior:")
                .and(predicate::str::contains(
                    "If NOTE is omitted in an interactive terminal",
                ))
                .and(predicate::str::contains("After the editor exits"))
                .and(predicate::str::contains("vulcan edit --new Inbox/Idea")),
        );
}

#[test]
fn diff_help_documents_comparison_sources() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["diff", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Comparison source:")
                .and(predicate::str::contains(
                    "git-backed vaults compare the note against git HEAD",
                ))
                .and(predicate::str::contains(
                    "cache-level changes since the last scan",
                ))
                .and(predicate::str::contains(
                    "vulcan diff --since weekly Projects/Alpha",
                )),
        );
}

#[test]
fn inbox_and_template_help_document_config_and_variables() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["inbox", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Configuration:")
                .and(predicate::str::contains(
                    "Inbox settings live under [inbox]",
                ))
                .and(predicate::str::contains("vulcan inbox --file scratch.txt")),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["template", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Template source:")
                .and(predicate::str::contains(
                    "If .obsidian/templates.json or the Templater plugin configures a template folder",
                ))
                .and(predicate::str::contains(
                    "{{title}} {{date}} {{time}} {{datetime}} {{uuid}}",
                ))
                .and(predicate::str::contains(
                    "{{date:YYYY-MM-DD}} {{time:HH:mm}}",
                ))
                .and(predicate::str::contains(
                    "Default template date/time formats live under [templates]",
                ))
                .and(predicate::str::contains("web_allowlist"))
                .and(predicate::str::contains("--engine auto"))
                .and(predicate::str::contains("--var key=value"))
                .and(predicate::str::contains("vulcan template --list"))
                .and(predicate::str::contains(
                    "vulcan template insert daily --prepend",
                ))
                .and(predicate::str::contains(
                    "vulcan template preview daily --path Journal/Today",
                ))
                .and(predicate::str::contains(
                    "Vulcan creates <date>-<template-name>.md",
                )),
        );
}

#[test]
fn template_preview_renders_templater_templates_with_var_bindings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template dir should be created");
    fs::write(
        vault_root.join(".vulcan/templates/preview.md"),
        "<%* tR += tp.file.title.toUpperCase(); %>\nProject <% tp.system.prompt(\"Project\") %>\nPath <% tp.obsidian.normalizePath(\"Notes/Plan\") %>\n",
    )
    .expect("template should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "template",
            "preview",
            "preview",
            "--path",
            "Notes/Plan",
            "--engine",
            "templater",
            "--var",
            "project=Vulcan",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["engine"], "templater");
    assert_eq!(json["path"], "Notes/Plan.md");
    let content = json["content"]
        .as_str()
        .expect("preview content should be a string");
    assert!(content.contains("PLAN"));
    assert!(content.contains("Project Vulcan"));
    assert!(content.contains("Path Notes/Plan.md"));
}

#[test]
fn template_insert_renders_templater_syntax_against_target_note() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template dir should be created");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should be created");
    fs::write(
        vault_root.join(".vulcan/templates/status.md"),
        "Status <% tp.frontmatter.status %>\nTitle <% tp.file.title %>\nToday <% tp.date.now(\"YYYY-MM-DD\") %>\n",
    )
    .expect("template should be written");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        "---\nstatus: active\n---\n# Existing\n",
    )
    .expect("target note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "template",
            "insert",
            "status",
            "Projects/Alpha",
            "--engine",
            "templater",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["engine"], "templater");
    assert_eq!(json["note"], "Projects/Alpha.md");
    let updated = fs::read_to_string(vault_root.join("Projects/Alpha.md"))
        .expect("updated note should exist");
    assert!(updated.contains("Status active"));
    assert!(updated.contains("Title Alpha"));
    assert!(updated
        .lines()
        .any(|line| line.starts_with("Today ") && line.len() == "Today ".len() + 10));
}

#[test]
fn template_preview_reports_diagnostics_for_mutating_helpers() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan/templates"))
        .expect("template dir should be created");
    fs::write(
        vault_root.join(".vulcan/templates/mutate.md"),
        "<%* await tp.file.create_new(\"Child body\", \"Child\") %>",
    )
    .expect("template should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "template",
            "preview",
            "mutate",
            "--engine",
            "templater",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["engine"], "templater");
    let diagnostics = json["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array");
    assert!(diagnostics.iter().any(|item| item
        .as_str()
        .is_some_and(|message| message.contains("disabled during template preview"))));
    assert!(!vault_root.join("Child.md").exists());
}

#[test]
fn bases_and_describe_help_document_runtime_surfaces() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["bases", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Evaluate and maintain Bases views")
                .and(predicate::str::contains("create"))
                .and(predicate::str::contains("view-add"))
                .and(predicate::str::contains(
                    "`create` derives folder and equality frontmatter from the first view; the TUI `n` hotkey uses the current view.",
                ))
                .and(predicate::str::contains(
                    "Mutating bases commands support --dry-run and --no-commit.",
                )),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["describe", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Output:")
                .and(predicate::str::contains("runtime CLI schema"))
                .and(predicate::str::contains(
                    "vulcan --output json describe > vulcan-schema.json",
                )),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["help", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Show integrated command and concept documentation")
                .and(predicate::str::contains("help query"))
                .and(predicate::str::contains("help note get --output json"))
                .and(predicate::str::contains("help --search graph")),
        );
}

#[test]
fn init_json_output_creates_default_config() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "init",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["created_config"], true);
    assert_eq!(json["created_cache"], true);
    assert!(vault_root.join(".vulcan/config.toml").exists());
    assert!(vault_root.join(".vulcan/cache.db").exists());
    assert!(vault_root.join(".vulcan/.gitignore").exists());
    assert_eq!(
        fs::read_to_string(vault_root.join(".vulcan/.gitignore"))
            .expect("gitignore should be readable"),
        "*\n!.gitignore\n!config.toml\nconfig.local.toml\n!reports/\nreports/*\n!reports/*.toml\n"
    );
    assert!(json.get("support_files").is_none());
}

#[test]
fn init_import_applies_detected_sources_and_reports_them() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
        .expect("dataview plugin dir should be created");
    fs::write(
        vault_root.join(".obsidian/app.json"),
        r#"{
          "useMarkdownLinks": true,
          "newLinkFormat": "relative"
        }"#,
    )
    .expect("app config should be written");
    fs::write(
        vault_root.join(".obsidian/plugins/dataview/data.json"),
        r#"{"inlineQueryPrefix":"dv:"}"#,
    )
    .expect("dataview config should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "init",
            "--import",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["created_config"], true);
    assert!(json["imported"].is_object());
    assert_eq!(json["imported"]["imported_count"], 2);
    assert!(json["importable_sources"]
        .as_array()
        .is_some_and(|sources| {
            sources.iter().any(|source| source["plugin"] == "core")
                && sources.iter().any(|source| source["plugin"] == "dataview")
        }));

    let rendered =
        fs::read_to_string(vault_root.join(".vulcan/config.toml")).expect("config should exist");
    assert!(rendered.contains("[links]"));
    assert!(rendered.contains("style = \"markdown\""));
    assert!(rendered.contains("[dataview]"));
    assert!(rendered.contains("inline_query_prefix = \"dv:\""));
}

#[test]
fn init_agent_files_writes_agents_template_and_default_skills() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "init",
            "--agent-files",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert!(vault_root.join("AGENTS.md").exists());
    assert!(vault_root.join("AI/Skills/note-operations.md").exists());
    assert!(vault_root.join("AI/Skills/js-api-guide.md").exists());
    assert!(fs::read_to_string(vault_root.join("AGENTS.md"))
        .expect("agents template should be readable")
        .contains("Use Vulcan as the primary automation surface"));
    assert!(json["support_files"].as_array().is_some_and(|items| items
        .iter()
        .any(|item| item["path"] == "AGENTS.md")
        && items
            .iter()
            .any(|item| item["path"] == "AI/Skills/js-api-guide.md")));
}

#[test]
fn scan_json_output_indexes_fixture_vault() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "scan",
            "--full",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let database = CacheDatabase::open(&VaultPaths::new(&vault_root)).expect("db should open");

    assert_eq!(json["mode"], "full");
    assert_eq!(json["discovered"], 3);
    assert_eq!(json["added"], 3);
    assert_eq!(
        document_paths(&database),
        vec![
            "Home.md".to_string(),
            "People/Bob.md".to_string(),
            "Projects/Alpha.md".to_string(),
        ]
    );
}

#[test]
fn cache_backed_commands_refresh_before_running_by_default() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    fs::write(vault_root.join("Home.md"), "# Home\nNo links yet.\n")
        .expect("home note should be written");
    fs::write(vault_root.join("Projects.md"), "# Alpha\n").expect("alpha note should be written");
    run_scan(&vault_root);
    fs::write(
        vault_root.join("Home.md"),
        "# Home\nNow links to [[Projects]].\n",
    )
    .expect("updated home note should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "backlinks",
            "Projects",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert!(rows
        .iter()
        .any(|row| row["source_path"] == Value::String("Home.md".to_string())));
}

#[test]
fn refresh_off_keeps_stale_cache_for_one_shot_commands() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault dir should be created");
    fs::write(vault_root.join("Home.md"), "# Home\nNo links yet.\n")
        .expect("home note should be written");
    fs::write(vault_root.join("Projects.md"), "# Alpha\n").expect("alpha note should be written");
    run_scan(&vault_root);
    fs::write(
        vault_root.join("Home.md"),
        "# Home\nNow links to [[Projects]].\n",
    )
    .expect("updated home note should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--refresh",
            "off",
            "backlinks",
            "Projects",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert!(
        rows.is_empty(),
        "stale cache should not include new backlink"
    );
}

#[test]
fn doctor_json_output_reports_clean_basic_vault() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "doctor",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["summary"]["unresolved_links"], 0);
    assert_eq!(json["summary"]["ambiguous_links"], 0);
    assert_eq!(json["summary"]["parse_failures"], 0);
    assert_eq!(json["summary"]["missing_index_rows"], 0);
    assert_eq!(json["summary"]["orphan_notes"], 0);
}

#[test]
fn doctor_json_output_reports_broken_frontmatter_vault() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("broken-frontmatter", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "doctor",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["summary"]["parse_failures"], 1);
    assert_eq!(json["parse_failures"][0]["document_path"], "Broken.md");
}

#[test]
fn doctor_json_output_reports_dataview_specific_issues() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".obsidian")).expect("obsidian dir should be created");
    fs::write(
        vault_root.join(".obsidian/types.json"),
        "{\n  \"priority\": \"number\"\n}\n",
    )
    .expect("types config should be written");
    fs::write(
        vault_root.join("Dashboard.md"),
        concat!(
            "priority:: high\n\n",
            "```dataview\n",
            "TABLE FROM\n",
            "```\n\n",
            "```dataviewjs\n",
            "dv.current()\n",
            "```\n",
        ),
    )
    .expect("note should be written");
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "doctor",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["summary"]["parse_failures"], 1);
    assert_eq!(json["summary"]["type_mismatches"], 1);
    assert_eq!(
        json["summary"]["unsupported_syntax"],
        Value::Number(usize::from(!cfg!(feature = "js_runtime")).into())
    );
    assert!(json["parse_failures"][0]["message"]
        .as_str()
        .is_some_and(|message| message.contains("Dataview block 0")));
    assert_eq!(json["type_mismatches"][0]["document_path"], "Dashboard.md");
    if cfg!(feature = "js_runtime") {
        assert_eq!(json["unsupported_syntax"], serde_json::json!([]));
    } else {
        assert_eq!(
            json["unsupported_syntax"][0]["document_path"],
            "Dashboard.md"
        );
    }
}

#[test]
fn doctor_fix_json_output_plans_repairs_for_uninitialized_vault() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "doctor",
            "--fix",
            "--dry-run",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);
    let fixes = json["fixes"].as_array().expect("fixes should be an array");

    assert_eq!(json["dry_run"], true);
    assert!(fixes.iter().any(|fix| fix["kind"] == "initialize"));
    assert!(fixes.iter().any(|fix| fix["kind"] == "scan"));
}

#[test]
fn rename_property_json_output_reports_planned_file_changes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("refactors", &vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "rename-property",
            "status",
            "phase",
            "--dry-run",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["action"], "rename_property");
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["files"][0]["path"], "Home.md");
    assert_eq!(json["files"][0]["changes"][0]["before"], "status");
    assert_eq!(json["files"][0]["changes"][0]["after"], "phase");
}

#[test]
fn graph_path_json_output_returns_note_path_chain() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "graph",
            "path",
            "Bob",
            "Home",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(
        json["path"],
        serde_json::json!(["People/Bob.md", "Projects/Alpha.md", "Home.md"])
    );
}

#[test]
fn graph_moc_and_trends_json_output_report_candidates_and_history() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    fs::write(vault_root.join("Extra.md"), "# Extra\n\n[[Home]]\n")
        .expect("extra note should write");
    run_incremental_scan(&vault_root);

    let moc_rows = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "graph",
                "moc",
            ])
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    assert_eq!(moc_rows[0]["document_path"], "Home.md");
    assert!(moc_rows[0]["reasons"]
        .as_array()
        .expect("reasons should be an array")
        .iter()
        .any(|reason| reason.as_str().unwrap_or_default().contains("index")));

    let trends = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "graph",
                "trends",
                "--limit",
                "2",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let points = trends["points"]
        .as_array()
        .expect("points should be an array");
    assert_eq!(points.len(), 2);
    assert_eq!(points[0]["note_count"], 3);
    assert_eq!(points[1]["note_count"], 4);
}

#[test]
fn checkpoint_and_changes_json_output_track_named_baselines() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let checkpoint = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "checkpoint",
                "create",
                "baseline",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    assert_eq!(checkpoint["name"], "baseline");
    assert_eq!(checkpoint["source"], "manual");

    fs::write(
        vault_root.join("Home.md"),
        "# Home\n\nUpdated dashboard links.\n",
    )
    .expect("updated note should write");
    run_incremental_scan(&vault_root);

    let checkpoint_rows = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "checkpoint",
                "list",
            ])
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    assert!(checkpoint_rows
        .iter()
        .any(|row| row["name"] == "baseline" && row["source"] == "manual"));

    let changes = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "changes",
                "--checkpoint",
                "baseline",
            ])
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    assert!(changes.iter().any(|row| {
        row["anchor"] == "baseline" && row["kind"] == "note" && row["path"] == "Home.md"
    }));
}

#[test]
fn cache_verify_json_output_reports_healthy_fixture_cache() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "cache",
            "verify",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["healthy"], true);
    assert!(json["checks"]
        .as_array()
        .expect("checks should be an array")
        .iter()
        .all(|check| check["ok"] == true));
}

#[test]
fn links_json_output_supports_alias_lookup() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "links",
            "Start",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 2);
    assert_eq!(json_lines[0]["note_path"], "Home.md");
    assert_eq!(json_lines[0]["matched_by"], "alias");
}

#[test]
fn backlinks_json_output_lists_sources() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "backlinks",
            "Projects/Alpha",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines[0]["note_path"], "Projects/Alpha.md");
    assert_eq!(
        json_lines
            .iter()
            .map(|row| row["source_path"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>(),
        vec!["Home.md".to_string(), "People/Bob.md".to_string()]
    );
}

#[test]
fn note_commands_without_arguments_fail_cleanly_in_non_interactive_mode() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let links_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "--output", "json", "links"])
        .assert()
        .failure();
    let links_json = parse_stdout_json(&links_assert);
    assert_eq!(links_json["code"], "operation_failed");
    assert!(links_json["error"]
        .as_str()
        .is_some_and(|message| message
            .contains("missing note; provide a note identifier or run interactively")));

    let related_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--vault", &vault_root_str, "--output", "json", "related"])
        .assert()
        .failure();
    let related_json = parse_stdout_json(&related_assert);
    assert_eq!(related_json["code"], "operation_failed");
    assert!(related_json["error"]
        .as_str()
        .is_some_and(|message| message
            .contains("missing note; provide a note identifier or run interactively")));

    let suggest_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "suggest",
            "mentions",
            "Missing",
        ])
        .assert()
        .failure();
    let suggest_json = parse_stdout_json(&suggest_assert);
    assert_eq!(suggest_json["code"], "operation_failed");
    assert!(suggest_json["error"]
        .as_str()
        .is_some_and(|message| message.contains("note not found")));
}

#[test]
fn json_error_output_is_structured_for_invalid_arguments() {
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "init", "--import", "--no-import"])
        .assert()
        .failure();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["code"], "invalid_arguments");
    assert!(json["error"]
        .as_str()
        .is_some_and(|message| message.contains("cannot be used with")));
}

#[test]
fn links_json_output_supports_fields_limit_and_offset() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "resolved_target_path,resolution_status",
            "--limit",
            "1",
            "--offset",
            "1",
            "links",
            "Start",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(
        json_lines[0],
        serde_json::json!({
            "resolved_target_path": "People/Bob.md",
            "resolution_status": "resolved"
        })
    );
}

#[test]
fn search_json_output_returns_ranked_hits_and_supports_filters() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,snippet",
            "--limit",
            "1",
            "search",
            "Robert",
            "--path-prefix",
            "People/",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(
        json_lines[0]["document_path"],
        serde_json::Value::String("People/Bob.md".to_string())
    );
    assert!(json_lines[0]["snippet"]
        .as_str()
        .expect("snippet should be a string")
        .contains("Bob"));
}

#[test]
fn search_json_output_supports_explain_fuzzy_and_where_filters() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,effective_query,parsed_query_explanation,explain",
            "search",
            "releese",
            "--where",
            "reviewed = true",
            "--fuzzy",
            "--explain",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["document_path"], "Done.md");
    assert!(rows[0]["effective_query"]
        .as_str()
        .expect("effective query should be a string")
        .contains("release"));
    assert!(rows[0]["parsed_query_explanation"]
        .as_array()
        .expect("parsed query explanation should be an array")
        .iter()
        .any(|line| line == "TERM releese"));
    assert_eq!(rows[0]["explain"]["strategy"], "keyword");
}

#[test]
fn search_explain_human_output_includes_grouped_query_plan() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "search",
            "(dashboard or bob) -(\"owned by\" draft)",
            "--explain",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Query plan:")
                .and(predicate::str::contains("AND"))
                .and(predicate::str::contains("OR"))
                .and(predicate::str::contains("NOT")),
        );
}

#[test]
fn search_inline_file_content_and_match_case_operators_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("Meeting.md"),
        "# Notes\nReleaseAlias checklist",
    )
    .expect("meeting note should write");
    fs::write(
        vault_root.join("Reference.md"),
        "---\naliases:\n  - ReleaseAlias\n---\n\n# Reference\nnothing else",
    )
    .expect("reference note should write");
    fs::write(vault_root.join("People.md"), "Bob\nbob").expect("people note should write");
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "content:ReleaseAlias",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Meeting.md\"")
                .and(predicate::str::contains("Reference.md").not()),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "match-case:Bob",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"document_path\":\"People.md\""));
}

#[test]
fn search_line_and_block_operators_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(vault_root.join("SameLine.md"), "mix flour\noven ready").expect("note should write");
    fs::write(vault_root.join("SplitLine.md"), "mix\nflour").expect("note should write");
    fs::write(
        vault_root.join("SameBlock.md"),
        "mix flour\nstir well\n\nserve",
    )
    .expect("note should write");
    fs::write(vault_root.join("SplitBlock.md"), "mix\n\nflour").expect("note should write");
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "line:(mix flour)",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"SameLine.md\"")
                .and(predicate::str::contains("SplitLine.md").not()),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "block:(mix flour)",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"SameBlock.md\"")
                .and(predicate::str::contains("SplitBlock.md").not()),
        );
}

#[test]
fn search_section_operator_works_across_heading_chunks() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("SameSection.md"),
        "# Plan\n\ndog checklist\n\ncat summary",
    )
    .expect("note should write");
    fs::write(
        vault_root.join("SplitSection.md"),
        "# Dogs\n\ndog checklist\n\n# Cats\n\ncat summary",
    )
    .expect("note should write");
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "section:(dog cat)",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"SameSection.md\"")
                .and(predicate::str::contains("SplitSection.md").not()),
        );
}

#[test]
fn search_inline_bracket_property_filters_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,parsed_query_explanation",
            "search",
            "release [status:done]",
            "--explain",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Done.md\"")
                .and(predicate::str::contains("Backlog.md").not())
                .and(predicate::str::contains("WHERE [status:done]")),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "[status:done OR backlog]",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Done.md\"")
                .and(predicate::str::contains("\"document_path\":\"Backlog.md\"")),
        );
}

#[test]
fn search_inline_regex_filters_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Journal")).expect("journal dir should exist");
    fs::write(vault_root.join("Notes.md"), "Meeting on 2026-03-26.").expect("note should write");
    fs::write(
        vault_root.join("Journal/2026-03-26.md"),
        "Daily notes without a date in body.",
    )
    .expect("note should write");
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,parsed_query_explanation",
            "search",
            "/\\d{4}-\\d{2}-\\d{2}/",
            "--explain",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Notes.md\"")
                .and(predicate::str::contains("Journal/2026-03-26.md").not())
                .and(predicate::str::contains(
                    "REGEX /\\\\d{4}-\\\\d{2}-\\\\d{2}/",
                )),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "path:/2026-03-\\d{2}/",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"document_path\":\"Journal/2026-03-26.md\"",
        ));
}

#[test]
fn search_task_operators_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("Tasks.md"),
        "- [ ] write docs\n- [x] ship release\nplain write docs note",
    )
    .expect("note should write");
    fs::write(vault_root.join("Body.md"), "write docs outside of tasks")
        .expect("note should write");
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,parsed_query_explanation",
            "search",
            "task:write",
            "--explain",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Tasks.md\"")
                .and(predicate::str::contains("Body.md").not())
                .and(predicate::str::contains("FILTER task:write")),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "task-todo:write",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Tasks.md\"")
                .and(predicate::str::contains("Body.md").not()),
        );

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "search",
            "task-done:ship",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"document_path\":\"Tasks.md\"")
                .and(predicate::str::contains("Body.md").not()),
        );
}

#[test]
fn search_sort_orders_results_and_reports_sort_plan() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(vault_root.join("Alpha.md"), "dashboard").expect("alpha note should write");
    fs::write(vault_root.join("Beta.md"), "dashboard").expect("beta note should write");
    fs::write(vault_root.join("Gamma.md"), "dashboard").expect("gamma note should write");
    run_scan(&vault_root);

    let paths = VaultPaths::new(&vault_root);
    let database = CacheDatabase::open(&paths).expect("db should open");
    let set_mtime = |path: &str, mtime: i64| {
        database
            .connection()
            .execute(
                "UPDATE documents SET file_mtime = ? WHERE path = ?",
                (mtime, path),
            )
            .expect("document mtime should update");
    };
    set_mtime("Alpha.md", 100);
    set_mtime("Beta.md", 300);
    set_mtime("Gamma.md", 200);

    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let search_rows = |fields: &str, sort: &str, explain: bool| {
        let mut args = vec![
            "--vault",
            vault_root_str.as_str(),
            "--refresh",
            "off",
            "--output",
            "json",
            "--fields",
            fields,
            "search",
            "dashboard",
            "--sort",
            sort,
        ];
        if explain {
            args.push("--explain");
        }
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args(args)
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    let document_paths = |rows: &[Value]| {
        rows.iter()
            .map(|row| {
                row["document_path"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect::<Vec<_>>()
    };

    let path_desc_rows = search_rows("document_path", "path-desc", false);
    assert_eq!(
        document_paths(&path_desc_rows),
        vec![
            "Gamma.md".to_string(),
            "Beta.md".to_string(),
            "Alpha.md".to_string(),
        ]
    );

    let modified_rows = search_rows(
        "document_path,parsed_query_explanation",
        "modified-newest",
        true,
    );
    assert_eq!(
        document_paths(&modified_rows),
        vec![
            "Beta.md".to_string(),
            "Gamma.md".to_string(),
            "Alpha.md".to_string(),
        ]
    );
    assert!(modified_rows[0]["parsed_query_explanation"]
        .as_array()
        .expect("parsed query explanation should be an array")
        .iter()
        .any(|line| line == "SORT modified-newest"));
}

#[test]
fn search_match_case_flag_reports_matched_line_and_no_result_suggestions() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(vault_root.join("Upper.md"), "Bob builds dashboards.")
        .expect("upper note should write");
    fs::write(vault_root.join("Lower.md"), "bob builds dashboards.")
        .expect("lower note should write");
    run_scan(&vault_root);

    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let match_case_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "document_path,matched_line",
            "search",
            "Bob",
            "--match-case",
        ])
        .assert()
        .success();
    let match_case_rows = parse_stdout_json_lines(&match_case_assert);
    assert_eq!(match_case_rows.len(), 1);
    assert_eq!(match_case_rows[0]["document_path"], "Upper.md");
    assert_eq!(match_case_rows[0]["matched_line"], 1);

    let no_result_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "no_results,parsed_query_explanation",
            "search",
            "contents:Bob task-todo:ship",
            "--explain",
        ])
        .assert()
        .success();
    let no_result_rows = parse_stdout_json_lines(&no_result_assert);
    assert_eq!(no_result_rows.len(), 1);
    assert_eq!(no_result_rows[0]["no_results"], true);
    let explanation = no_result_rows[0]["parsed_query_explanation"]
        .as_array()
        .expect("parsed query explanation should be an array");
    assert!(explanation
        .iter()
        .any(|line| line == "SUGGESTION did you mean `content:` instead of `contents:`?"));
    assert!(explanation
        .iter()
        .any(|line| line == "SUGGESTION no tasks found in matched files for `task-todo:`"));
}

#[test]
fn notes_json_output_filters_and_sorts_property_queries() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,properties",
            "notes",
            "--where",
            "estimate > 2",
            "--sort",
            "due",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 2);
    assert_eq!(json_lines[0]["document_path"], "Done.md");
    assert_eq!(json_lines[1]["document_path"], "Backlog.md");
    assert_eq!(json_lines[0]["properties"]["status"], "done");
}

#[test]
fn notes_json_output_supports_inline_field_and_file_namespace_filters() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    fs::write(
        vault_root.join("Large.md"),
        format!("due:: 2020-01-01\n\n{}\n", "x".repeat(12_000)),
    )
    .expect("large note should be written");
    fs::write(vault_root.join("Small.md"), "due:: 2099-01-01\n")
        .expect("small note should be written");
    run_scan(&vault_root);

    let overdue = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "notes",
            "--where",
            "due < date(today)",
        ])
        .assert()
        .success();
    let overdue_rows = parse_stdout_json_lines(&overdue);
    assert_eq!(overdue_rows.len(), 1);
    assert_eq!(overdue_rows[0]["document_path"], "Large.md");

    let large_files = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "notes",
            "--where",
            "file.size > 10000",
        ])
        .assert()
        .success();
    let large_file_rows = parse_stdout_json_lines(&large_files);
    assert_eq!(large_file_rows.len(), 1);
    assert_eq!(large_file_rows[0]["document_path"], "Large.md");
}

#[test]
fn search_json_output_supports_has_property_filter() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "search",
            "release",
            "--has-property",
            "empty_text",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(json_lines[0]["document_path"], "Done.md");
}

#[test]
fn bases_eval_json_output_returns_rows_and_diagnostics() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "bases",
            "eval",
            "release.base",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["views"][0]["name"], "Release Table");
    assert_eq!(
        json["views"][0]["filters"],
        serde_json::json!([
            "file.ext = \"md\"",
            "status starts_with \"b\"",
            "estimate > 2"
        ])
    );
    assert_eq!(json["views"][0]["group_by"]["property"], "status");
    assert_eq!(json["views"][0]["columns"][1]["display_name"], "Due");
    assert_eq!(json["views"][0]["rows"][0]["document_path"], "Backlog.md");
    assert_eq!(json["views"][0]["rows"][0]["group_value"], "backlog");
    assert_eq!(
        json["views"][0]["rows"][0]["formulas"]["note_name"],
        "Backlog"
    );
    assert!(json["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array")
        .iter()
        .any(|diagnostic| diagnostic["message"] == "unsupported view type `board`"));
}

#[test]
fn bases_eval_json_fields_stream_rows() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,group_value,cells",
            "bases",
            "eval",
            "release.base",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["document_path"], "Backlog.md");
    assert_eq!(rows[0]["group_value"], "backlog");
    assert_eq!(rows[0]["cells"]["note_name"], "Backlog");
}

#[test]
fn bases_human_output_is_compact_and_grouped() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "eval",
            "release.base",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Release Table")
                .and(predicate::str::contains("Grouped by: Status"))
                .and(predicate::str::contains("Group: backlog"))
                .and(predicate::str::contains("Name"))
                .and(predicate::str::contains("Due"))
                .and(predicate::str::contains("Backlog")),
        );
}

#[test]
fn bases_tui_json_output_falls_back_to_eval_report() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    let tui_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "bases",
                "tui",
                "release.base",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    let eval_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "bases",
                "eval",
                "release.base",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    assert_eq!(tui_json, eval_json);
}

#[test]
fn search_notes_and_bases_support_file_exports() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);
    let search_export = temp_dir.path().join("search.csv");
    let notes_export = temp_dir.path().join("notes.jsonl");
    let bases_export = temp_dir.path().join("bases.csv");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "search",
            "release",
            "--export",
            "csv",
            "--export-path",
            search_export
                .to_str()
                .expect("search export path should be valid utf-8"),
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "notes",
            "--where",
            "reviewed = true",
            "--export",
            "jsonl",
            "--export-path",
            notes_export
                .to_str()
                .expect("notes export path should be valid utf-8"),
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "eval",
            "release.base",
            "--export",
            "csv",
            "--export-path",
            bases_export
                .to_str()
                .expect("bases export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let search_csv = fs::read_to_string(&search_export).expect("search export should exist");
    let notes_jsonl = fs::read_to_string(&notes_export).expect("notes export should exist");
    let bases_csv = fs::read_to_string(&bases_export).expect("bases export should exist");

    assert!(search_csv.contains("document_path"));
    assert!(search_csv.contains("Backlog.md"));
    assert_eq!(notes_jsonl.lines().count(), 2);
    assert!(notes_jsonl.contains("\"document_path\":\"Backlog.md\""));
    assert!(bases_csv.contains("document_path"));
    assert!(bases_csv.contains("Backlog.md"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn graph_links_changes_and_cluster_support_file_exports() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "checkpoint",
            "create",
            "baseline",
        ])
        .assert()
        .success();
    fs::write(
        vault_root.join("Home.md"),
        "---\naliases:\n  - Start\ntags:\n  - dashboard\n---\n\n# Home\n\nUpdated dashboard links.\n",
    )
    .expect("updated note should write");
    run_incremental_scan(&vault_root);
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "vectors",
            "index",
        ])
        .assert()
        .success();
    let links_export = temp_dir.path().join("links.csv");
    let hubs_export = temp_dir.path().join("hubs.jsonl");
    let changes_export = temp_dir.path().join("changes.csv");
    let cluster_export = temp_dir.path().join("cluster.jsonl");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "links",
            "Bob",
            "--export",
            "csv",
            "--export-path",
            links_export
                .to_str()
                .expect("links export path should be valid utf-8"),
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "graph",
            "hubs",
            "--export",
            "jsonl",
            "--export-path",
            hubs_export
                .to_str()
                .expect("hubs export path should be valid utf-8"),
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "changes",
            "--checkpoint",
            "baseline",
            "--export",
            "csv",
            "--export-path",
            changes_export
                .to_str()
                .expect("changes export path should be valid utf-8"),
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "cluster",
            "--clusters",
            "2",
            "--export",
            "jsonl",
            "--export-path",
            cluster_export
                .to_str()
                .expect("cluster export path should be valid utf-8"),
        ])
        .assert()
        .success();

    let links_csv = fs::read_to_string(&links_export).expect("links export should exist");
    let hubs_jsonl = fs::read_to_string(&hubs_export).expect("hubs export should exist");
    let changes_csv = fs::read_to_string(&changes_export).expect("changes export should exist");
    let cluster_jsonl = fs::read_to_string(&cluster_export).expect("cluster export should exist");

    assert!(links_csv.contains("Projects/Alpha.md"));
    assert!(links_csv.contains("[[Projects/Alpha#Status]]"));
    assert!(hubs_jsonl.contains("\"document_path\":\"Projects/Alpha.md\""));
    assert!(changes_csv.contains("baseline"));
    assert!(changes_csv.contains("Home.md"));
    assert!(cluster_jsonl.contains("\"cluster_label\""));
    server.shutdown();
}

#[test]
fn export_search_index_writes_static_json_payload() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let export_path = temp_dir.path().join("search-index.json");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "export",
            "search-index",
            "--path",
            export_path
                .to_str()
                .expect("export path should be valid utf-8"),
            "--pretty",
        ])
        .assert()
        .success();

    let payload: Value = serde_json::from_str(
        &fs::read_to_string(&export_path).expect("search index export should exist"),
    )
    .expect("search index export should parse");

    assert_eq!(payload["version"], 1);
    assert_eq!(payload["documents"], 3);
    assert!(payload["entries"]
        .as_array()
        .expect("entries should be an array")
        .iter()
        .any(|entry| {
            entry["document_path"] == "Home.md"
                && entry["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("dashboard")
        }));
}

#[test]
#[allow(clippy::too_many_lines)]
fn saved_reports_can_be_listed_run_and_batched() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--fields",
            "document_path,rank",
            "--limit",
            "1",
            "saved",
            "search",
            "weekly-search",
            "release",
            "--description",
            "weekly release hits",
            "--export",
            "jsonl",
            "--export-path",
            "exports/search.jsonl",
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--fields",
            "document_path,group_value",
            "saved",
            "bases",
            "release-table",
            "release.base",
            "--export",
            "csv",
            "--export-path",
            "exports/release.csv",
        ])
        .assert()
        .success();

    let list_rows = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "saved",
                "list",
            ])
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    assert_eq!(list_rows.len(), 2);
    assert_eq!(list_rows[0]["name"], "release-table");
    assert_eq!(list_rows[1]["name"], "weekly-search");

    let show_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "saved",
                "show",
                "weekly-search",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    assert_eq!(show_json["name"], "weekly-search");
    assert_eq!(show_json["kind"], "search");

    let run_rows = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "saved",
                "run",
                "weekly-search",
            ])
            .assert()
            .success();
        parse_stdout_json_lines(&assert)
    };
    assert_eq!(run_rows.len(), 1);
    assert_eq!(run_rows[0]["document_path"], "Backlog.md");
    assert!(vault_root.join("exports/search.jsonl").exists());

    let batch_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "batch",
                "--all",
            ])
            .assert()
            .success();
        let mut json = parse_stdout_json(&assert);
        replace_string_recursively(&mut json, &vault_root.display().to_string(), "<vault>");
        // Normalize any remaining backslash path separators (Windows) to forward slashes.
        replace_string_recursively(&mut json, "\\", "/");
        json
    };
    assert_eq!(batch_json["succeeded"], 2);
    assert_eq!(batch_json["failed"], 0);
    assert!(vault_root.join("exports/search.jsonl").exists());
    assert!(vault_root.join("exports/release.csv").exists());
}

#[test]
fn doctor_and_cache_verify_support_issue_exit_codes() {
    let broken_dir = TempDir::new().expect("temp dir should be created");
    let broken_vault = broken_dir.path().join("vault");
    copy_fixture_vault("broken-frontmatter", &broken_vault);
    run_scan(&broken_vault);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            broken_vault
                .to_str()
                .expect("broken vault path should be valid utf-8"),
            "doctor",
            "--fail-on-issues",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("parse failures: 1"));

    let cache_dir = TempDir::new().expect("temp dir should be created");
    let cache_vault = cache_dir.path().join("vault");
    copy_fixture_vault("basic", &cache_vault);
    run_scan(&cache_vault);
    let paths = VaultPaths::new(&cache_vault);
    let mut database = CacheDatabase::open(&paths).expect("cache should open");
    database
        .with_transaction(|transaction| {
            transaction
                .execute("DELETE FROM search_chunk_content", [])
                .expect("search rows should delete");
            Ok::<_, vulcan_core::CacheError>(())
        })
        .expect("cache mutation should succeed");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            cache_vault
                .to_str()
                .expect("cache vault path should be valid utf-8"),
            "cache",
            "verify",
            "--fail-on-errors",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("Cache healthy: false"));
}

#[test]
fn automation_run_executes_saved_reports_and_health_checks() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--fields",
            "document_path,rank",
            "--limit",
            "1",
            "saved",
            "search",
            "weekly-search",
            "dashboard",
            "--description",
            "weekly dashboard hits",
            "--export",
            "jsonl",
            "--export-path",
            "exports/search.jsonl",
        ])
        .assert()
        .success();

    let automation_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                vault_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "automation",
                "run",
                "--scan",
                "--doctor",
                "--verify-cache",
                "weekly-search",
                "--fail-on-issues",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    assert_eq!(
        automation_json["actions"],
        serde_json::json!(["scan", "doctor", "cache_verify", "saved_reports"])
    );
    assert_eq!(automation_json["issues_detected"], false);
    assert_eq!(automation_json["cache_verify"]["healthy"], true);
    assert_eq!(automation_json["reports"]["succeeded"], 1);
    assert!(vault_root.join("exports/search.jsonl").exists());
}

fn replace_string_recursively(value: &mut Value, pattern: &str, replacement: &str) {
    match value {
        Value::Object(object) => {
            for nested in object.values_mut() {
                replace_string_recursively(nested, pattern, replacement);
            }
        }
        Value::Array(values) => {
            for nested in values {
                replace_string_recursively(nested, pattern, replacement);
            }
        }
        Value::String(string) => {
            if string.contains(pattern) {
                *string = string.replace(pattern, replacement);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[test]
fn search_json_output_supports_limit_and_offset() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,heading_path",
            "--limit",
            "1",
            "--offset",
            "1",
            "search",
            "Alpha",
        ])
        .assert()
        .success();
    let json_lines = parse_stdout_json_lines(&assert);

    assert_eq!(json_lines.len(), 1);
    assert_eq!(
        json_lines[0],
        serde_json::json!({
            "document_path": "Projects/Alpha.md",
            "heading_path": ["Alpha", "Status"]
        })
    );
}

#[test]
fn search_json_output_matches_snapshot() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,heading_path,query,tag,path_prefix,snippet",
            "search",
            "dashboard",
        ])
        .assert()
        .success();

    assert_json_snapshot_lines(
        "search_basic_dashboard.json",
        &parse_stdout_json_lines(&assert),
    );
}

#[test]
fn vectors_index_and_neighbors_json_output_work_end_to_end() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);

    let mut index_command = Command::cargo_bin("vulcan").expect("binary should build");
    let index_assert = index_command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "vectors",
            "index",
        ])
        .assert()
        .success();
    let index_json = parse_stdout_json(&index_assert);

    assert_eq!(index_json["indexed"], 4);
    assert_eq!(index_json["failed"], 0);

    let mut neighbors_command = Command::cargo_bin("vulcan").expect("binary should build");
    let neighbors_assert = neighbors_command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,distance",
            "--limit",
            "1",
            "vectors",
            "neighbors",
            "dashboard",
        ])
        .assert()
        .success();
    let neighbor_rows = parse_stdout_json_lines(&neighbors_assert);

    assert_eq!(neighbor_rows.len(), 1);
    assert_eq!(neighbor_rows[0]["document_path"], "Home.md");
    server.shutdown();
}

#[test]
fn search_human_output_is_multi_line_and_readable() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "search",
            "dashboard",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1. Home.md > Home")
                .and(predicate::str::contains("\n   Rank: "))
                .and(predicate::str::contains("\n   Snippet: Home"))
                .and(predicate::str::contains(
                    "The [dashboard] note uses the tag #index.",
                )),
        );
}

#[test]
fn search_hybrid_json_output_combines_vector_and_keyword_results() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "vectors",
            "index",
        ])
        .assert()
        .success();

    let mut command = Command::cargo_bin("vulcan").expect("binary should build");
    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,mode",
            "--limit",
            "2",
            "search",
            "dashboard",
            "--mode",
            "hybrid",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["mode"], "hybrid");
    assert_eq!(rows[0]["document_path"], "Home.md");
    server.shutdown();
}

#[test]
fn vectors_duplicates_and_cluster_json_output_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "vectors",
            "index",
        ])
        .assert()
        .success();

    let duplicates_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "left_document_path,right_document_path,similarity",
            "vectors",
            "duplicates",
            "--threshold",
            "0.7",
        ])
        .assert()
        .success();
    let duplicate_rows = parse_stdout_json_lines(&duplicates_assert);

    assert!(!duplicate_rows.is_empty());
    assert!(
        duplicate_rows[0]["similarity"]
            .as_f64()
            .expect("similarity should be numeric")
            >= 0.7
    );

    let cluster_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "cluster_id,cluster_label,keywords,chunk_count,document_count",
            "cluster",
            "--clusters",
            "2",
        ])
        .assert()
        .success();
    let cluster_rows = parse_stdout_json_lines(&cluster_assert);

    assert_eq!(cluster_rows.len(), 2);
    assert!(cluster_rows[0]["chunk_count"].as_u64().unwrap_or_default() >= 1);
    assert!(!cluster_rows[0]["cluster_label"]
        .as_str()
        .expect("cluster label should be a string")
        .is_empty());
    assert!(!cluster_rows[0]["keywords"]
        .as_array()
        .expect("keywords should be an array")
        .is_empty());
    server.shutdown();
}

#[test]
fn vectors_repair_queue_and_related_json_output_work() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "vectors",
            "index",
        ])
        .assert()
        .success();
    fs::write(
        vault_root.join("Home.md"),
        "---\naliases:\n  - Start\ntags:\n  - dashboard\n---\n\n# Home\n\nUpdated dashboard plans.\n",
    )
    .expect("updated note should write");
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "scan",
        ])
        .assert()
        .success();

    let repair_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "vectors",
            "repair",
            "--dry-run",
        ])
        .assert()
        .success();
    let repair_json = parse_stdout_json(&repair_assert);
    assert_eq!(repair_json["pending_chunks"], 1);

    let queue_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "vectors",
            "queue",
            "status",
        ])
        .assert()
        .success();
    let queue_json = parse_stdout_json(&queue_assert);
    assert_eq!(queue_json["pending_chunks"], 1);

    let related_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,similarity,matched_chunks",
            "related",
            "Home",
        ])
        .assert()
        .success();
    let related_rows = parse_stdout_json_lines(&related_assert);
    assert!(!related_rows.is_empty());
    assert_ne!(related_rows[0]["document_path"], "Home.md");
    server.shutdown();
}

#[test]
fn scan_human_output_reports_progress_on_stderr() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "scan",
            "--full",
        ])
        .assert()
        .success()
        .stderr(
            predicate::str::contains("Discovered 3 files; running full scan...")
                .and(predicate::str::contains("Scanned 3/3 files"))
                .and(predicate::str::contains("Resolving links...")),
        );
}

#[test]
fn vectors_index_human_output_reports_progress_and_throughput_settings() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vault_root, &server.base_url());
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "vectors",
            "index",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("batch size 8, concurrency 1"))
        .stderr(
            predicate::str::contains("Indexing 4 vector chunks with openai-compatible:fixture")
                .and(predicate::str::contains("Completed batch 1/1")),
        );

    server.shutdown();
}

#[test]
fn scan_json_output_matches_snapshot() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "scan",
            "--full",
        ])
        .assert()
        .success();

    assert_json_snapshot("scan_basic_full.json", &parse_stdout_json(&assert));
}

#[test]
fn doctor_json_output_matches_snapshot() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("broken-frontmatter", &vault_root);
    run_scan(&vault_root);
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    let assert = command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "doctor",
        ])
        .assert()
        .success();

    assert_json_snapshot(
        "doctor_broken_frontmatter.json",
        &parse_stdout_json(&assert),
    );
}

#[test]
fn move_json_output_supports_dry_run_and_apply() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("move-rewrite", &vault_root);
    run_scan(&vault_root);
    let mut dry_run_command = Command::cargo_bin("vulcan").expect("binary should build");

    let dry_run = dry_run_command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "move",
            "Projects/Alpha.md",
            "Archive/Alpha.md",
            "--dry-run",
        ])
        .assert()
        .success();
    let dry_run_json = parse_stdout_json(&dry_run);

    assert_eq!(dry_run_json["dry_run"], true);
    assert_eq!(dry_run_json["destination_path"], "Archive/Alpha.md");
    assert!(vault_root.join("Projects/Alpha.md").exists());

    let mut move_command = Command::cargo_bin("vulcan").expect("binary should build");
    let applied = move_command
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "move",
            "Projects/Alpha.md",
            "Archive/Alpha.md",
        ])
        .assert()
        .success();
    let applied_json = parse_stdout_json(&applied);

    assert_eq!(applied_json["dry_run"], false);
    assert!(vault_root.join("Archive/Alpha.md").exists());
    assert!(fs::read_to_string(vault_root.join("Home.md"))
        .expect("home should be readable")
        .contains("[[Archive/Alpha#Status]]"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn suggest_and_rewrite_json_outputs_cover_linking_and_duplicates() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("suggestions", &vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let mentions_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "source_path,matched_text,target_path,candidate_count,status",
            "suggest",
            "mentions",
            "Home",
        ])
        .assert()
        .success();
    let mention_rows = parse_stdout_json_lines(&mentions_assert);
    assert!(mention_rows.iter().any(|row| {
        row["matched_text"] == "Bob"
            && row["target_path"] == "People/Bob.md"
            && row["status"] == "unambiguous"
    }));
    assert!(mention_rows.iter().any(|row| {
        row["matched_text"] == "Alpha"
            && row["target_path"].is_null()
            && row["candidate_count"] == 2
            && row["status"] == "ambiguous"
    }));

    let duplicates_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "kind,value,paths,left_path,right_path,score",
            "suggest",
            "duplicates",
        ])
        .assert()
        .success();
    let duplicate_rows = parse_stdout_json_lines(&duplicates_assert);
    assert!(duplicate_rows
        .iter()
        .any(|row| row["kind"] == "duplicate_title" && row["value"] == "Alpha"));
    assert!(duplicate_rows
        .iter()
        .any(|row| row["kind"] == "alias_collision" && row["value"] == "Guide"));
    assert!(duplicate_rows.iter().any(|row| {
        row["kind"] == "merge_candidate"
            && row["left_path"] == "Archive/Alpha.md"
            && row["right_path"] == "Projects/Alpha.md"
    }));

    let link_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "link-mentions",
            "Home",
            "--dry-run",
        ])
        .assert()
        .success();
    let link_json = parse_stdout_json(&link_assert);
    assert_eq!(link_json["action"], "link_mentions");
    assert_eq!(link_json["dry_run"], true);
    assert_eq!(link_json["files"][0]["path"], "Home.md");

    let rewrite_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "rewrite",
            "--find",
            "Guide",
            "--replace",
            "Manual",
            "--dry-run",
        ])
        .assert()
        .success();
    let rewrite_json = parse_stdout_json(&rewrite_assert);
    assert_eq!(rewrite_json["action"], "bulk_replace");
    assert_eq!(rewrite_json["dry_run"], true);
    assert!(rewrite_json["files"]
        .as_array()
        .expect("files should be an array")
        .iter()
        .any(|file| file["path"] == "Home.md"));
}

#[test]
fn rebuild_and_repair_json_output_support_dry_run() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    run_scan(&vault_root);

    let rebuild_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "rebuild",
            "--dry-run",
        ])
        .assert()
        .success();
    let rebuild_json = parse_stdout_json(&rebuild_assert);
    assert_eq!(rebuild_json["dry_run"], true);
    assert_eq!(rebuild_json["discovered"], 3);

    let repair_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "repair",
            "fts",
            "--dry-run",
        ])
        .assert()
        .success();
    let repair_json = parse_stdout_json(&repair_assert);
    assert_eq!(repair_json["dry_run"], true);
    assert_eq!(repair_json["indexed_documents"], 3);
    assert_eq!(repair_json["indexed_chunks"], 4);
}

#[test]
fn describe_json_output_exposes_runtime_command_schema() {
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "describe"])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["name"], "vulcan");
    assert!(json["after_help"]
        .as_str()
        .expect("after_help should be a string")
        .contains("User guide: docs/cli.md"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "index"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "edit"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "browse"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "help"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "run"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "note"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "tasks"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .find(|command| command["name"] == "browse")
        .and_then(|command| command["after_help"].as_str())
        .expect("browse after_help should be present")
        .contains("Browse modes:"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .find(|command| command["name"] == "template")
        .and_then(|command| command["after_help"].as_str())
        .expect("template after_help should be present")
        .contains("Template source:"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .find(|command| command["name"] == "note")
        .and_then(|command| command["after_help"].as_str())
        .expect("note after_help should be present")
        .contains("Subcommands:"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .find(|command| command["name"] == "notes")
        .and_then(|command| command["after_help"].as_str())
        .expect("notes after_help should be present")
        .contains("Filter syntax:"));
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .all(|command| command["name"] != "repair"));
}

#[test]
fn help_json_output_returns_structured_topic_docs() {
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "help", "query"])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["name"], "query");
    assert_eq!(json["kind"], "command");
    assert!(json["summary"]
        .as_str()
        .expect("summary should be present")
        .contains("Run a query"));
    assert!(json["body"]
        .as_str()
        .expect("body should be present")
        .contains("Query DSL syntax:"));

    let scripting_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "help", "scripting"])
        .assert()
        .success();
    let scripting_json = parse_stdout_json(&scripting_assert);
    assert_eq!(scripting_json["kind"], "concept");
    assert!(scripting_json["body"]
        .as_str()
        .expect("body should be present")
        .contains("DataviewJS evaluation"));

    let js_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "help", "js.vault"])
        .assert()
        .success();
    let js_json = parse_stdout_json(&js_assert);
    assert_eq!(js_json["name"], "js.vault");
    assert!(js_json["body"]
        .as_str()
        .expect("body should be present")
        .contains("vault.daily.today()"));
}

#[test]
fn run_json_output_executes_script_files_and_named_scripts() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    fs::create_dir_all(vault_root.join(".vulcan/scripts")).expect("scripts dir should exist");
    fs::write(
        vault_root.join(".vulcan/scripts/runtime-demo.js"),
        concat!(
            "#!/usr/bin/env -S vulcan run --script\n",
            "console.log(help(vault.search));\n",
            "({ note: vault.note(\"Projects/Alpha\").file.name, hits: vault.search(\"Alpha\", { limit: 1 }).hits.length });\n",
        ),
    )
    .expect("named script should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            "runtime-demo",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["value"]["note"], "Alpha");
    assert_eq!(json["value"]["hits"], 1);
    let help_text = json["outputs"][0]["text"]
        .as_str()
        .expect("help text should be rendered");
    assert!(
        help_text.contains("vault.search(query: string, opts?: { limit?: number }): SearchReport")
    );
    assert!(help_text.contains("Parameters:"));
    assert!(help_text.contains("Example:"));
    assert!(help_text.contains("See also: vault.notes(), vault.query()"));
}

#[test]
fn run_json_output_reports_timeout_failures() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let script_path = vault_root.join("runtime-timeout.js");
    fs::write(&script_path, "while (true) {}\n").expect("timeout script should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            script_path
                .to_str()
                .expect("script path should be valid utf-8"),
            "--timeout",
            "200ms",
        ])
        .assert()
        .failure();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["code"], "operation_failed");
    assert!(json["error"]
        .as_str()
        .expect("error should be present")
        .contains("timed out after 200 ms"));
}

#[test]
fn run_json_output_enforces_sandbox_levels_and_supports_configured_script_roots() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::create_dir_all(vault_root.join("Runtime/Scripts")).expect("scripts dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        "[js_runtime]\ndefault_sandbox = \"fs\"\nscripts_folder = \"Runtime/Scripts\"\n",
    )
    .expect("config should be written");
    fs::write(
        vault_root.join("Runtime/Scripts/mutate.js"),
        r###"
        const created = vault.transaction((tx) => {
          const note = tx.create("Scratch", { content: "# Scratch\n\n## Log\n" });
          tx.append("Scratch", "Follow-up", { heading: "Log" });
          tx.update("Scratch", "status", "draft");
          tx.unset("Scratch", "status");
          return note;
        });
        ({ path: created.path, headings: vault.note("Scratch").headings.length, hasStatus: vault.note("Scratch").frontmatter.status !== undefined });
        "###,
    )
    .expect("script should be written");

    let strict_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            "mutate",
            "--sandbox",
            "strict",
        ])
        .assert()
        .failure();
    let strict_json = parse_stdout_json(&strict_assert);
    assert_eq!(strict_json["code"], "operation_failed");
    assert!(strict_json["error"]
        .as_str()
        .is_some_and(|message| message.contains("requires --sandbox fs or higher")));

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            "mutate",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["value"]["path"], "Scratch.md");
    assert_eq!(json["value"]["headings"], 2);
    assert_eq!(json["value"]["hasStatus"], false);

    let scratch =
        fs::read_to_string(vault_root.join("Scratch.md")).expect("scratch note should exist");
    assert!(scratch.contains("## Log"));
    assert!(scratch.contains("Follow-up"));
    assert!(!scratch.contains("status: draft"));
}

#[test]
fn run_json_output_net_sandbox_exposes_web_helpers() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    run_scan(&vault_root);

    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should have a local address");
    let base_url = format!("http://{address}");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[web.search]\nbackend = \"kagi\"\napi_key_env = \"VULCAN_JS_TEST_KAGI_KEY\"\nbase_url = \"{base_url}/search\"\n"
        ),
    )
    .expect("config should be written");
    std::env::set_var("VULCAN_JS_TEST_KAGI_KEY", "test-key");

    let handle = thread::spawn(move || {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().expect("connection should be accepted");
            let mut buffer = [0_u8; 4096];
            let read = stream
                .read(&mut buffer)
                .expect("request should be readable");
            let request = String::from_utf8_lossy(&buffer[..read]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let (content_type, body) = if path.starts_with("/search") {
                (
                    "application/json",
                    r#"{"meta":{"id":"test"},"data":[{"t":"Alpha","url":"http://example.com/alpha","snippet":"Alpha snippet"}]}"#,
                )
            } else if path == "/robots.txt" {
                ("text/plain", "User-agent: *\nAllow: /\n")
            } else {
                (
                    "text/html",
                    "<html><body><main><h1>Alpha page</h1><p>Fetched content.</p></main></body></html>",
                )
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                content_type,
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("response should be written");
        }
    });

    let script_path = vault_root.join("runtime-web.js");
    fs::write(
        &script_path,
        format!(
            r#"
            const search = web.search("Alpha", {{ limit: 1 }});
            const fetched = web.fetch("{base_url}/article", {{ mode: "markdown", extractArticle: true }});
            ({{
              title: search.results[0].title,
              status: fetched.status,
              containsAlpha: fetched.content.includes("Alpha page")
            }});
            "#
        ),
    )
    .expect("script should be written");

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "run",
            script_path
                .to_str()
                .expect("script path should be valid utf-8"),
            "--sandbox",
            "net",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    handle.join().expect("server thread should finish");

    assert_eq!(json["value"]["title"], "Alpha");
    assert_eq!(json["value"]["status"], 200);
    assert_eq!(json["value"]["containsAlpha"], true);
}

#[test]
fn describe_openai_and_mcp_formats_export_tool_definitions() {
    let openai_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "describe", "--format", "openai-tools"])
        .assert()
        .success();
    let openai_json = parse_stdout_json(&openai_assert);
    let openai_tools = openai_json["tools"]
        .as_array()
        .expect("tools should be an array");
    assert!(openai_tools
        .iter()
        .any(|tool| tool["function"]["name"] == "query"));
    assert!(openai_tools
        .iter()
        .any(|tool| tool["function"]["name"] == "note_get"));

    let mcp_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["--output", "json", "describe", "--format", "mcp"])
        .assert()
        .success();
    let mcp_json = parse_stdout_json(&mcp_assert);
    let mcp_tools = mcp_json["tools"]
        .as_array()
        .expect("tools should be an array");
    assert!(mcp_tools.iter().any(|tool| tool["name"] == "search"));
    assert!(mcp_tools
        .iter()
        .any(|tool| tool["inputSchema"]["type"] == "object"));
}

#[test]
fn completions_command_emits_shell_script() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("vulcan").and(predicate::str::contains("complete")));
}

#[test]
fn browse_requires_interactive_terminal() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["browse"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "browse requires an interactive terminal",
        ));
}

#[test]
fn fish_completions_command_emits_shell_script() {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args(["completions", "fish"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("complete -c vulcan")
                .and(predicate::str::contains("Search indexed note content")),
        );
}

#[test]
fn query_command_dsl_returns_matching_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    // DSL query: status = backlog
    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "query",
            "from notes where status = backlog",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["document_path"], "Backlog.md");
}

#[test]
fn query_command_json_payload_returns_matching_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let json_payload =
        r#"{"source":"notes","predicates":[{"field":"status","operator":"eq","value":"done"}]}"#;

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "query",
            "--json",
            json_payload,
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["document_path"], "Done.md");
}

#[test]
fn query_command_explain_includes_ast_in_json() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "query",
            "--explain",
            "from notes where status = done",
        ])
        .assert()
        .success();
    let result = parse_stdout_json(&assert);
    assert!(
        result.get("query").is_some(),
        "explain output should include query AST"
    );
    assert!(
        result.get("notes").is_some(),
        "explain output should include notes"
    );
    assert_eq!(result["query"]["source"], "notes");
}

#[test]
fn query_command_dsl_order_and_limit() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "query",
            "from notes order by file.path limit 1",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);
    assert_eq!(rows.len(), 1, "limit 1 should return exactly one note");
}

#[test]
fn query_command_rejects_both_dsl_and_json() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "query",
            "from notes",
            "--json",
            r#"{"source":"notes"}"#,
        ])
        .assert()
        .failure();
}

#[test]
fn query_command_results_match_notes_command() {
    // Prove equivalence: query DSL and notes --where produce identical results
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let query_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "query",
            "from notes where status = backlog",
        ])
        .assert()
        .success();
    let query_paths: Vec<String> = parse_stdout_json_lines(&query_assert)
        .into_iter()
        .filter_map(|v| v["document_path"].as_str().map(str::to_string))
        .collect();

    let notes_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path",
            "notes",
            "--where",
            "status = backlog",
        ])
        .assert()
        .success();
    let notes_paths: Vec<String> = parse_stdout_json_lines(&notes_assert)
        .into_iter()
        .filter_map(|v| v["document_path"].as_str().map(str::to_string))
        .collect();

    assert_eq!(
        query_paths, notes_paths,
        "query DSL and notes --where should return identical results"
    );
}

#[test]
fn query_command_matches_operator_filters_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "query",
            "--format",
            "paths",
            "from notes where file.name matches \"^D\"",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows, vec![Value::String("Done.md".to_string())]);
}

#[test]
fn ls_command_supports_glob_and_count_format() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "ls",
            "--glob",
            "D*",
            "--format",
            "count",
        ])
        .assert()
        .success();
    let json = parse_stdout_json(&assert);

    assert_eq!(json["count"], Value::Number(1.into()));
}

#[test]
fn search_regex_flag_runs_explicit_regex_queries() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "--fields",
            "document_path,matched_line",
            "search",
            "--regex",
            "release\\s+readiness",
        ])
        .assert()
        .success();
    let rows = parse_stdout_json_lines(&assert);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["document_path"], "Done.md");
    assert!(rows[0]["matched_line"].as_u64().is_some());
}

#[test]
fn update_command_sets_property_on_matching_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "update",
            "--where",
            "status = backlog",
            "--key",
            "reviewed",
            "--value",
            "true",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Applied"));

    let backlog_content =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");
    assert!(
        backlog_content.contains("reviewed: true"),
        "backlog note should have reviewed: true after update"
    );

    let done_content =
        fs::read_to_string(vault_root.join("Done.md")).expect("Done.md should be readable");
    assert!(
        done_content.contains("reviewed: true"),
        "done note should be unchanged (already true)"
    );
}

#[test]
fn update_command_dry_run_does_not_modify_files() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let original =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "update",
            "--where",
            "status = backlog",
            "--key",
            "priority",
            "--value",
            "high",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"));

    let after =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");
    assert_eq!(original, after, "dry run should not modify the file");
}

#[test]
fn unset_command_removes_property_from_matching_notes() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "unset",
            "--where",
            "status = backlog",
            "--key",
            "estimate",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Applied"));

    let backlog_content =
        fs::read_to_string(vault_root.join("Backlog.md")).expect("Backlog.md should be readable");
    assert!(
        !backlog_content.contains("estimate:"),
        "estimate property should be removed from backlog note"
    );

    let done_content =
        fs::read_to_string(vault_root.join("Done.md")).expect("Done.md should be readable");
    assert!(
        done_content.contains("estimate:"),
        "done note should be unaffected"
    );
}

#[test]
fn unset_command_dry_run_does_not_modify_files() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let original =
        fs::read_to_string(vault_root.join("Done.md")).expect("Done.md should be readable");

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "unset",
            "--where",
            "status = done",
            "--key",
            "estimate",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"));

    let after = fs::read_to_string(vault_root.join("Done.md")).expect("Done.md should be readable");
    assert_eq!(original, after, "dry run should not modify the file");
}

#[test]
fn update_command_json_output_includes_mutation_report() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("mixed-properties", &vault_root);
    run_scan(&vault_root);

    let assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "--output",
            "json",
            "update",
            "--where",
            "status = backlog",
            "--key",
            "flagged",
            "--value",
            "true",
            "--dry-run",
        ])
        .assert()
        .success();

    let json = parse_stdout_json(&assert);
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["key"], "flagged");
    assert_eq!(json["value"], "true");
    assert!(
        json["filters"].as_array().is_some(),
        "JSON output should include filters"
    );
}

#[test]
fn bases_view_add_command_creates_view_and_previews_rows() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "view-add",
            "release.base",
            "Sprint",
            "--filter",
            "status = backlog",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Sprint"));

    let contents = fs::read_to_string(vault_root.join("release.base"))
        .expect("release.base should be readable");
    assert!(
        contents.contains("Sprint"),
        "Sprint view should be in the file"
    );
}

#[test]
fn bases_view_delete_command_removes_view() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "view-delete",
            "release.base",
            "Board",
        ])
        .assert()
        .success();

    let contents = fs::read_to_string(vault_root.join("release.base"))
        .expect("release.base should be readable");
    assert!(
        !contents.contains("Board"),
        "Board view should be removed from the file"
    );
}

#[test]
fn bases_view_rename_command_renames_view() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "view-rename",
            "release.base",
            "Release Table",
            "Renamed",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Renamed"));

    let contents = fs::read_to_string(vault_root.join("release.base"))
        .expect("release.base should be readable");
    assert!(
        contents.contains("Renamed"),
        "new name should be in the file"
    );
    assert!(
        !contents.contains("Release Table"),
        "old name should be gone"
    );
}

#[test]
fn bases_view_edit_command_adds_filter() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "bases",
            "view-edit",
            "release.base",
            "Release Table",
            "--add-filter",
            "reviewed = true",
        ])
        .assert()
        .success();

    let contents = fs::read_to_string(vault_root.join("release.base"))
        .expect("release.base should be readable");
    assert!(
        contents.contains("reviewed = true"),
        "added filter should be in the file"
    );
}

#[test]
fn command_json_outputs_match_composite_snapshot() {
    assert_json_snapshot("commands_composite.json", &build_command_snapshot());
}

#[test]
fn edit_new_creates_note_and_updates_cache() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root should exist");
    run_scan(&vault_root);
    let editor = write_test_editor(temp_dir.path(), "Created by test");
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    let edit_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .env("EDITOR", editor)
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "edit",
            "--new",
            "Notes/Idea.md",
        ])
        .assert()
        .success();
    let edit_json = parse_stdout_json(&edit_assert);

    assert_eq!(edit_json["path"], "Notes/Idea.md");
    assert_eq!(edit_json["created"], true);
    assert_eq!(edit_json["rescanned"], true);
    assert_eq!(
        fs::read_to_string(vault_root.join("Notes/Idea.md"))
            .expect("new note should be readable")
            .replace("\r\n", "\n"),
        "Created by test\n"
    );

    let notes_assert = Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--output",
            "json",
            "--fields",
            "document_path",
            "notes",
        ])
        .assert()
        .success();
    let note_rows = parse_stdout_json_lines(&notes_assert);
    assert!(note_rows
        .iter()
        .any(|row| row["document_path"] == "Notes/Idea.md"));
}

#[test]
fn saved_report_and_export_outputs_match_snapshot() {
    assert_json_snapshot(
        "saved_reports_and_exports.json",
        &build_saved_report_snapshot(),
    );
}

#[test]
#[ignore = "regenerates the checked-in composite command snapshot"]
fn regenerate_command_json_snapshot() {
    write_json_snapshot("commands_composite.json", &build_command_snapshot());
}

#[test]
#[ignore = "regenerates the checked-in saved report snapshot"]
fn regenerate_saved_report_snapshot() {
    write_json_snapshot(
        "saved_reports_and_exports.json",
        &build_saved_report_snapshot(),
    );
}

fn parse_stdout_json(assert: &assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("stdout should contain valid json")
}

fn parse_stdout_json_lines(assert: &assert_cmd::assert::Assert) -> Vec<Value> {
    String::from_utf8(assert.get_output().stdout.clone())
        .expect("stdout should be valid utf-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("each line should contain valid json"))
        .collect()
}

fn document_paths(database: &CacheDatabase) -> Vec<String> {
    let mut statement = database
        .connection()
        .prepare("SELECT path FROM documents ORDER BY path")
        .expect("statement should prepare");
    let rows = statement
        .query_map([], |row| row.get(0))
        .expect("query should succeed");

    rows.map(|row| row.expect("row should deserialize"))
        .collect()
}

fn run_scan(vault_root: &Path) {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "scan",
            "--full",
        ])
        .assert()
        .success();
}

fn run_incremental_scan(vault_root: &Path) {
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            vault_root
                .to_str()
                .expect("vault path should be valid utf-8"),
            "scan",
        ])
        .assert()
        .success();
}

fn write_note_crud_sample(vault_root: &Path) {
    fs::create_dir_all(vault_root).expect("vault root should be created");
    fs::write(
        vault_root.join("Dashboard.md"),
        concat!(
            "---\n",
            "status: active\n",
            "tags:\n",
            "  - project\n",
            "---\n",
            "# Dashboard\n",
            "\n",
            "Intro line\n",
            "## Tasks\n",
            "Before\n",
            "TODO first\n",
            "Context after\n",
            "### Nested\n",
            "TODO nested\n",
            "## Done\n",
            "Finished line\n",
            "- Item line\n",
            "^done-item\n",
        ),
    )
    .expect("Dashboard.md should be written");
}

fn write_test_editor(base: &Path, body: &str) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let script = base.join("editor.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' '{body}' > \"$1\"\n"),
        )
        .expect("editor script should be written");
        let mut permissions = fs::metadata(&script)
            .expect("editor script metadata should load")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("editor script should be executable");
        format!("sh {}", script.display())
    }

    #[cfg(windows)]
    {
        let script = base.join("editor.cmd");
        fs::write(&script, format!("@echo off\r\necho {body}> %1\r\n"))
            .expect("editor script should be written");
        format!("cmd /C {}", script.display())
    }
}

fn replace_field_recursively(value: &mut Value, field: &str, replacement: &Value) {
    match value {
        Value::Object(object) => {
            if let Some(slot) = object.get_mut(field) {
                *slot = replacement.clone();
            }
            for nested in object.values_mut() {
                replace_field_recursively(nested, field, replacement);
            }
        }
        Value::Array(values) => {
            for nested in values {
                replace_field_recursively(nested, field, replacement);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn assert_json_snapshot(name: &str, value: &Value) {
    let snapshot_path = snapshot_path(name);
    let expected = fs::read_to_string(snapshot_path)
        .expect("snapshot should be readable")
        .replace("\r\n", "\n");
    let actual = serde_json::to_string_pretty(value).expect("json should serialize");

    assert_eq!(actual, expected.trim_end_matches('\n'));
}

fn assert_json_snapshot_lines(name: &str, values: &[Value]) {
    let snapshot_path = snapshot_path(name);
    let expected = fs::read_to_string(snapshot_path)
        .expect("snapshot should be readable")
        .replace("\r\n", "\n");
    let actual = serde_json::to_string_pretty(values).expect("json should serialize");

    assert_eq!(actual, expected.trim_end_matches('\n'));
}

fn write_json_snapshot(name: &str, value: &Value) {
    let snapshot_path = snapshot_path(name);
    if let Some(parent) = snapshot_path.parent() {
        fs::create_dir_all(parent).expect("snapshot directory should exist");
    }
    fs::write(
        snapshot_path,
        serde_json::to_string_pretty(value).expect("snapshot should serialize"),
    )
    .expect("snapshot should write");
}

fn snapshot_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}

#[allow(clippy::too_many_lines)]
fn build_command_snapshot() -> Value {
    let temp_dir = TempDir::new().expect("temp dir should be created");

    let init_root = temp_dir.path().join("init-vault");
    fs::create_dir_all(&init_root).expect("init vault should exist");
    let init_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                init_root
                    .to_str()
                    .expect("vault path should be valid utf-8"),
                "--output",
                "json",
                "index",
                "init",
            ])
            .assert()
            .success();
        let mut json = parse_stdout_json(&assert);
        json["vault_root"] = Value::String("<vault>".to_string());
        json["cache_path"] = Value::String("<vault>/.vulcan/cache.db".to_string());
        json["config_path"] = Value::String("<vault>/.vulcan/config.toml".to_string());
        json
    };

    let basic_root = temp_dir.path().join("basic");
    copy_fixture_vault("basic", &basic_root);
    let basic_root_str = basic_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let scan_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "index",
                "scan",
                "--full",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let rebuild_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "index",
                "rebuild",
                "--dry-run",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let repair_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "index",
                "repair",
                "fts",
                "--dry-run",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let links_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "--fields",
                "note_path,raw_text,resolved_target_path,resolution_status",
                "note",
                "links",
                "Start",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let backlinks_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "--fields",
                "note_path,source_path,raw_text",
                "note",
                "backlinks",
                "Projects/Alpha",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let search_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &basic_root_str,
                "--output",
                "json",
                "--fields",
                "document_path,heading_path,query,snippet",
                "search",
                "dashboard",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let describe_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args(["--output", "json", "describe"])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    let mixed_root = temp_dir.path().join("mixed");
    copy_fixture_vault("mixed-properties", &mixed_root);
    run_scan(&mixed_root);
    let mixed_root_str = mixed_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let notes_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &mixed_root_str,
                "--output",
                "json",
                "--fields",
                "document_path,properties",
                "notes",
                "--where",
                "estimate > 2",
                "--sort",
                "due",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };

    let bases_root = temp_dir.path().join("bases");
    copy_fixture_vault("bases", &bases_root);
    run_scan(&bases_root);
    let bases_root_str = bases_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let bases_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &bases_root_str,
                "--output",
                "json",
                "bases",
                "eval",
                "release.base",
            ])
            .assert()
            .success();
        let mut json = parse_stdout_json(&assert);
        replace_field_recursively(&mut json, "file_mtime", &serde_json::json!(0));
        json
    };
    let suggestions_root = temp_dir.path().join("suggestions");
    copy_fixture_vault("suggestions", &suggestions_root);
    run_scan(&suggestions_root);
    let suggestions_root_str = suggestions_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let suggest_mentions_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &suggestions_root_str,
                "--output",
                "json",
                "--fields",
                "source_path,matched_text,target_path,candidate_count,status",
                "refactor",
                "suggest",
                "mentions",
                "Home",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let suggest_duplicates_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &suggestions_root_str,
                "--output",
                "json",
                "--fields",
                "kind,value,left_path,right_path,score",
                "refactor",
                "suggest",
                "duplicates",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let link_mentions_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &suggestions_root_str,
                "--output",
                "json",
                "refactor",
                "link-mentions",
                "Home",
                "--dry-run",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let rewrite_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &suggestions_root_str,
                "--output",
                "json",
                "refactor",
                "rewrite",
                "--find",
                "Guide",
                "--replace",
                "Manual",
                "--dry-run",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    let move_root = temp_dir.path().join("move");
    copy_fixture_vault("move-rewrite", &move_root);
    run_scan(&move_root);
    let move_root_str = move_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let move_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &move_root_str,
                "--output",
                "json",
                "refactor",
                "move",
                "Projects/Alpha.md",
                "Archive/Alpha.md",
                "--dry-run",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    let doctor_root = temp_dir.path().join("broken");
    copy_fixture_vault("broken-frontmatter", &doctor_root);
    run_scan(&doctor_root);
    let doctor_root_str = doctor_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let doctor_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args(["--vault", &doctor_root_str, "--output", "json", "doctor"])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };

    let vectors_root = temp_dir.path().join("vectors");
    copy_fixture_vault("basic", &vectors_root);
    let server = MockEmbeddingServer::spawn();
    write_embedding_config(&vectors_root, &server.base_url());
    run_scan(&vectors_root);
    let vectors_root_str = vectors_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();
    let vectors_index_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vectors_root_str,
                "--output",
                "json",
                "vectors",
                "index",
            ])
            .assert()
            .success();
        let mut json = parse_stdout_json(&assert);
        json["elapsed_seconds"] = serde_json::json!(0.0);
        json["rate_per_second"] = serde_json::json!(0.0);
        json["endpoint_url"] = serde_json::json!("http://127.0.0.1:0/v1/embeddings");
        json
    };
    let vectors_neighbors_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vectors_root_str,
                "--output",
                "json",
                "--fields",
                "document_path,distance",
                "--limit",
                "2",
                "vectors",
                "neighbors",
                "dashboard",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let vectors_duplicates_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vectors_root_str,
                "--output",
                "json",
                "--fields",
                "left_document_path,right_document_path,similarity",
                "vectors",
                "duplicates",
                "--threshold",
                "0.7",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let cluster_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vectors_root_str,
                "--output",
                "json",
                "--fields",
                "cluster_id,cluster_label,keywords,chunk_count,document_count",
                "cluster",
                "--clusters",
                "2",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    server.shutdown();

    serde_json::json!({
        "index_init": init_json,
        "index_scan": scan_json,
        "index_rebuild": rebuild_json,
        "index_repair_fts": repair_json,
        "note_links": links_json,
        "note_backlinks": backlinks_json,
        "search": search_json,
        "notes": notes_json,
        "bases": bases_json,
        "refactor_suggest_mentions": suggest_mentions_json,
        "refactor_suggest_duplicates": suggest_duplicates_json,
        "refactor_link_mentions": link_mentions_json,
        "refactor_rewrite": rewrite_json,
        "refactor_move": move_json,
        "doctor": doctor_json,
        "describe": describe_json,
        "vectors_index": vectors_index_json,
        "vectors_neighbors": vectors_neighbors_json,
        "vectors_duplicates": vectors_duplicates_json,
        "cluster": cluster_json,
    })
}

#[allow(clippy::too_many_lines)]
fn build_saved_report_snapshot() -> Value {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let vault_root = temp_dir.path().join("bases");
    copy_fixture_vault("bases", &vault_root);
    run_scan(&vault_root);
    let vault_root_str = vault_root
        .to_str()
        .expect("vault path should be valid utf-8")
        .to_string();

    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--fields",
            "document_path,rank",
            "--limit",
            "1",
            "saved",
            "search",
            "weekly-search",
            "release",
            "--description",
            "weekly release hits",
            "--export",
            "jsonl",
            "--export-path",
            "exports/search.jsonl",
        ])
        .assert()
        .success();
    Command::cargo_bin("vulcan")
        .expect("binary should build")
        .args([
            "--vault",
            &vault_root_str,
            "--fields",
            "document_path,group_value",
            "saved",
            "bases",
            "release-table",
            "release.base",
            "--export",
            "csv",
            "--export-path",
            "exports/release.csv",
        ])
        .assert()
        .success();

    let list_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vault_root_str,
                "--output",
                "json",
                "saved",
                "list",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let show_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vault_root_str,
                "--output",
                "json",
                "saved",
                "show",
                "weekly-search",
            ])
            .assert()
            .success();
        parse_stdout_json(&assert)
    };
    let run_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vault_root_str,
                "--output",
                "json",
                "saved",
                "run",
                "weekly-search",
            ])
            .assert()
            .success();
        Value::Array(parse_stdout_json_lines(&assert))
    };
    let batch_json = {
        let assert = Command::cargo_bin("vulcan")
            .expect("binary should build")
            .args([
                "--vault",
                &vault_root_str,
                "--output",
                "json",
                "batch",
                "--all",
            ])
            .assert()
            .success();
        let mut json = parse_stdout_json(&assert);
        replace_string_recursively(&mut json, &vault_root.display().to_string(), "<vault>");
        // Normalize any remaining backslash path separators (Windows) to forward slashes.
        replace_string_recursively(&mut json, "\\", "/");
        json
    };
    let search_export = fs::read_to_string(vault_root.join("exports/search.jsonl"))
        .expect("search export should exist")
        .replace("\r\n", "\n");
    let bases_export = fs::read_to_string(vault_root.join("exports/release.csv"))
        .expect("bases export should exist")
        .replace("\r\n", "\n");

    serde_json::json!({
        "saved_list": list_json,
        "saved_show": show_json,
        "saved_run": run_json,
        "batch": batch_json,
        "search_export": search_export,
        "bases_export": bases_export,
    })
}

fn copy_fixture_vault(name: &str, destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/vaults")
        .join(name);

    copy_dir_recursive(&source, destination);
}

fn copy_dir_recursive(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("destination directory should be created");

    for entry in fs::read_dir(source).expect("source directory should be readable") {
        let entry = entry.expect("directory entry should be readable");
        let file_type = entry.file_type().expect("file type should be readable");
        let target = destination.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else if file_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).expect("parent directory should exist");
            }
            fs::copy(entry.path(), target).expect("file should be copied");
        }
    }
}

fn write_tasknotes_views_fixture(vault_root: &Path) {
    fs::create_dir_all(vault_root.join("TaskNotes/Views"))
        .expect("tasknotes views directory should be created");
    fs::write(
        vault_root.join("TaskNotes/Views/tasks-default.base"),
        concat!(
            "source:\n",
            "  type: tasknotes\n",
            "  config:\n",
            "    type: tasknotesTaskList\n",
            "    includeArchived: false\n",
            "views:\n",
            "  - type: tasknotesTaskList\n",
            "    name: Tasks\n",
            "    order:\n",
            "      - file.name\n",
            "      - priorityWeight\n",
            "      - efficiencyRatio\n",
            "      - urgencyScore\n",
            "    sort:\n",
            "      - column: file.name\n",
            "        direction: ASC\n",
        ),
    )
    .expect("tasks default base should be written");
    fs::write(
        vault_root.join("TaskNotes/Views/kanban-default.base"),
        concat!(
            "source:\n",
            "  type: tasknotes\n",
            "  config:\n",
            "    type: tasknotesKanban\n",
            "    includeArchived: false\n",
            "views:\n",
            "  - type: tasknotesKanban\n",
            "    name: Kanban Board\n",
            "    order:\n",
            "      - file.name\n",
            "      - status\n",
            "    groupBy:\n",
            "      property: status\n",
            "      direction: ASC\n",
        ),
    )
    .expect("kanban default base should be written");
}

fn write_embedding_config(vault_root: &Path, base_url: &str) {
    fs::create_dir_all(vault_root.join(".vulcan")).expect("config directory should exist");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        format!(
            "[embedding]\nprovider = \"openai-compatible\"\nbase_url = \"{base_url}\"\nmodel = \"fixture\"\nmax_batch_size = 8\nmax_concurrency = 1\n"
        ),
    )
    .expect("embedding config should be written");
}

struct MockEmbeddingServer {
    address: String,
    shutdown_tx: std::sync::mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockEmbeddingServer {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        listener
            .set_nonblocking(true)
            .expect("listener should support nonblocking mode");
        let address = listener
            .local_addr()
            .expect("listener should expose its local address");
        let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

        let handle = thread::spawn(move || loop {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }

            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("stream should support blocking mode");
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .expect("read timeout should be configurable");
                    let request = read_request(&mut stream);
                    let inputs = request
                        .body
                        .get("input")
                        .and_then(Value::as_array)
                        .expect("request should include input");
                    let body = serde_json::json!({
                        "data": inputs.iter().enumerate().map(|(index, input)| {
                            serde_json::json!({
                                "index": index,
                                "embedding": embedding_for_input(input.as_str().unwrap_or_default()),
                            })
                        }).collect::<Vec<_>>(),
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("response should write");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("unexpected mock server error: {error}"),
            }
        });

        Self {
            address: format!("http://{address}/v1"),
            shutdown_tx,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> String {
        self.address.clone()
    }

    fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.handle.take() {
            handle.join().expect("mock server should join");
        }
    }
}

struct MockWebServer {
    address: String,
    shutdown_tx: std::sync::mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockWebServer {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        listener
            .set_nonblocking(true)
            .expect("listener should support nonblocking mode");
        let address = listener
            .local_addr()
            .expect("listener should expose its local address");
        let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

        let handle = thread::spawn(move || loop {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }

            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("stream should support blocking mode");
                    let request = read_header_request(&mut stream);
                    if request.path.starts_with("/api/v0/search") {
                        let auth = request
                            .headers
                            .get("authorization")
                            .cloned()
                            .unwrap_or_default();
                        let status_line = if auth == "Bot test-token" {
                            "HTTP/1.1 200 OK"
                        } else {
                            "HTTP/1.1 401 Unauthorized"
                        };
                        let body = if auth == "Bot test-token" {
                            serde_json::json!({
                                "data": [
                                    {
                                        "title": "Release Notes",
                                        "url": "https://example.com/release",
                                        "snippet": "Everything that shipped this week."
                                    },
                                    {
                                        "title": "Status Update",
                                        "url": "https://example.com/status",
                                        "snippet": "Current project status."
                                    }
                                ]
                            })
                            .to_string()
                        } else {
                            serde_json::json!({ "error": "unauthorized" }).to_string()
                        };
                        write_http_response(
                            &mut stream,
                            status_line,
                            "application/json",
                            body.as_bytes(),
                        );
                    } else if request.path == "/robots.txt" {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 200 OK",
                            "text/plain",
                            b"User-agent: *\nDisallow:\n",
                        );
                    } else if request.path == "/article" {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 200 OK",
                            "text/html",
                            br"<!doctype html><html><body><nav>skip me</nav><article><h1>Release Summary</h1><p>Shipped &amp; stable.</p></article></body></html>",
                        );
                    } else if request.path == "/raw" {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 200 OK",
                            "application/octet-stream",
                            b"raw-body",
                        );
                    } else {
                        write_http_response(
                            &mut stream,
                            "HTTP/1.1 404 Not Found",
                            "text/plain",
                            b"not found",
                        );
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("unexpected mock server error: {error}"),
            }
        });

        Self {
            address: format!("http://{address}"),
            shutdown_tx,
            handle: Some(handle),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.address, path)
    }

    fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.handle.take() {
            handle.join().expect("mock server should join");
        }
    }
}

#[derive(Debug)]
struct CapturedRequest {
    body: Value,
}

#[derive(Debug)]
struct CapturedHeaderRequest {
    path: String,
    headers: std::collections::BTreeMap<String, String>,
}

fn read_request(stream: &mut std::net::TcpStream) -> CapturedRequest {
    let mut buffer = Vec::new();
    let mut header_end = None;

    loop {
        let mut chunk = [0_u8; 1024];
        let bytes_read = stream.read(&mut chunk).expect("request should be readable");
        if bytes_read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            header_end = Some(position + 4);
            break;
        }
    }

    let header_end = header_end.expect("request should contain headers");
    let header_text = String::from_utf8(buffer[..header_end].to_vec()).expect("utf8 headers");
    let content_length = header_text
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .expect("request should include content length");
    let mut body_bytes = buffer[header_end..].to_vec();
    while body_bytes.len() < content_length {
        let mut chunk = vec![0_u8; content_length - body_bytes.len()];
        let bytes_read = stream
            .read(chunk.as_mut_slice())
            .expect("body should be readable");
        if bytes_read == 0 {
            break;
        }
        body_bytes.extend_from_slice(&chunk[..bytes_read]);
    }

    CapturedRequest {
        body: serde_json::from_slice(&body_bytes).expect("request body should parse"),
    }
}

fn read_header_request(stream: &mut std::net::TcpStream) -> CapturedHeaderRequest {
    let mut buffer = Vec::new();

    loop {
        let mut chunk = [0_u8; 1024];
        let bytes_read = stream.read(&mut chunk).expect("request should be readable");
        if bytes_read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if find_subslice(&buffer, b"\r\n\r\n").is_some() {
            break;
        }
    }

    let request = String::from_utf8(buffer).expect("request should be utf8");
    let mut lines = request.lines();
    let request_line = lines
        .next()
        .expect("request should start with a request line");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .expect("request line should include a path")
        .to_string();
    let headers = lines
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();

    CapturedHeaderRequest { path, headers }
}

fn write_http_response(
    stream: &mut std::net::TcpStream,
    status_line: &str,
    content_type: &str,
    body: &[u8],
) {
    let headers = format!(
        "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .expect("headers should write");
    stream.write_all(body).expect("body should write");
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn embedding_for_input(input: &str) -> Vec<f32> {
    if input.contains("dashboard") || input.contains("Home links") {
        vec![1.0, 0.0]
    } else if input.contains("Bob") || input.contains("ownership") {
        vec![0.0, 1.0]
    } else if input.contains("Alpha") || input.contains("Project") {
        vec![0.75, 0.25]
    } else {
        vec![0.5, 0.5]
    }
}
