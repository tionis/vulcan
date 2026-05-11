use super::*;
use crate::trust;
use serde_json::{json, Value};
use std::env;
use std::fs;
use tempfile::TempDir;
use vulcan_core::paths::initialize_vulcan_dir;
use vulcan_core::{scan_vault, ScanMode, VaultPaths};

fn test_paths() -> (TempDir, VaultPaths) {
    let dir = TempDir::new().expect("temp dir should be created");
    let paths = VaultPaths::new(dir.path());
    initialize_vulcan_dir(&paths).expect("vault should initialize");
    (dir, paths)
}

fn write_tool(paths: &VaultPaths, name: &str, manifest: &str, source: &str) {
    let metadata = tool_test_manifest_metadata(manifest);
    let root = paths.vault_root().join(".agents/skills").join(name);
    let scripts = root.join("scripts");
    fs::create_dir_all(&scripts).expect("skill scripts dir should exist");
    fs::write(
        root.join("SKILL.md"),
        render_tool_test_skill_manifest(name, &metadata),
    )
    .expect("skill manifest should write");
    fs::write(scripts.join("main.js"), source).expect("skill source should write");
}

struct ToolTestManifestMetadata {
    tool_name: String,
    description: String,
    body: String,
    permission_profile: Option<String>,
    input_schema: Value,
    output_schema: Option<Value>,
}

fn tool_test_manifest_metadata(manifest: &str) -> ToolTestManifestMetadata {
    let frontmatter = manifest
        .trim_start()
        .strip_prefix("---")
        .and_then(|rest| rest.split_once("---"))
        .expect("test tool manifest should have frontmatter");
    let (frontmatter, body) = frontmatter;
    let value: serde_yaml::Value =
        serde_yaml::from_str(frontmatter).expect("test tool manifest should parse");
    let mapping = value
        .as_mapping()
        .expect("test tool manifest frontmatter should be a map");
    let get = |key: &str| mapping.get(serde_yaml::Value::String(key.to_string()));
    let tool_name = get("name")
        .and_then(serde_yaml::Value::as_str)
        .expect("test tool manifest should set name")
        .to_string();
    let description = get("description")
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or("Test custom tool.")
        .to_string();
    let permission_profile = get("permission_profile")
        .and_then(serde_yaml::Value::as_str)
        .map(ToOwned::to_owned);
    let input_schema = get("input_schema")
        .cloned()
        .map(serde_json::to_value)
        .transpose()
        .expect("input_schema should convert to JSON")
        .unwrap_or_else(|| json!({ "type": "object" }));
    let output_schema = get("output_schema")
        .cloned()
        .map(serde_json::to_value)
        .transpose()
        .expect("output_schema should convert to JSON");
    ToolTestManifestMetadata {
        tool_name,
        description,
        body: body.trim().to_string(),
        permission_profile,
        input_schema,
        output_schema,
    }
}

fn render_tool_test_skill_manifest(name: &str, metadata: &ToolTestManifestMetadata) -> String {
    let mut command = serde_json::Map::new();
    command.insert("id".to_string(), json!("run"));
    command.insert("description".to_string(), json!(metadata.description));
    command.insert("script".to_string(), json!("scripts/main.js"));
    command.insert("expose".to_string(), json!(true));
    command.insert(
        "cli".to_string(),
        json!({ "aliases": [metadata.tool_name.clone()] }),
    );
    command.insert("input_schema".to_string(), metadata.input_schema.clone());
    if let Some(output_schema) = &metadata.output_schema {
        command.insert("output_schema".to_string(), output_schema.clone());
    }
    if let Some(permission_profile) = &metadata.permission_profile {
        command.insert("permission_profile".to_string(), json!(permission_profile));
    }
    let manifest = json!({
        "name": name,
        "description": metadata.description,
        "metadata": {
            "vulcan": {
                "commands": [Value::Object(command)]
            }
        }
    });
    let frontmatter = serde_yaml::to_string(&manifest).expect("manifest should render");
    let body = if metadata.body.is_empty() {
        format!("# {name}")
    } else {
        metadata.body.clone()
    };
    format!("---\n{frontmatter}---\n\n{body}\n")
}

