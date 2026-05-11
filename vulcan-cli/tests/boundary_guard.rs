use std::fs;
use std::path::Path;

fn visit_rs_files(root: &Path, callback: &mut dyn FnMut(&Path)) {
    let mut entries = fs::read_dir(root)
        .expect("source dir should be readable")
        .map(|entry| entry.expect("dir entry should be readable").path())
        .collect::<Vec<_>>();
    entries.sort();

    for path in entries {
        if path.is_dir() {
            visit_rs_files(&path, callback);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            callback(&path);
        }
    }
}

fn production_source(source: &str) -> &str {
    source
        .split("\n#[cfg(test)]")
        .next()
        .expect("split should always yield a first segment")
}

fn is_test_module(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("tests.rs")
        || path
            .components()
            .any(|component| component.as_os_str().to_str() == Some("tests"))
}

#[test]
fn production_cli_code_avoids_direct_backend_dependencies() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let banned_patterns = [
        (
            "reqwest::",
            "web client access belongs below the CLI boundary",
        ),
        (
            "rusqlite::",
            "raw SQLite access belongs below the CLI boundary",
        ),
        (
            "serde_yaml::",
            "runtime YAML parsing belongs below the CLI boundary",
        ),
        (
            "CacheDatabase::open(",
            "cache inspection should use shared services",
        ),
        (
            ".connection().prepare(",
            "raw SQL preparation should use shared services",
        ),
        (
            ".connection().query_row(",
            "raw SQL reads should use shared services",
        ),
        (
            ".connection().execute(",
            "raw SQL writes should use shared services",
        ),
        (
            "\"SELECT ",
            "embedded SQL should live below the CLI boundary",
        ),
        (
            "\"UPDATE documents SET",
            "embedded SQL should live below the CLI boundary",
        ),
        (
            "\"INSERT INTO ",
            "embedded SQL should live below the CLI boundary",
        ),
        (
            "\"DELETE FROM ",
            "embedded SQL should live below the CLI boundary",
        ),
    ];

    let mut violations = Vec::new();
    visit_rs_files(&src_root, &mut |path| {
        if is_test_module(path) {
            return;
        }

        let source = fs::read_to_string(path).expect("source file should read");
        let production = production_source(&source);
        for (pattern, reason) in banned_patterns {
            if production.contains(pattern) {
                let relative = path
                    .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
                    .expect("path should be inside crate root");
                violations.push(format!(
                    "{} contains `{}` ({})",
                    relative.display(),
                    pattern,
                    reason
                ));
            }
        }
    });

    assert!(
        violations.is_empty(),
        "CLI boundary violations found:\n{}",
        violations.join("\n")
    );
}

#[test]
fn core_optional_backend_dependencies_stay_feature_gated() {
    let core_manifest = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("cli crate should have workspace parent")
            .join("vulcan-core/Cargo.toml"),
    )
    .expect("core manifest should read");

    for dependency in [
        "base64",
        "jsonwebtoken",
        "reqwest",
        "rs-trafilatura",
        "sha2",
        "vulcan-embed",
    ] {
        let pattern = format!("{dependency} = {{");
        let line = core_manifest
            .lines()
            .find(|line| line.trim_start().starts_with(&pattern))
            .unwrap_or_else(|| panic!("{dependency} dependency should be declared explicitly"));
        assert!(
            line.contains("optional = true"),
            "{dependency} must stay optional in vulcan-core: {line}"
        );
    }

    for feature in ["oauth =", "vectors =", "web ="] {
        assert!(
            core_manifest.contains(feature),
            "vulcan-core must expose a `{feature}` feature"
        );
    }
}

#[test]
fn cli_dependencies_do_not_force_optional_backend_features() {
    let cli_manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("cli manifest should read");

    for dependency in ["vulcan-app", "vulcan-core"] {
        let pattern = format!("{dependency} = {{");
        let line = cli_manifest
            .lines()
            .find(|line| line.trim_start().starts_with(&pattern))
            .unwrap_or_else(|| panic!("{dependency} dependency should be declared explicitly"));
        assert!(
            !line.contains("features = ["),
            "{dependency} must not force optional backend features in no-default CLI builds: {line}"
        );
        assert!(
            line.contains("default-features = false"),
            "{dependency} must keep default features disabled at the dependency edge: {line}"
        );
    }
}

