//! Markdown Wiki Package import and export workflows.

use crate::textbundle::validate_new_destination;
use crate::AppError;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;
use vulcan_core::paths::secure_create_file;
use vulcan_core::textbundle::TextBundleRepresentation;
use vulcan_core::wiki_package::{
    inspect_wiki_package, logical_identity, WikiPackageManifest, WikiPackageMember,
    WikiPackageMemberRole, WikiPackageProducer, WIKI_PACKAGE_FORMAT, WIKI_PACKAGE_VERSION,
};
use vulcan_core::{ScanMode, VaultPaths};
use zip::write::FileOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiPackageExportRequest {
    pub output: PathBuf,
    pub title: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WikiPackageExportReport {
    pub dry_run: bool,
    pub output_path: String,
    pub representation: TextBundleRepresentation,
    pub identity: String,
    pub notes: usize,
    pub assets: usize,
    pub excluded_roots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiPackageImportRequest {
    pub package: PathBuf,
    pub destination: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WikiPackageImportReport {
    pub dry_run: bool,
    pub package_identity: String,
    pub destination_root: String,
    pub notes: usize,
    pub assets: usize,
    pub members: Vec<String>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct ExportMember {
    manifest: WikiPackageMember,
    source: PathBuf,
}

pub fn export_wiki_package(
    paths: &VaultPaths,
    request: &WikiPackageExportRequest,
) -> Result<WikiPackageExportReport, AppError> {
    let representation = output_representation(&request.output)?;
    if request.output.exists() {
        return Err(AppError::operation(format!(
            "wiki package output already exists: {}",
            request.output.display()
        )));
    }
    let members = collect_vault_members(paths)?;
    let manifest = WikiPackageManifest {
        format: WIKI_PACKAGE_FORMAT.to_string(),
        version: WIKI_PACKAGE_VERSION,
        title: request.title.clone(),
        producer: WikiPackageProducer {
            name: "vulcan".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            extensions: serde_json::Map::new(),
        },
        lineage: Vec::new(),
        members: members
            .iter()
            .map(|member| member.manifest.clone())
            .collect(),
        extensions: serde_json::Map::new(),
    };
    let identity = logical_identity(&manifest.members);
    if !request.dry_run {
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(AppError::operation)?;
        match representation {
            TextBundleRepresentation::Directory => {
                write_directory(&request.output, &manifest_bytes, &members)?;
            }
            TextBundleRepresentation::Zip => {
                write_zip(&request.output, &manifest_bytes, &members)?;
            }
        }
    }
    Ok(WikiPackageExportReport {
        dry_run: request.dry_run,
        output_path: request.output.display().to_string(),
        representation,
        identity,
        notes: members
            .iter()
            .filter(|member| member.manifest.role == WikiPackageMemberRole::Note)
            .count(),
        assets: members
            .iter()
            .filter(|member| member.manifest.role == WikiPackageMemberRole::Asset)
            .count(),
        excluded_roots: vec![
            ".git".to_string(),
            ".obsidian".to_string(),
            ".stfolder".to_string(),
            ".trash".to_string(),
            ".vulcan".to_string(),
        ],
    })
}

pub fn import_wiki_package(
    paths: &VaultPaths,
    request: &WikiPackageImportRequest,
) -> Result<WikiPackageImportReport, AppError> {
    let _lock = vulcan_core::write_lock::acquire_write_lock(paths).map_err(AppError::operation)?;
    let destination = validate_new_destination(paths, &request.destination, "wiki package")?;
    let package = inspect_wiki_package(&request.package).map_err(AppError::operation)?;
    if !package.valid {
        return Err(AppError::operation(format!(
            "wiki package validation failed: {}",
            package
                .diagnostics
                .iter()
                .take(8)
                .map(|item| format!("{}: {}", item.code, item.message))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    let manifest = package.manifest.as_ref().expect("validated manifest");
    let members = manifest
        .members
        .iter()
        .map(|member| {
            format!(
                "{destination}/{}",
                member
                    .path
                    .strip_prefix("content/")
                    .expect("validated content path")
            )
        })
        .collect::<Vec<_>>();
    if !request.dry_run {
        let apply = (|| -> Result<(), Box<dyn std::error::Error>> {
            for (member, target) in manifest.members.iter().zip(&members) {
                let mut output = secure_create_file(paths.vault_root(), Path::new(target))?;
                package.copy_member_to(&member.path, &mut output)?;
                output.sync_all()?;
            }
            Ok(())
        })();
        if let Err(error) = apply {
            let _ = fs::remove_dir_all(paths.vault_root().join(&destination));
            return Err(AppError::operation(format!(
                "failed to import wiki package; removed partial destination: {error}"
            )));
        }
        if let Err(error) = vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental) {
            let _ = fs::remove_dir_all(paths.vault_root().join(&destination));
            let _ = vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental);
            return Err(AppError::operation(format!(
                "failed to refresh cache; removed imported wiki package: {error}"
            )));
        }
    }
    Ok(WikiPackageImportReport {
        dry_run: request.dry_run,
        package_identity: package.identity,
        destination_root: destination,
        notes: manifest
            .members
            .iter()
            .filter(|member| member.role == WikiPackageMemberRole::Note)
            .count(),
        assets: manifest
            .members
            .iter()
            .filter(|member| member.role == WikiPackageMemberRole::Asset)
            .count(),
        changed_paths: members.clone(),
        members,
    })
}

fn collect_vault_members(paths: &VaultPaths) -> Result<Vec<ExportMember>, AppError> {
    let excluded = [".git", ".obsidian", ".stfolder", ".trash", ".vulcan"];
    let mut pending = vec![paths.vault_root().to_path_buf()];
    let mut members = Vec::new();
    let mut folded_paths = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(AppError::operation)? {
            let entry = entry.map_err(AppError::operation)?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| matches!(name, ".DS_Store" | "Thumbs.db"))
            {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(paths.vault_root())
                .map_err(AppError::operation)?
                .to_path_buf();
            if relative
                .components()
                .next()
                .and_then(|part| part.as_os_str().to_str())
                .is_some_and(|name| excluded.contains(&name))
            {
                continue;
            }
            let file_type = entry.file_type().map_err(AppError::operation)?;
            if file_type.is_symlink() {
                return Err(AppError::operation(format!(
                    "wiki export does not follow symbolic links: {}",
                    relative.display()
                )));
            }
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                return Err(AppError::operation(format!(
                    "wiki export requires regular files: {}",
                    relative.display()
                )));
            }
            let relative = relative
                .to_str()
                .ok_or_else(|| AppError::operation("wiki member path is not UTF-8"))?
                .replace('\\', "/");
            if relative.nfc().collect::<String>() != relative {
                return Err(AppError::operation(format!(
                    "wiki export member path is not NFC-normalized: {relative}"
                )));
            }
            if !folded_paths.insert(relative.to_lowercase()) {
                return Err(AppError::operation(format!(
                    "wiki export has a case-fold path collision: {relative}"
                )));
            }
            let source = entry.path();
            let size = entry.metadata().map_err(AppError::operation)?.len();
            let digest = hash_file(&source)?;
            let role = if Path::new(&relative)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("md"))
            {
                WikiPackageMemberRole::Note
            } else {
                WikiPackageMemberRole::Asset
            };
            members.push(ExportMember {
                manifest: WikiPackageMember {
                    path: format!("content/{relative}"),
                    role,
                    media_type: (role == WikiPackageMemberRole::Note)
                        .then(|| "text/markdown".to_string()),
                    size,
                    digest,
                    document_id: None,
                    extensions: serde_json::Map::new(),
                },
                source,
            });
        }
    }
    members.sort_by(|left, right| left.manifest.path.cmp(&right.manifest.path));
    Ok(members)
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

fn output_representation(path: &Path) -> Result<TextBundleRepresentation, AppError> {
    match path.extension().and_then(|value| value.to_str()) {
        Some(value) if value.eq_ignore_ascii_case("wikibundle") => {
            Ok(TextBundleRepresentation::Directory)
        }
        Some(value) if value.eq_ignore_ascii_case("wikipack") => Ok(TextBundleRepresentation::Zip),
        _ => Err(AppError::operation(
            "wiki package output must end in .wikibundle or .wikipack",
        )),
    }
}

fn write_directory(
    output: &Path,
    manifest: &[u8],
    members: &[ExportMember],
) -> Result<(), AppError> {
    fs::create_dir(output).map_err(AppError::operation)?;
    let result = (|| -> Result<(), std::io::Error> {
        fs::write(output.join("wiki.json"), manifest)?;
        for member in members {
            let target = output.join(&member.manifest.path);
            fs::create_dir_all(target.parent().expect("parent"))?;
            fs::copy(&member.source, target)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(output);
        return Err(AppError::operation(error));
    }
    Ok(())
}

fn write_zip(output: &Path, manifest: &[u8], members: &[ExportMember]) -> Result<(), AppError> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut zip = zip::ZipWriter::new(File::create(output)?);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("wiki.json", options)?;
        zip.write_all(manifest)?;
        for member in members {
            zip.start_file(&member.manifest.path, options)?;
            std::io::copy(&mut File::open(&member.source)?, &mut zip)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, scan_vault};

    #[test]
    fn export_and_import_preserve_wiki_bytes_and_exclude_internal_state() {
        let temp = tempdir().expect("temp");
        let vault = temp.path().join("vault");
        fs::create_dir_all(vault.join("assets")).expect("dirs");
        let paths = VaultPaths::new(&vault);
        initialize_vulcan_dir(&paths).expect("init");
        fs::write(vault.join("Home.md"), "# Home\n\n![](assets/a.bin)\n").expect("note");
        fs::write(vault.join("assets/a.bin"), b"asset").expect("asset");
        scan_vault(&paths, ScanMode::Full).expect("scan");
        let output = temp.path().join("wiki.wikipack");
        let report = export_wiki_package(
            &paths,
            &WikiPackageExportRequest {
                output: output.clone(),
                title: Some("Test".to_string()),
                dry_run: false,
            },
        )
        .expect("export");
        assert_eq!((report.notes, report.assets), (1, 1));
        let inspected = inspect_wiki_package(&output).expect("inspect");
        assert!(inspected.valid, "{:?}", inspected.diagnostics);
        let directory = temp.path().join("wiki.wikibundle");
        export_wiki_package(
            &paths,
            &WikiPackageExportRequest {
                output: directory.clone(),
                title: Some("Test".to_string()),
                dry_run: false,
            },
        )
        .expect("directory export");
        let directory_inspection = inspect_wiki_package(&directory).expect("directory inspect");
        assert!(directory_inspection.valid);
        assert_eq!(directory_inspection.identity, inspected.identity);
        assert!(!inspected
            .manifest
            .as_ref()
            .expect("manifest")
            .members
            .iter()
            .any(|member| member.path.contains(".vulcan")));
        let preview = import_wiki_package(
            &paths,
            &WikiPackageImportRequest {
                package: output.clone(),
                destination: "Imported".to_string(),
                dry_run: true,
            },
        )
        .expect("preview");
        assert_eq!(preview.members.len(), 2);
        assert!(!vault.join("Imported").exists());
        import_wiki_package(
            &paths,
            &WikiPackageImportRequest {
                package: output,
                destination: "Imported".to_string(),
                dry_run: false,
            },
        )
        .expect("import");
        assert_eq!(
            fs::read(vault.join("Home.md")).expect("source"),
            fs::read(vault.join("Imported/Home.md")).expect("imported")
        );
        assert_eq!(
            fs::read(vault.join("assets/a.bin")).expect("source asset"),
            fs::read(vault.join("Imported/assets/a.bin")).expect("imported asset")
        );

        fs::write(paths.cache_db(), b"not a sqlite database").expect("corrupt cache");
        let failed = import_wiki_package(
            &paths,
            &WikiPackageImportRequest {
                package: directory,
                destination: "Rollback".to_string(),
                dry_run: false,
            },
        );
        assert!(failed.is_err());
        assert!(!vault.join("Rollback").exists());
    }
}