fn write_skill(paths: &VaultPaths, name: &str, manifest: &str, source_name: &str, source: &str) {
    let root = paths.vault_root().join(".agents/skills").join(name);
    let scripts = root.join("scripts");
    fs::create_dir_all(&scripts).expect("skill scripts dir should exist");
    fs::write(root.join("SKILL.md"), manifest).expect("skill manifest should write");
    fs::write(scripts.join(source_name), source).expect("skill script should write");
}

fn with_trusted_vault(paths: &VaultPaths) {
    trust::add_trust(paths.vault_root()).expect("trust should be added");
    assert!(trust::is_trusted(paths.vault_root()));
}

fn test_env_lock_guard() -> std::sync::MutexGuard<'static, ()> {
    trust::test_env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn list_custom_tools_marks_untrusted_vaults_as_not_callable() {
    let (_dir, paths) = test_paths();
    write_tool(
        &paths,
        "summary",
        r"---
name: summary_tool
description: Summarize one note.
input_schema:
  type: object
---
",
        "function main() { return null; }\n",
    );

    let tools = list_custom_tools(&paths, None, &CustomToolRegistryOptions::default())
        .expect("tools should load");
    assert_eq!(tools.len(), 1);
    assert!(!tools[0].callable);
}

#[test]
#[allow(clippy::too_many_lines)]
fn custom_tool_cli_completion_candidates_include_aliases_and_flags() {
    let (_dir, paths) = test_paths();
    write_skill(
        &paths,
        "conversation-export",
        r"---
name: conversation-export
description: Export conversations.
metadata:
  vulcan:
    commands:
      - id: export
        description: Export one conversation.
        script: scripts/export.js
        expose: true
        cli:
          aliases: [conversation-export]
          args:
            - flag: title
              action: string
              field: title
            - flag: dry-run
              action: boolean
              field: options.dry_run
            - flag: limit
              action: integer
              field: options.limit
            - flag: score
              action: number
              field: options.score
            - flag: source
              action: choice
              field: source
              choices: [chatgpt, codex]
            - flag: tag
              action: string_array
              field: tags
            - flag: user
              action: append_message
              role: user
        input_schema:
          type: object
---
# Conversation Export
",
        "export.js",
        "function main(input) { return input; }\n",
    );

    assert_eq!(
        collect_custom_tool_cli_name_candidates(&paths, &CustomToolRegistryOptions::default())
            .expect("name candidates"),
        vec![
            "skill_conversation_export_export".to_string(),
            "conversation-export".to_string()
        ]
    );
    assert_eq!(
        collect_custom_tool_cli_flag_candidates(
            &paths,
            "conversation-export",
            &CustomToolRegistryOptions::default(),
        )
        .expect("flag candidates"),
        vec![
            "--title".to_string(),
            "--dry-run".to_string(),
            "--limit".to_string(),
            "--score".to_string(),
            "--source".to_string(),
            "--tag".to_string(),
            "--user".to_string()
        ]
    );

    let (_resolved, input) = build_custom_tool_cli_input(
        &paths,
        "conversation-export",
        &[
            "--title".to_string(),
            "Chat".to_string(),
            "--dry-run".to_string(),
            "--limit".to_string(),
            "3".to_string(),
            "--score".to_string(),
            "1.5".to_string(),
            "--source".to_string(),
            "codex".to_string(),
            "--tag".to_string(),
            "alpha".to_string(),
            "--tag".to_string(),
            "beta".to_string(),
        ],
        &CustomToolRegistryOptions::default(),
    )
    .expect("cli input should build");
    assert_eq!(
        input,
        json!({
            "title": "Chat",
            "options": {
                "dry_run": true,
                "limit": 3,
                "score": 1.5
            },
            "source": "codex",
            "tags": ["alpha", "beta"]
        })
    );
    let error = build_custom_tool_cli_input(
        &paths,
        "conversation-export",
        &[
            "--title".to_string(),
            "Chat".to_string(),
            "--source".to_string(),
            "gemini".to_string(),
        ],
        &CustomToolRegistryOptions::default(),
    )
    .expect_err("invalid choice should fail");
    assert!(
        error
            .to_string()
            .contains("invalid choice `gemini` for custom CLI flag `--source`"),
        "{error}"
    );
}

