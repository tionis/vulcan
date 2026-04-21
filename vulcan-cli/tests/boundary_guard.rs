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