#[test]
fn app_production_code_avoids_cli_and_terminal_dependencies() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate should have workspace parent");
    let app_src_root = workspace_root.join("vulcan-app/src");
    let banned_patterns = [
        ("clap::", "argument parsing belongs in vulcan-cli"),
        ("ratatui::", "TUI rendering belongs in vulcan-cli"),
        ("crossterm::", "terminal control belongs in vulcan-cli"),
        ("anstyle::", "terminal styling belongs in vulcan-cli"),
        ("println!", "terminal output belongs in vulcan-cli"),
        ("eprintln!", "terminal output belongs in vulcan-cli"),
        ("print!", "terminal output belongs in vulcan-cli"),
        ("eprint!", "terminal output belongs in vulcan-cli"),
    ];

    let mut violations = Vec::new();
    visit_rs_files(&app_src_root, &mut |path| {
        if is_test_module(path) {
            return;
        }

        let source = fs::read_to_string(path).expect("source file should read");
        let production = production_source(&source);
        for (pattern, reason) in banned_patterns {
            if production.contains(pattern) {
                violations.push(format!(
                    "{} contains `{}` ({})",
                    path.strip_prefix(workspace_root)
                        .expect("path should be inside workspace")
                        .display(),
                    pattern,
                    reason
                ));
            }
        }
    });

    assert!(
        violations.is_empty(),
        "app boundary violations found:\n{}",
        violations.join("\n")
    );
}

#[test]
fn core_production_code_avoids_daemon_runtime_crates() {
    let core_src_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate should have workspace parent")
        .join("vulcan-core/src");
    let banned_patterns = [
        ("tokio::", "async runtime belongs at the daemon boundary"),
        ("axum::", "HTTP routing belongs at the daemon boundary"),
    ];

    let mut violations = Vec::new();
    visit_rs_files(&core_src_root, &mut |path| {
        let source = fs::read_to_string(path).expect("source file should read");
        let production = production_source(&source);
        for (pattern, reason) in banned_patterns {
            if production.contains(pattern) {
                let relative = path
                    .strip_prefix(core_src_root.parent().expect("core src has parent"))
                    .expect("path should be inside core crate");
                violations.push(format!(
                    "{} contains `{}` ({})",
                    relative.display(),
                    pattern,
                    reason
                ));
            }
        }
    });

    assert!(
        violations.is_empty(),
        "core boundary violations found:\n{}",
        violations.join("\n")
    );
}

#[test]
fn js_runtime_usage_stays_in_js_gated_modules() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate should have workspace parent");
    let scanned_roots = [
        workspace_root.join("vulcan-core/src"),
        workspace_root.join("vulcan-app/src"),
        workspace_root.join("vulcan-cli/src"),
    ];
    let allowed_files = [
        workspace_root.join("vulcan-core/src/dataview_js.rs"),
        workspace_root.join("vulcan-app/src/templates.rs"),
    ];

    let mut violations = Vec::new();
    for root in scanned_roots {
        visit_rs_files(&root, &mut |path| {
            if is_test_module(path) {
                return;
            }
            let source = fs::read_to_string(path).expect("source file should read");
            let production = production_source(&source);
            if !production.contains("rquickjs::") {
                return;
            }
            if !allowed_files.iter().any(|allowed| allowed == path) {
                violations.push(format!(
                    "{} uses rquickjs outside the approved JS runtime modules",
                    path.strip_prefix(workspace_root)
                        .expect("path should be inside workspace")
                        .display()
                ));
                return;
            }
            if !production.contains("feature = \"js_runtime\"") {
                violations.push(format!(
                    "{} uses rquickjs without an explicit js_runtime cfg guard",
                    path.strip_prefix(workspace_root)
                        .expect("path should be inside workspace")
                        .display()
                ));
            }
        });
    }

    assert!(
        violations.is_empty(),
        "JS runtime boundary violations found:\n{}",
        violations.join("\n")
    );
}