#[test]
fn list_custom_tools_marks_execute_denied_profiles_as_not_callable() {
    let _lock = test_env_lock_guard();
    let (_dir, paths) = test_paths();
    let config_home = TempDir::new().expect("config home should be created");
    let previous_xdg = env::var_os("XDG_CONFIG_HOME");
    env::set_var("XDG_CONFIG_HOME", config_home.path());
    fs::write(
        paths.vault_root().join(".vulcan/config.toml"),
        r#"
[permissions.profiles.blocked]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "deny"
shell = "deny"
"#,
    )
    .expect("config should write");
    with_trusted_vault(&paths);
    write_tool(
        &paths,
        "summary",
        r"---
name: summary_tool
description: Summarize one note.
input_schema:
  type: object
---
",
        "function main() { return null; }\n",
    );

    let tools = list_custom_tools(
        &paths,
        Some("blocked"),
        &CustomToolRegistryOptions::default(),
    )
    .expect("tools should load");
    assert_eq!(tools.len(), 1);
    assert!(!tools[0].callable);

    trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
    match previous_xdg {
        Some(value) => env::set_var("XDG_CONFIG_HOME", value),
        None => env::remove_var("XDG_CONFIG_HOME"),
    }
}

#[test]
fn list_custom_tools_marks_missing_and_broader_profiles_as_not_callable() {
    let _lock = test_env_lock_guard();
    let (_dir, paths) = test_paths();
    let config_home = TempDir::new().expect("config home should be created");
    let previous_xdg = env::var_os("XDG_CONFIG_HOME");
    env::set_var("XDG_CONFIG_HOME", config_home.path());
    fs::write(
        paths.vault_root().join(".vulcan/config.toml"),
        r#"
[permissions.profiles.readonly]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.writer]
read = "all"
write = { allow = ["folder:Projects/**"] }
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.networker]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = { allow = true, domains = ["example.com"] }
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.sheller]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "allow"
"#,
    )
    .expect("config should write");
    with_trusted_vault(&paths);
    write_tool(
        &paths,
        "writer",
        r"---
name: writer_tool
description: Needs write access.
permission_profile: writer
input_schema:
  type: object
---
",
        "function main() { return null; }\n",
    );
    write_tool(
        &paths,
        "networker",
        r"---
name: networker_tool
description: Needs network access.
permission_profile: networker
input_schema:
  type: object
---
",
        "function main() { return null; }\n",
    );
    write_tool(
        &paths,
        "sheller",
        r"---
name: sheller_tool
description: Needs shell access.
permission_profile: sheller
input_schema:
  type: object
---
",
        "function main() { return null; }\n",
    );
    write_tool(
        &paths,
        "missing",
        r"---
name: missing_profile_tool
description: References a missing profile.
permission_profile: missing_profile
input_schema:
  type: object
---
",
        "function main() { return null; }\n",
    );

    let tools = list_custom_tools(
        &paths,
        Some("readonly"),
        &CustomToolRegistryOptions::default(),
    )
    .expect("tools should load");
    assert_eq!(tools.len(), 4);
    for name in [
        "skill_writer_run",
        "skill_networker_run",
        "skill_sheller_run",
        "skill_missing_run",
    ] {
        assert!(
            tools
                .iter()
                .find(|tool| tool.summary.name == name)
                .is_some_and(|tool| !tool.callable),
            "tool `{name}` should stay visible but not callable"
        );
    }

    trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
    match previous_xdg {
        Some(value) => env::set_var("XDG_CONFIG_HOME", value),
        None => env::remove_var("XDG_CONFIG_HOME"),
    }
}

#[test]
fn run_custom_tool_validates_input_and_output() {
    let _lock = test_env_lock_guard();
    let (_dir, paths) = test_paths();
    scan_vault(&paths, ScanMode::Full).expect("vault should scan");
    let config_home = TempDir::new().expect("config home should be created");
    let previous_xdg = env::var_os("XDG_CONFIG_HOME");
    env::set_var("XDG_CONFIG_HOME", config_home.path());
    with_trusted_vault(&paths);
    write_tool(
            &paths,
            "remote",
            r"---
name: remote_tool
description: Reads one note argument.
input_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
  required:
    - note
output_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
    command:
      type: string
  required:
    - note
    - command
---
",
            "function main(input, ctx) {\n  return {\n    note: input.note,\n    command: ctx.command.id,\n  };\n}\n",
        );

    let report = run_custom_tool(
        &paths,
        None,
        "remote_tool",
        &json!({ "note": "Projects/Alpha.md" }),
        &CustomToolRegistryOptions::default(),
        &CustomToolRunOptions {
            surface: "cli".to_string(),
        },
    )
    .expect("tool should run");

    assert_eq!(report.name, "remote_tool");
    assert_eq!(report.result["note"], json!("Projects/Alpha.md"));
    assert_eq!(report.result["command"], json!("run"));
    assert_eq!(report.text, None);

    trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
    match previous_xdg {
        Some(value) => env::set_var("XDG_CONFIG_HOME", value),
        None => env::remove_var("XDG_CONFIG_HOME"),
    }
}

