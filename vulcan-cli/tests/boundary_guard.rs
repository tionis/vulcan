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
