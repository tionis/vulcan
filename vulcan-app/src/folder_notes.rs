use crate::config::{
    apply_config_batch_report, plan_config_batch_report, ConfigMutationOperation, ConfigTarget,
};
use crate::export::execute_export_query;
use crate::AppError;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use toml::Value as TomlValue;
use vulcan_core::{load_vault_config, move_note, FolderNotesConfig, MoveSummary, VaultPaths};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderNoteConversionRequest {
    pub source: Option<FolderNotesConfig>,
    pub destination: FolderNotesConfig,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderNoteMovePlan {
    pub folder: String,
    pub source_path: String,
    pub destination_path: String,
    pub rewritten_files: Vec<vulcan_core::RewrittenFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderNoteConversionReport {
    pub dry_run: bool,
    pub source: FolderNotesConfig,
    pub destination: FolderNotesConfig,
    pub moves: Vec<FolderNoteMovePlan>,
    pub config_path: PathBuf,
    pub config_updated: bool,
    #[serde(skip_serializing)]
    pub changed_paths: Vec<String>,
}

pub fn convert_folder_notes(
    paths: &VaultPaths,
    request: &FolderNoteConversionRequest,
) -> Result<FolderNoteConversionReport, AppError> {
    let current = load_vault_config(paths).config.folder_notes;
    let source = request.source.clone().unwrap_or(current);
    source.validate().map_err(AppError::operation)?;
    request
        .destination
        .validate()
        .map_err(AppError::operation)?;

    let planned = plan_conversion_paths(paths, &source, &request.destination)?;

    let config_plan = plan_config_batch_report(
        paths,
        &[
            ConfigMutationOperation::Set {
                key: "folder_notes.placement".to_string(),
                value: TomlValue::String(
                    match request.destination.placement {
                        vulcan_core::FolderNotePlacement::Inside => "inside",
                        vulcan_core::FolderNotePlacement::Outside => "outside",
                    }
                    .to_string(),
                ),
            },
            ConfigMutationOperation::Set {
                key: "folder_notes.name".to_string(),
                value: TomlValue::String(request.destination.name.clone()),
            },
        ],
        ConfigTarget::Shared,
        request.dry_run,
    )?;

    // Validate every move before writing the first file. The applied pass is
    // still retryable: shared config changes only after all moves complete.
    let mut preflight = Vec::new();
    for (folder, (source_path, destination_path)) in &planned {
        let summary =
            move_note(paths, source_path, destination_path, true).map_err(AppError::operation)?;
        preflight.push((
            folder.clone(),
            source_path.clone(),
            destination_path.clone(),
            summary,
        ));
    }

    let mut moves = Vec::new();
    let mut changed_paths = BTreeSet::new();
    for (folder, source_path, destination_path, planned_summary) in preflight {
        let MoveSummary {
            rewritten_files, ..
        } = if request.dry_run {
            planned_summary
        } else {
            move_note(paths, &source_path, &destination_path, false).map_err(AppError::operation)?
        };
        changed_paths.insert(source_path.clone());
        changed_paths.insert(destination_path.clone());
        changed_paths.extend(rewritten_files.iter().map(|file| file.path.clone()));
        moves.push(FolderNoteMovePlan {
            folder,
            source_path,
            destination_path,
            rewritten_files,
        });
    }

    if config_plan.updated {
        changed_paths.insert(config_plan.config_path.to_string_lossy().into_owned());
    }
    if !request.dry_run {
        apply_config_batch_report(paths, config_plan.clone())?;
    }

    Ok(FolderNoteConversionReport {
        dry_run: request.dry_run,
        source,
        destination: request.destination.clone(),
        moves,
        config_path: config_plan.config_path,
        config_updated: config_plan.updated,
        changed_paths: changed_paths.into_iter().collect(),
    })
}

fn plan_conversion_paths(
    paths: &VaultPaths,
    source: &FolderNotesConfig,
    destination: &FolderNotesConfig,
) -> Result<BTreeMap<String, (String, String)>, AppError> {
    let query = execute_export_query(paths, None, None, None)?;
    let note_paths = query
        .notes
        .iter()
        .map(|note| note.document_path.clone())
        .collect::<BTreeSet<_>>();
    let mut planned = BTreeMap::<String, (String, String)>::new();
    let mut destinations = BTreeMap::<String, String>::new();

    for note_path in &note_paths {
        let Some(folder) = source.folder_for_note_path(note_path) else {
            continue;
        };
        if !paths.vault_root().join(&folder).is_dir() {
            continue;
        }
        let destination_path = destination.note_path_for_folder(&folder).ok_or_else(|| {
            AppError::operation(format!(
                "cannot map folder `{folder}` with the destination folder-note convention"
            ))
        })?;
        if destination_path == *note_path {
            continue;
        }
        let destination_case_key = destination_path.to_lowercase();
        if let Some(existing) = destinations.insert(destination_case_key, note_path.clone()) {
            return Err(AppError::operation(format!(
                "folder-note conversion collision: `{existing}` and `{note_path}` both map to `{destination_path}`"
            )));
        }
        planned.insert(folder, (note_path.clone(), destination_path));
    }

    let planned_sources = planned
        .values()
        .map(|(source_path, _)| source_path.as_str())
        .collect::<BTreeSet<_>>();
    for (source_path, destination_path) in planned.values() {
        if planned_sources.contains(destination_path.as_str()) {
            return Err(AppError::operation(format!(
                "folder-note conversion collision: destination `{destination_path}` is also a planned source"
            )));
        }
        reject_destination_collision(paths, source_path, destination_path)?;
    }

    Ok(planned)
}

fn reject_destination_collision(
    paths: &VaultPaths,
    source_path: &str,
    destination_path: &str,
) -> Result<(), AppError> {
    let destination = paths.vault_root().join(destination_path);
    if destination.exists() {
        return Err(AppError::operation(format!(
            "folder-note conversion would overwrite existing `{destination_path}`"
        )));
    }

    let parent = destination.parent().unwrap_or_else(|| paths.vault_root());
    let Some(destination_name) = destination.file_name() else {
        return Err(AppError::operation(format!(
            "invalid folder-note destination `{destination_path}`"
        )));
    };
    if !parent.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(parent).map_err(AppError::operation)? {
        let entry = entry.map_err(AppError::operation)?;
        if entry.file_name().eq_ignore_ascii_case(destination_name) {
            let relative = entry
                .path()
                .strip_prefix(paths.vault_root())
                .unwrap_or(entry.path().as_path())
                .to_string_lossy()
                .replace('\\', "/");
            if relative != source_path {
                return Err(AppError::operation(format!(
                    "folder-note conversion has a case-insensitive conflict: `{destination_path}` conflicts with `{relative}`"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::{scan_vault, FolderNotePlacement, ScanMode};

    fn convention(placement: FolderNotePlacement, name: &str) -> FolderNotesConfig {
        FolderNotesConfig {
            placement,
            name: name.to_string(),
        }
    }

    fn initialized_vault() -> (tempfile::TempDir, VaultPaths) {
        let temp = tempdir().expect("temp dir");
        fs::create_dir_all(temp.path().join(".vulcan")).expect("vulcan dir");
        let paths = VaultPaths::new(temp.path());
        (temp, paths)
    }

    #[test]
    fn converts_nested_inside_folder_names_to_index_notes_and_updates_config() {
        let (temp, paths) = initialized_vault();
        fs::create_dir_all(temp.path().join("Projects/Deep")).expect("nested folders");
        fs::write(
            temp.path().join("Projects/Projects.md"),
            "# Projects\n\n[Deep](Deep/Deep.md)\n",
        )
        .expect("parent note");
        fs::write(temp.path().join("Projects/Deep/Deep.md"), "# Deep\n").expect("nested note");
        scan_vault(&paths, ScanMode::Full).expect("scan");

        let report = convert_folder_notes(
            &paths,
            &FolderNoteConversionRequest {
                source: None,
                destination: convention(FolderNotePlacement::Inside, "index"),
                dry_run: false,
            },
        )
        .expect("conversion");

        assert_eq!(report.moves.len(), 2);
        assert!(temp.path().join("Projects/index.md").is_file());
        assert!(temp.path().join("Projects/Deep/index.md").is_file());
        assert!(!temp.path().join("Projects/Projects.md").exists());
        assert!(fs::read_to_string(temp.path().join(".vulcan/config.toml"))
            .expect("config")
            .contains("name = \"index\""));
        assert_eq!(
            load_vault_config(&paths).config.folder_notes,
            convention(FolderNotePlacement::Inside, "index")
        );
    }

    #[test]
    fn dry_run_is_deterministic_and_does_not_mutate_files_or_config() {
        let (temp, paths) = initialized_vault();
        fs::create_dir_all(temp.path().join("Projects")).expect("folder");
        fs::write(temp.path().join("Projects/Projects.md"), "# Projects\n").expect("note");
        scan_vault(&paths, ScanMode::Full).expect("scan");
        let request = FolderNoteConversionRequest {
            source: None,
            destination: convention(FolderNotePlacement::Outside, "{{folder_name}}"),
            dry_run: true,
        };

        let first = convert_folder_notes(&paths, &request).expect("first plan");
        let second = convert_folder_notes(&paths, &request).expect("second plan");

        assert_eq!(first, second);
        assert!(temp.path().join("Projects/Projects.md").is_file());
        assert!(!temp.path().join("Projects.md").exists());
        assert!(!temp.path().join(".vulcan/config.toml").exists());
    }

    #[test]
    fn rejects_existing_and_case_insensitive_destinations() {
        let (temp, paths) = initialized_vault();
        fs::create_dir_all(temp.path().join("Projects")).expect("folder");
        fs::write(temp.path().join("Projects/Projects.md"), "# Projects\n").expect("source");
        fs::write(temp.path().join("Projects/INDEX.md"), "# Existing\n").expect("conflict");
        scan_vault(&paths, ScanMode::Full).expect("scan");

        let error = convert_folder_notes(
            &paths,
            &FolderNoteConversionRequest {
                source: None,
                destination: convention(FolderNotePlacement::Inside, "index"),
                dry_run: true,
            },
        )
        .expect_err("case conflict must fail");

        assert!(error.to_string().contains("case-insensitive conflict"));
        assert!(temp.path().join("Projects/Projects.md").is_file());
    }

    #[test]
    fn supports_explicit_readme_source_and_outside_destination() {
        let (temp, paths) = initialized_vault();
        fs::create_dir_all(temp.path().join("Guides")).expect("folder");
        fs::write(temp.path().join("Guides/README.md"), "# Guides\n").expect("source");
        scan_vault(&paths, ScanMode::Full).expect("scan");

        let report = convert_folder_notes(
            &paths,
            &FolderNoteConversionRequest {
                source: Some(convention(FolderNotePlacement::Inside, "README")),
                destination: convention(FolderNotePlacement::Outside, "{{folder_name}}"),
                dry_run: false,
            },
        )
        .expect("conversion");

        assert_eq!(report.moves[0].destination_path, "Guides.md");
        assert!(temp.path().join("Guides.md").is_file());
    }
}