#[test]
fn run_custom_tool_rejects_missing_permission_profiles() {
    let _lock = test_env_lock_guard();
    let (_dir, paths) = test_paths();
    let config_home = TempDir::new().expect("config home should be created");
    let previous_xdg = env::var_os("XDG_CONFIG_HOME");
    env::set_var("XDG_CONFIG_HOME", config_home.path());
    scan_vault(&paths, ScanMode::Full).expect("vault should scan");
    with_trusted_vault(&paths);
    write_tool(
        &paths,
        "missing-profile",
        r"---
name: missing_profile_tool
description: References a profile that does not exist.
permission_profile: missing_profile
input_schema:
  type: object
---
",
        "function main() { return null; }\n",
    );

    let error = run_custom_tool(
        &paths,
        None,
        "missing_profile_tool",
        &json!({}),
        &CustomToolRegistryOptions::default(),
        &CustomToolRunOptions::default(),
    )
    .expect_err("missing tool profile should fail");
    assert!(error
        .to_string()
        .contains("unknown permission profile `missing_profile`"));

    trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
    match previous_xdg {
        Some(value) => env::set_var("XDG_CONFIG_HOME", value),
        None => env::remove_var("XDG_CONFIG_HOME"),
    }
}

#[test]
fn run_custom_tool_surfaces_runtime_script_errors() {
    let _lock = test_env_lock_guard();
    let (_dir, paths) = test_paths();
    let config_home = TempDir::new().expect("config home should be created");
    let previous_xdg = env::var_os("XDG_CONFIG_HOME");
    env::set_var("XDG_CONFIG_HOME", config_home.path());
    scan_vault(&paths, ScanMode::Full).expect("vault should scan");
    with_trusted_vault(&paths);
    write_tool(
        &paths,
        "broken",
        r"---
name: broken_tool
description: Throws from JS.
input_schema:
  type: object
---
",
        "function main() { throw new Error('boom'); }\n",
    );

    let error = run_custom_tool(
        &paths,
        None,
        "broken_tool",
        &json!({}),
        &CustomToolRegistryOptions::default(),
        &CustomToolRunOptions::default(),
    )
    .expect_err("runtime failure should surface");
    assert!(error.to_string().contains("boom"));

    trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
    match previous_xdg {
        Some(value) => env::set_var("XDG_CONFIG_HOME", value),
        None => env::remove_var("XDG_CONFIG_HOME"),
    }
}

#[test]
fn run_custom_tool_rejects_output_schema_mismatches() {
    let _lock = test_env_lock_guard();
    let (_dir, paths) = test_paths();
    let config_home = TempDir::new().expect("config home should be created");
    let previous_xdg = env::var_os("XDG_CONFIG_HOME");
    env::set_var("XDG_CONFIG_HOME", config_home.path());
    scan_vault(&paths, ScanMode::Full).expect("vault should scan");
    with_trusted_vault(&paths);
    write_tool(
        &paths,
        "mismatch",
        r"---
name: mismatch_tool
description: Returns the wrong shape.
input_schema:
  type: object
output_schema:
  type: object
  additionalProperties: false
  properties:
    ok:
      type: boolean
  required:
    - ok
---
",
        "function main() { return { ok: 'nope' }; }\n",
    );

    let error = run_custom_tool(
        &paths,
        None,
        "mismatch_tool",
        &json!({}),
        &CustomToolRegistryOptions::default(),
        &CustomToolRunOptions::default(),
    )
    .expect_err("output schema mismatch should fail");
    assert!(error
        .to_string()
        .contains("tool `mismatch_tool` output validation failed"));

    trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
    match previous_xdg {
        Some(value) => env::set_var("XDG_CONFIG_HOME", value),
        None => env::remove_var("XDG_CONFIG_HOME"),
    }
}

