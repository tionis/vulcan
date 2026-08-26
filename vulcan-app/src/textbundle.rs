//! TextBundle/TextPack import and export workflows.

use crate::AppError;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use vulcan_core::config::load_vault_config;
use vulcan_core::paths::{
    normalize_relative_input_path, secure_create, secure_create_file, RelativePathOptions,
};
use vulcan_core::textbundle::{inspect_text_bundle, TextBundleInfo, TextBundleRepresentation};
use vulcan_core::{parse_document, LinkKind, OriginContext, ScanMode, VaultPaths};
use zip::write::FileOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBundleImportRequest {
    pub package: PathBuf,
    pub destination: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextBundleImportReport {
    pub dry_run: bool,
    pub package_identity: String,
    pub destination_root: String,
    pub note_path: String,
    pub assets: Vec<TextBundleImportAsset>,
    pub info: Option<TextBundleInfo>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextBundleImportAsset {
    pub package_path: String,
    pub vault_path: String,
    pub size: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBundleExportRequest {
    pub note: String,
    pub output: PathBuf,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextBundleExportReport {
    pub dry_run: bool,
    pub note_path: String,
    pub output_path: String,
    pub representation: TextBundleRepresentation,
    pub assets: Vec<TextBundleExportAsset>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextBundleExportAsset {
    pub vault_path: String,
    pub package_path: String,
    pub size: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedAsset {
    report: TextBundleExportAsset,
    source: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextEdit {
    start: usize,
    end: usize,
    replacement: String,
}

pub fn import_text_bundle(
    paths: &VaultPaths,
    request: &TextBundleImportRequest,
) -> Result<TextBundleImportReport, AppError> {
    let _lock = vulcan_core::write_lock::acquire_write_lock(paths).map_err(AppError::operation)?;
    let destination = validate_new_destination(paths, &request.destination, "TextBundle")?;
    let package = inspect_text_bundle(&request.package).map_err(AppError::operation)?;
    if !package.valid {
        let errors = package
            .diagnostics
            .iter()
            .take(8)
            .map(|item| format!("{}: {}", item.code, item.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::operation(format!(
            "TextBundle validation failed: {errors}"
        )));
    }
    let note_path = format!("{destination}/text.md");
    let assets = package
        .assets
        .iter()
        .map(|asset| TextBundleImportAsset {
            package_path: asset.path.clone(),
            vault_path: format!("{destination}/{}", asset.path),
            size: asset.size,
            digest: asset.digest.clone(),
        })
        .collect::<Vec<_>>();
    let mut changed_paths = vec![note_path.clone()];
    changed_paths.extend(assets.iter().map(|asset| asset.vault_path.clone()));

    if !request.dry_run {
        let apply = (|| -> Result<(), Box<dyn std::error::Error>> {
            secure_create(
                paths.vault_root(),
                Path::new(&note_path),
                package
                    .text
                    .as_deref()
                    .ok_or("validated package has no text")?,
            )?;
            for asset in &assets {
                let mut output =
                    secure_create_file(paths.vault_root(), Path::new(&asset.vault_path))?;
                package.copy_member_to(&asset.package_path, &mut output)?;
                output.sync_all()?;
            }
            Ok(())
        })();
        if let Err(error) = apply {
            let _ = fs::remove_dir_all(paths.vault_root().join(&destination));
            return Err(AppError::operation(format!(
                "failed to import TextBundle; removed partial destination: {error}"
            )));
        }
        if let Err(error) = vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental) {
            let _ = fs::remove_dir_all(paths.vault_root().join(&destination));
            let _ = vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental);
            return Err(AppError::operation(format!(
                "failed to refresh cache; removed imported TextBundle: {error}"
            )));
        }
    }

    Ok(TextBundleImportReport {
        dry_run: request.dry_run,
        package_identity: package.identity,
        destination_root: destination,
        note_path,
        assets,
        info: package.info,
        changed_paths,
    })
}

pub fn export_text_bundle(
    paths: &VaultPaths,
    request: &TextBundleExportRequest,
) -> Result<TextBundleExportReport, AppError> {
    let note_path = normalize_relative_input_path(
        &request.note,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(AppError::operation)?;
    let source = paths.vault_root().join(&note_path);
    let metadata = fs::symlink_metadata(&source).map_err(AppError::operation)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::operation(
            "TextBundle source must be a regular Markdown file",
        ));
    }
    let content = fs::read_to_string(&source).map_err(AppError::operation)?;
    let config = load_vault_config(paths).config;
    let (text, planned_assets) = plan_export_assets(paths, &note_path, &content, &config)?;
    let representation = output_representation(&request.output)?;
    if request.output.exists() {
        return Err(AppError::operation(format!(
            "TextBundle output already exists: {}",
            request.output.display()
        )));
    }
    let info = TextBundleInfo {
        version: 2,
        text_type: Some("net.daringfireball.markdown".to_string()),
        transient: false,
        creator_url: None,
        creator_identifier: Some("dev.tionis.vulcan".to_string()),
        source_url: None,
        extensions: serde_json::Map::new(),
    };
    let info_bytes = serde_json::to_vec_pretty(&info).map_err(AppError::operation)?;
    if !request.dry_run {
        match representation {
            TextBundleRepresentation::Directory => {
                write_textbundle_directory(&request.output, &info_bytes, &text, &planned_assets)?;
            }
            TextBundleRepresentation::Zip => {
                write_textpack_zip(&request.output, &info_bytes, &text, &planned_assets)?;
            }
        }
    }
    let assets = planned_assets
        .into_iter()
        .map(|asset| asset.report)
        .collect::<Vec<_>>();
    Ok(TextBundleExportReport {
        dry_run: request.dry_run,
        note_path,
        output_path: request.output.display().to_string(),
        representation,
        assets,
        changed_paths: if request.dry_run {
            Vec::new()
        } else {
            vec![request.output.display().to_string()]
        },
    })
}

fn plan_export_assets(
    paths: &VaultPaths,
    note_path: &str,
    content: &str,
    config: &vulcan_core::VaultConfig,
) -> Result<(String, Vec<PlannedAsset>), AppError> {
    let parsed = parse_document(content, config);
    let note_parent = Path::new(note_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut by_source = BTreeMap::<String, PlannedAsset>::new();
    let mut edits = Vec::new();
    for link in parsed.links {
        if link.origin_context != OriginContext::Body || link.link_kind == LinkKind::External {
            continue;
        }
        let Some(candidate) = link.target_path_candidate.as_deref() else {
            continue;
        };
        if candidate.is_empty()
            || Path::new(candidate)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let joined = note_parent.join(candidate);
        let Some(joined) = joined.to_str() else {
            continue;
        };
        let Ok(vault_path) = normalize_relative_input_path(
            joined,
            RelativePathOptions {
                expected_extension: None,
                append_extension_if_missing: false,
            },
        ) else {
            continue;
        };
        let source = paths.vault_root().join(&vault_path);
        let Ok(metadata) = fs::symlink_metadata(&source) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let package_path = format!("assets/{vault_path}");
        let digest = hash_file(&source)?;
        by_source
            .entry(vault_path.clone())
            .or_insert_with(|| PlannedAsset {
                report: TextBundleExportAsset {
                    vault_path,
                    package_path: package_path.clone(),
                    size: metadata.len(),
                    digest,
                },
                source,
            });
        if let Some(relative) = link.raw_text.find(candidate) {
            let mut replacement = link.raw_text.clone();
            replacement.replace_range(relative..relative + candidate.len(), &package_path);
            edits.push(TextEdit {
                start: link.byte_offset,
                end: link.byte_offset + link.raw_text.len(),
                replacement,
            });
        }
    }
    edits.sort_by_key(|edit| edit.start);
    let mut text = content.to_string();
    for edit in edits.into_iter().rev() {
        text.replace_range(edit.start..edit.end, &edit.replacement);
    }
    Ok((text, by_source.into_values().collect()))
}

fn output_representation(path: &Path) -> Result<TextBundleRepresentation, AppError> {
    match path.extension().and_then(|value| value.to_str()) {
        Some(value) if value.eq_ignore_ascii_case("textbundle") => {
            Ok(TextBundleRepresentation::Directory)
        }
        Some(value) if value.eq_ignore_ascii_case("textpack") => Ok(TextBundleRepresentation::Zip),
        _ => Err(AppError::operation(
            "TextBundle output must end in .textbundle or .textpack",
        )),
    }
}

fn hash_file(path: &Path) -> Result<String, AppError> {
    let mut file = File::open(path).map_err(AppError::operation)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(AppError::operation)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("blake3:{}", hasher.finalize()))
}

fn write_textbundle_directory(
    output: &Path,
    info: &[u8],
    text: &str,
    assets: &[PlannedAsset],
) -> Result<(), AppError> {
    fs::create_dir(output).map_err(AppError::operation)?;
    let result = (|| -> Result<(), std::io::Error> {
        fs::write(output.join("info.json"), info)?;
        fs::write(output.join("text.md"), text)?;
        for asset in assets {
            let destination = output.join(&asset.report.package_path);
            fs::create_dir_all(destination.parent().expect("asset parent"))?;
            fs::copy(&asset.source, destination)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(output);
        return Err(AppError::operation(error));
    }
    Ok(())
}

fn write_textpack_zip(
    output: &Path,
    info: &[u8],
    text: &str,
    assets: &[PlannedAsset],
) -> Result<(), AppError> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let file = File::create(output)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (path, bytes) in [("info.json", info), ("text.md", text.as_bytes())] {
            zip.start_file(path, options)?;
            zip.write_all(bytes)?;
        }
        for asset in assets {
            zip.start_file(&asset.report.package_path, options)?;
            let mut file = File::open(&asset.source)?;
            std::io::copy(&mut file, &mut zip)?;
        }
        zip.finish()?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(output);
        return Err(AppError::operation(error));
    }
    Ok(())
}

pub(crate) fn validate_new_destination(
    paths: &VaultPaths,
    destination: &str,
    kind: &str,
) -> Result<String, AppError> {
    let normalized = normalize_relative_input_path(
        destination,
        RelativePathOptions {
            expected_extension: None,
            append_extension_if_missing: false,
        },
    )
    .map_err(AppError::operation)?;
    if normalized != destination || normalized == ".vulcan" || normalized.starts_with(".vulcan/") {
        return Err(AppError::operation(format!(
            "{kind} destination must be a normalized non-internal vault-relative folder"
        )));
    }
    let components = Path::new(&normalized).components().collect::<Vec<_>>();
    let mut current = paths.vault_root().to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let name = component.as_os_str().to_string_lossy();
        let mut matched = None;
        if current.exists() {
            for entry in fs::read_dir(&current).map_err(AppError::operation)? {
                let entry = entry.map_err(AppError::operation)?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&name)
                {
                    matched = Some(entry);
                    break;
                }
            }
        }
        if let Some(entry) = matched {
            if index + 1 == components.len() {
                return Err(AppError::operation(format!(
                    "{kind} destination collides with existing vault path: {}",
                    entry.path().display()
                )));
            }
            if entry.file_name().to_string_lossy() != name
                || entry.file_type().map_err(AppError::operation)?.is_symlink()
                || !entry.file_type().map_err(AppError::operation)?.is_dir()
            {
                return Err(AppError::operation(format!(
                    "{kind} destination ancestor is not an exact regular directory: {}",
                    entry.path().display()
                )));
            }
            current = entry.path();
        } else {
            current.push(name.as_ref());
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, scan_vault};

    #[test]
    fn export_textpack_and_import_it_without_mutating_dry_run() {
        let temp = tempdir().expect("temp");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("init");
        fs::create_dir_all(temp.path().join("Notes/images")).expect("dirs");
        fs::write(
            temp.path().join("Notes/Page.md"),
            "# Page\n\n![](images/map.png)\n",
        )
        .expect("note");
        fs::write(temp.path().join("Notes/images/map.png"), b"synthetic").expect("asset");
        scan_vault(&paths, ScanMode::Full).expect("scan");
        let package = temp.path().join("out/page.textpack");
        let exported = export_text_bundle(
            &paths,
            &TextBundleExportRequest {
                note: "Notes/Page.md".to_string(),
                output: package.clone(),
                dry_run: false,
            },
        )
        .expect("export");
        assert_eq!(exported.assets.len(), 1);
        let inspected = inspect_text_bundle(&package).expect("inspect");
        assert!(inspected.valid, "{:?}", inspected.diagnostics);
        assert!(inspected
            .text
            .expect("text")
            .contains("assets/Notes/images/map.png"));

        let preview = import_text_bundle(
            &paths,
            &TextBundleImportRequest {
                package: package.clone(),
                destination: "Imported/Page".to_string(),
                dry_run: true,
            },
        )
        .expect("preview");
        assert!(!temp.path().join("Imported").exists());
        assert_eq!(preview.assets.len(), 1);
        let imported = import_text_bundle(
            &paths,
            &TextBundleImportRequest {
                package,
                destination: "Imported/Page".to_string(),
                dry_run: false,
            },
        )
        .expect("import");
        assert!(temp.path().join(&imported.note_path).exists());
        assert!(temp.path().join(&imported.assets[0].vault_path).exists());
    }

    #[test]
    fn import_rejects_existing_destination() {
        let temp = tempdir().expect("temp");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("init");
        fs::create_dir(temp.path().join("Existing")).expect("existing");
        let result = import_text_bundle(
            &paths,
            &TextBundleImportRequest {
                package: temp.path().join("missing.textpack"),
                destination: "Existing".to_string(),
                dry_run: true,
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn import_rejects_casefold_destination_collision_before_package_inspection() {
        let temp = tempdir().expect("temp");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("init");
        fs::create_dir(temp.path().join("Existing")).expect("existing");
        let result = import_text_bundle(
            &paths,
            &TextBundleImportRequest {
                package: temp.path().join("missing.textpack"),
                destination: "existing".to_string(),
                dry_run: true,
            },
        );
        assert!(result
            .expect_err("casefold collision")
            .to_string()
            .contains("collides"));
    }
}
