//! Numerals calculator blocks (`vulcan numerals`): evaluate a note's blocks
//! and inline calculations, and plan the `@[name::result]` rewrites that
//! `vulcan numerals apply` writes. Rendering never writes these values; see
//! `vulcan_core::numerals` and `docs/specs/live-documents.md` (9.42.4).

use serde::{Deserialize, Serialize};
use vulcan_core::numerals::{
    evaluate_numerals_in_source, numerals_insertion_edits, NumeralsRegion,
};
use vulcan_core::{
    parse_document, PermissionGuard, ProfilePermissionGuard, VaultConfig, VaultPaths,
};

use crate::AppError;

/// A note's evaluated Numerals regions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumeralsReport {
    pub path: String,
    pub enabled: bool,
    pub regions: Vec<NumeralsRegion>,
}

/// One `@[name::result]` line `apply` rewrites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NumeralsChange {
    /// One-based line number in the note.
    pub line: usize,
    pub before: String,
    pub after: String,
}

/// The note after its insertions are written.
#[derive(Debug, Clone)]
pub struct NumeralsApplyPlan {
    pub updated: String,
    pub changes: Vec<NumeralsChange>,
}

/// Evaluate `source`, the note at `path`, under `permission_profile`: the
/// note's scope and its cross-note references come from the notes the
/// profile can read.
pub fn evaluate_numerals_note(
    paths: &VaultPaths,
    config: &VaultConfig,
    path: &str,
    source: &str,
    permission_profile: Option<&str>,
) -> Result<NumeralsReport, AppError> {
    let parsed = parse_document(source, config);
    let note_index = match permission_profile {
        Some(_) => {
            let selection = vulcan_core::resolve_permission_profile(paths, permission_profile)
                .map_err(AppError::operation)?;
            let guard = ProfilePermissionGuard::new(paths, selection);
            guard.check_read_path(path).map_err(AppError::operation)?;
            vulcan_core::properties::load_note_index_with_guard(paths, &guard)
        }
        None => vulcan_core::properties::load_note_index(paths),
    }
    .ok();
    Ok(NumeralsReport {
        path: path.to_string(),
        enabled: config.numerals.enabled,
        regions: evaluate_numerals_in_source(
            config,
            note_index.as_ref(),
            Some(path),
            source,
            &parsed,
        ),
    })
}

/// The `@[name::result]` rewrites for the evaluated note, as the plugin
/// makes them when it renders a block.
#[must_use]
pub fn plan_numerals_apply(source: &str, report: &NumeralsReport) -> NumeralsApplyPlan {
    let edits = numerals_insertion_edits(source, &report.regions);
    let mut lines = source
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut changes = Vec::new();
    for (index, after) in edits {
        if let Some(line) = lines.get_mut(index) {
            changes.push(NumeralsChange {
                line: index + 1,
                before: std::mem::replace(line, after.clone()),
                after,
            });
        }
    }
    NumeralsApplyPlan {
        updated: lines.join("\n"),
        changes,
    }
}

#[cfg(all(test, feature = "js_runtime"))]
mod tests {
    use super::*;

    fn vault(config: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, VaultPaths) {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join(".vulcan")).expect(".vulcan");
        std::fs::write(dir.path().join(".vulcan/config.toml"), config).expect("config");
        for (path, source) in files {
            std::fs::write(dir.path().join(path), source).expect("write");
        }
        let paths = VaultPaths::new(dir.path());
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).expect("scan");
        (dir, paths)
    }

    #[test]
    fn apply_plans_insertions_from_note_scope_and_references() {
        let note = concat!(
            "---\nnumerals: all\nrate: 150\n---\n",
            "```math\nhours = 3\n@[cost::0] = rate * hours + [[Prices]].$fee\n```\n",
        );
        let (_dir, paths) = vault(
            "[numerals]\nenabled = true\n",
            &[("Bill.md", note), ("Prices.md", "---\n$fee: 50\n---\n")],
        );
        let config = vulcan_core::load_vault_config(&paths).config;
        let report =
            evaluate_numerals_note(&paths, &config, "Bill.md", note, None).expect("evaluate");
        let plan = plan_numerals_apply(note, &report);
        assert_eq!(
            plan.changes,
            vec![NumeralsChange {
                line: 7,
                before: "@[cost::0] = rate * hours + [[Prices]].$fee".to_string(),
                after: "@[cost::500] = rate * hours + [[Prices]].$fee".to_string(),
            }]
        );
        assert!(plan.updated.contains("@[cost::500]"));
    }
}