#[test]
fn run_custom_tool_rejects_broader_permission_profiles() {
    let _lock = test_env_lock_guard();
    let (_dir, paths) = test_paths();
    let config_home = TempDir::new().expect("config home should be created");
    let previous_xdg = env::var_os("XDG_CONFIG_HOME");
    env::set_var("XDG_CONFIG_HOME", config_home.path());
    scan_vault(&paths, ScanMode::Full).expect("vault should scan");
    fs::write(
        paths.vault_root().join(".vulcan/config.toml"),
        r#"
[permissions.profiles.agent]
read = "all"
write = { allow = ["folder:Projects/**"] }
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.readonly]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"
"#,
    )
    .expect("config should write");
    with_trusted_vault(&paths);
    write_tool(
        &paths,
        "restricted",
        r"---
name: restricted_tool
description: Needs agent profile.
permission_profile: agent
input_schema:
  type: object
---
",
        "function main() { return null; }\n",
    );

    let error = run_custom_tool(
        &paths,
        Some("readonly"),
        "restricted_tool",
        &json!({}),
        &CustomToolRegistryOptions::default(),
        &CustomToolRunOptions::default(),
    )
    .expect_err("broader requested profile should fail");
    assert!(error
        .to_string()
        .contains("tool `restricted_tool` requires permission profile `agent`"));
    let listed = list_custom_tools(
        &paths,
        Some("readonly"),
        &CustomToolRegistryOptions::default(),
    )
    .expect("tools should list");
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].callable);
    trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
    match previous_xdg {
        Some(value) => env::set_var("XDG_CONFIG_HOME", value),
        None => env::remove_var("XDG_CONFIG_HOME"),
    }
}

#[test]
fn custom_tools_can_list_get_and_call_other_tools_from_js() {
    let _lock = test_env_lock_guard();
    let (_dir, paths) = test_paths();
    let config_home = TempDir::new().expect("config home should be created");
    let previous_xdg = env::var_os("XDG_CONFIG_HOME");
    env::set_var("XDG_CONFIG_HOME", config_home.path());
    scan_vault(&paths, ScanMode::Full).expect("vault should scan");
    with_trusted_vault(&paths);
    write_tool(
        &paths,
        "inner",
        r"---
name: inner_tool
description: Inner helper.
input_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
  required:
    - note
---

Inner tool documentation.
",
        "function main(input) {\n  return { echoed: String(input.note).toUpperCase() };\n}\n",
    );
    write_tool(
            &paths,
            "outer",
            r"---
name: outer_tool
description: Outer helper.
input_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
  required:
    - note
---

Outer tool documentation.
",
            "function main(input) {\n  const normalized = tool.input({ fallback: true });\n  const listed = tools.list();\n  const described = tools.get('inner_tool');\n  const called = tools.callChecked('inner_tool', { note: input.note });\n  return tool.result().summary('nested call complete').data({\n    listed: listed.map((tool) => tool.name),\n    callable: listed.every((tool) => tool.callable === true),\n    body_has_doc: described.body.includes('Inner tool documentation.'),\n    echoed: called.expect('echoed'),\n    fallback: normalized.fallback,\n  }).ok();\n}\n",
        );

    let report = run_custom_tool(
        &paths,
        None,
        "outer_tool",
        &json!({ "note": "alpha" }),
        &CustomToolRegistryOptions::default(),
        &CustomToolRunOptions {
            surface: "cli".to_string(),
        },
    )
    .expect("nested tool calls should succeed");

    assert_eq!(report.result["ok"], json!(true));
    assert_eq!(report.result["summary"], json!("nested call complete"));
    assert_eq!(
        report.result["data"]["listed"],
        json!(["skill_inner_run", "skill_outer_run"])
    );
    assert_eq!(report.result["data"]["callable"], json!(true));
    assert_eq!(report.result["data"]["body_has_doc"], json!(true));
    assert_eq!(report.result["data"]["echoed"], json!("ALPHA"));
    assert_eq!(report.result["data"]["fallback"], json!(true));

    trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
    match previous_xdg {
        Some(value) => env::set_var("XDG_CONFIG_HOME", value),
        None => env::remove_var("XDG_CONFIG_HOME"),
    }
}

