use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;
use tempfile::TempDir;
use vulcan_core::{CacheDatabase, VaultPaths};

#[test]
fn help_mentions_global_flags_and_core_commands() {
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    command.arg("--help").assert().success().stdout(
        predicate::str::contains("--vault <VAULT>")
            .and(predicate::str::contains("--output <OUTPUT>"))
            .and(predicate::str::contains("--verbose"))
            .and(predicate::str::contains("init"))
            .and(predicate::str::contains("scan"))
            .and(predicate::str::contains("rebuild"))
            .and(predicate::str::contains("repair"))
            .and(predicate::str::contains("watch"))
            .and(predicate::str::contains("links"))
            .and(predicate::str::contains("backlinks"))
            .and(predicate::str::contains("graph"))
            .and(predicate::str::contains("notes"))
            .and(predicate::str::contains("bases"))
            .and(predicate::str::contains("search"))
            .and(predicate::str::contains("vectors"))
            .and(predicate::str::contains("cluster"))
            .and(predicate::str::contains("move"))
            .and(predicate::str::contains("doctor"))
            .and(predicate::str::contains("cache"))
            .and(predicate::str::contains("rename-property"))
            .and(predicate::str::contains("merge-tags"))
            .and(predicate::str::contains("rename-alias"))
            .and(predicate::str::contains("rename-heading"))
            .and(predicate::str::contains("rename-block-ref"))
            .and(predicate::str::contains("saved"))
            .and(predicate::str::contains("batch"))
            .and(predicate::str::contains("describe"))
            .and(predicate::str::contains("completions"))
            .and(predicate::str::contains(
                "Initialize .vulcan/ state for a vault",
            ))
            .and(predicate::str::contains("Search indexed note content"))
            .and(predicate::str::contains(
                "Generate shell completion scripts",
            ))
            .and(predicate::str::contains("Command Groups:"))
            .and(predicate::str::contains(
                "Indexing: init, scan, rebuild, repair, watch",
            ))
            .and(predicate::str::contains(
                "Graph and Query: links, backlinks, graph, search, notes, bases",
            ))
            .and(predicate::str::contains(
                "Reports and Automation: saved, batch",
            ))
            .and(predicate::str::contains(
                "Maintenance: move, doctor, cache, rename-property, merge-tags, rename-alias, rename-heading, rename-block-ref, describe, completions",
            )),
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
        "*\n!.gitignore\n!config.toml\n!reports/\nreports/*\n!reports/*.toml\n"
    );
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
        json
    };
    assert_eq!(batch_json["succeeded"], 2);
    assert_eq!(batch_json["failed"], 0);
    assert!(vault_root.join("exports/search.jsonl").exists());
    assert!(vault_root.join("exports/release.csv").exists());
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
    assert!(json["commands"]
        .as_array()
        .expect("commands should be an array")
        .iter()
        .any(|command| command["name"] == "repair"));
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
fn command_json_outputs_match_composite_snapshot() {
    assert_json_snapshot("commands_composite.json", &build_command_snapshot());
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
    let expected = fs::read_to_string(snapshot_path).expect("snapshot should be readable");
    let actual = serde_json::to_string_pretty(value).expect("json should serialize");

    assert_eq!(actual, expected.trim_end_matches('\n'));
}

fn assert_json_snapshot_lines(name: &str, values: &[Value]) {
    let snapshot_path = snapshot_path(name);
    let expected = fs::read_to_string(snapshot_path).expect("snapshot should be readable");
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
        "init": init_json,
        "scan": scan_json,
        "rebuild": rebuild_json,
        "repair_fts": repair_json,
        "links": links_json,
        "backlinks": backlinks_json,
        "search": search_json,
        "notes": notes_json,
        "bases": bases_json,
        "move": move_json,
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
        json
    };
    let search_export = fs::read_to_string(vault_root.join("exports/search.jsonl"))
        .expect("search export should exist");
    let bases_export = fs::read_to_string(vault_root.join("exports/release.csv"))
        .expect("bases export should exist");

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

#[derive(Debug)]
struct CapturedRequest {
    body: Value,
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