#[test]
fn vector_dependency_usage_stays_in_vector_gated_modules() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate should have workspace parent");
    let scanned_roots = [
        workspace_root.join("vulcan-core/src"),
        workspace_root.join("vulcan-app/src"),
        workspace_root.join("vulcan-cli/src"),
    ];
    let allowed_files = [
        workspace_root.join("vulcan-core/src/cache/mod.rs"),
        workspace_root.join("vulcan-core/src/suggestions.rs"),
        workspace_root.join("vulcan-core/src/vector.rs"),
    ];
    let vector_patterns = ["vulcan_embed::", "sqlite_vec"];

    let mut violations = Vec::new();
    for root in scanned_roots {
        visit_rs_files(&root, &mut |path| {
            if is_test_module(path) {
                return;
            }
            let source = fs::read_to_string(path).expect("source file should read");
            let production = production_source(&source);
            if !vector_patterns
                .iter()
                .any(|pattern| production.contains(pattern))
            {
                return;
            }
            if !allowed_files.iter().any(|allowed| allowed == path) {
                violations.push(format!(
                    "{} uses vector backend dependencies outside vector.rs",
                    path.strip_prefix(workspace_root)
                        .expect("path should be inside workspace")
                        .display()
                ));
                return;
            }
            if path.file_name().and_then(|name| name.to_str()) == Some("vector.rs") {
                return;
            }
            if !production.contains("feature = \"vectors\"") {
                violations.push(format!(
                    "{} uses vector backend dependencies without an explicit vectors cfg guard",
                    path.strip_prefix(workspace_root)
                        .expect("path should be inside workspace")
                        .display()
                ));
            }
        });
    }

    assert!(
        violations.is_empty(),
        "vector dependency boundary violations found:\n{}",
        violations.join("\n")
    );
}

#[test]
fn mcp_transport_code_avoids_terminal_rendering_dependencies() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate should have workspace parent");
    let mcp_roots = [
        workspace_root.join("vulcan-cli/src/mcp.rs"),
        workspace_root.join("vulcan-cli/src/mcp"),
    ];
    let banned_patterns = [
        (
            "crate::output",
            "presentation formatting belongs outside MCP transport",
        ),
        (
            "crate::terminal_markdown",
            "terminal rendering belongs outside MCP transport",
        ),
        (
            "crate::browse_tui",
            "interactive TUI state belongs outside MCP transport",
        ),
        (
            "crate::bases_tui",
            "interactive TUI state belongs outside MCP transport",
        ),
        (
            "crate::config_tui",
            "interactive TUI state belongs outside MCP transport",
        ),
        (
            "crate::editor",
            "editor launching belongs outside MCP transport",
        ),
        (
            "ratatui::",
            "terminal UI dependencies belong outside MCP transport",
        ),
        (
            "crossterm::",
            "terminal UI dependencies belong outside MCP transport",
        ),
        (
            "anstyle::",
            "terminal styling belongs outside MCP transport",
        ),
    ];

    let mut violations = Vec::new();
    for root in mcp_roots {
        if root.is_dir() {
            visit_rs_files(&root, &mut |path| {
                collect_mcp_terminal_dependency_violations(
                    workspace_root,
                    path,
                    &banned_patterns,
                    &mut violations,
                );
            });
        } else {
            collect_mcp_terminal_dependency_violations(
                workspace_root,
                &root,
                &banned_patterns,
                &mut violations,
            );
        }
    }

    assert!(
        violations.is_empty(),
        "MCP terminal/rendering boundary violations found:\n{}",
        violations.join("\n")
    );
}

fn collect_mcp_terminal_dependency_violations(
    workspace_root: &Path,
    path: &Path,
    banned_patterns: &[(&str, &str)],
    violations: &mut Vec<String>,
) {
    if is_test_module(path) {
        return;
    }
    let source = fs::read_to_string(path).expect("source file should read");
    let production = production_source(&source);
    for (pattern, reason) in banned_patterns {
        if production.contains(pattern) {
            violations.push(format!(
                "{} contains `{}` ({})",
                path.strip_prefix(workspace_root)
                    .expect("path should be inside workspace")
                    .display(),
                pattern,
                reason
            ));
        }
    }
}