#[test]
fn tools_namespace_preserves_nested_permission_ceiling() {
    let _lock = test_env_lock_guard();
    let (_dir, paths) = test_paths();
    let config_home = TempDir::new().expect("config home should be created");
    let previous_xdg = env::var_os("XDG_CONFIG_HOME");
    env::set_var("XDG_CONFIG_HOME", config_home.path());
    scan_vault(&paths, ScanMode::Full).expect("vault should scan");
    fs::write(
        paths.vault_root().join(".vulcan/config.toml"),
        r#"
[permissions.profiles.agent]
read = "all"
write = { allow = ["folder:Projects/**"] }
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.readonly]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"
"#,
    )
    .expect("config should write");
    with_trusted_vault(&paths);
    write_tool(
        &paths,
        "inner",
        r"---
name: privileged_inner
description: Needs agent profile.
permission_profile: agent
input_schema:
  type: object
---
",
        "function main() { return { ok: true }; }\n",
    );
    write_tool(
        &paths,
        "outer",
        r"---
name: readonly_outer
description: Calls another tool.
permission_profile: readonly
input_schema:
  type: object
---
",
        "function main() { return tools.call('privileged_inner', {}); }\n",
    );

    let error = run_custom_tool(
        &paths,
        None,
        "readonly_outer",
        &json!({}),
        &CustomToolRegistryOptions::default(),
        &CustomToolRunOptions {
            surface: "cli".to_string(),
        },
    )
    .expect_err("nested broader tool should fail");
    assert!(error.to_string().contains(
            "tool `privileged_inner` requires permission profile `agent`, which is broader than active profile `readonly`"
        ));

    trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
    match previous_xdg {
        Some(value) => env::set_var("XDG_CONFIG_HOME", value),
        None => env::remove_var("XDG_CONFIG_HOME"),
    }
}

#[test]
fn tools_namespace_rejects_recursive_tool_loops() {
    let _lock = test_env_lock_guard();
    let (_dir, paths) = test_paths();
    let config_home = TempDir::new().expect("config home should be created");
    let previous_xdg = env::var_os("XDG_CONFIG_HOME");
    env::set_var("XDG_CONFIG_HOME", config_home.path());
    scan_vault(&paths, ScanMode::Full).expect("vault should scan");
    with_trusted_vault(&paths);
    write_tool(
        &paths,
        "loop",
        r"---
name: loop_tool
description: Calls itself.
input_schema:
  type: object
---
",
        "function main() { return tools.call('loop_tool', {}); }\n",
    );

    let error = run_custom_tool(
        &paths,
        None,
        "loop_tool",
        &json!({}),
        &CustomToolRegistryOptions::default(),
        &CustomToolRunOptions {
            surface: "cli".to_string(),
        },
    )
    .expect_err("recursive tool call should fail");
    assert!(error
        .to_string()
        .contains("recursive custom tool call detected: loop_tool -> loop_tool"));

    trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
    match previous_xdg {
        Some(value) => env::set_var("XDG_CONFIG_HOME", value),
        None => env::remove_var("XDG_CONFIG_HOME"),
    }
}

#[test]
fn json_schema_typescript_supports_composed_tool_schemas() {
    let schema = json!({
        "type": "object",
        "required": ["mode", "payload"],
        "properties": {
            "mode": { "const": "append" },
            "payload": {
                "anyOf": [
                    { "type": "string" },
                    { "type": "array", "items": { "type": ["string", "null"] } }
                ]
            },
            "labels": {
                "type": "object",
                "additionalProperties": { "type": "number" }
            },
            "status": { "enum": ["open", "done", null] }
        },
        "additionalProperties": false
    });

    let typescript = json_schema_to_typescript(&schema, 0);

    assert!(typescript.contains("mode: \"append\";"));
    assert!(typescript.contains("payload: string | (string | null)[];"));
    assert!(typescript.contains("labels?: Record<string, number>;"));
    assert!(typescript.contains("status?: \"open\" | \"done\" | null;"));
    assert!(!typescript.contains("[key: string]"));
}
