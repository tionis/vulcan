//! Markdown Wiki Package import and export workflows.

use crate::templates::{
    parse_frontmatter_document, render_note_from_parts, YamlMapping, YamlValue,
};
use crate::textbundle::validate_new_destination;
use crate::AppError;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;
use vulcan_core::exchange::{
    blake3_digest, canonical_json, container_identity, ExchangeActivity, ExchangeProducer,
    ExchangeProvenance, ExchangeSelector, ExchangeTool,
};
use vulcan_core::paths::{secure_create, secure_create_file};
use vulcan_core::textbundle::TextBundleRepresentation;
use vulcan_core::wiki_package::{
    inspect_wiki_package, is_markdown_path, WikiPackage, WikiPackageManifestV2,
    WikiPackageMemberRole, WikiPackageMemberV2, WikiPackageSummary, WikiSourceMapping,
    WIKI_MANIFEST_PATH, WIKI_PACKAGE_FORMAT, WIKI_PACKAGE_VERSION, WIKI_PROVENANCE_PATH,
};
use vulcan_core::{ScanMode, VaultPaths};
use zip::write::FileOptions;

const EXPORT_ACTIVITY: &str = "export";

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
    pub format_version: u32,
    pub identity: String,
    pub notes: usize,
    pub assets: usize,
    pub excluded_roots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiPackageImportRequest {
    pub package: PathBuf,
    pub destination: String,
    pub source_locators: WikiSourceLocators,
    pub dry_run: bool,
}

/// How much of the package source map import copies into `vulcan.source`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WikiSourceLocators {
    /// Package identity, member path, and mapping count. The spans stay in
    /// the package, which remains the evidence of record.
    #[default]
    Summary,
    /// Every mapped span with its locators, as MDAF import writes them.
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WikiPackageImportReport {
    pub dry_run: bool,
    pub package_identity: String,
    pub format_version: u32,
    pub source_locators: WikiSourceLocators,
    pub destination_root: String,
    pub notes: usize,
    pub assets: usize,
    pub members: Vec<String>,
    /// Imported notes that received `vulcan.source` locators from the
    /// package source map.
    pub annotated_notes: Vec<String>,
    pub summary: WikiPackageSummary,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct ExportMember {
    manifest: WikiPackageMemberV2,
    source: PathBuf,
}

/// Compact per-note provenance recorded on import by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NoteSourceSummary {
    artifact: String,
    member: String,
    mappings: usize,
}

/// Per-note provenance recorded on import. It has the same shape as MDAF
/// import's `vulcan.source`; spans address the package member's bytes.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct NoteSourceProvenance {
    artifact: String,
    member: String,
    spans: Vec<NoteSourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct NoteSourceSpan {
    start: usize,
    end: usize,
    locators: Vec<NoteSourceLocator>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct NoteSourceLocator {
    source_id: String,
    selectors: Vec<ExchangeSelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
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
    let (manifest_bytes, provenance_bytes) = export_control_members(&members, request)?;
    let manifest_digest = blake3_digest(&manifest_bytes);
    let provenance_digest = blake3_digest(&provenance_bytes);
    let identity = container_identity(
        members
            .iter()
            .map(|member| {
                (
                    member.manifest.path.as_str(),
                    member.manifest.size,
                    member.manifest.digest.as_str(),
                )
            })
            .chain([
                (
                    WIKI_MANIFEST_PATH,
                    manifest_bytes.len() as u64,
                    manifest_digest.as_str(),
                ),
                (
                    WIKI_PROVENANCE_PATH,
                    provenance_bytes.len() as u64,
                    provenance_digest.as_str(),
                ),
            ]),
    );
    if !request.dry_run {
        let control = [
            (WIKI_MANIFEST_PATH, manifest_bytes.as_slice()),
            (WIKI_PROVENANCE_PATH, provenance_bytes.as_slice()),
        ];
        match representation {
            TextBundleRepresentation::Directory => {
                write_directory(&request.output, &control, &members)?;
            }
            TextBundleRepresentation::Zip => {
                write_zip(&request.output, &control, &members)?;
            }
        }
    }
    Ok(WikiPackageExportReport {
        dry_run: request.dry_run,
        output_path: request.output.display().to_string(),
        representation,
        format_version: WIKI_PACKAGE_VERSION,
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

/// Build `wiki.json` and a single-activity `provenance.json` for an export.
fn export_control_members(
    members: &[ExportMember],
    request: &WikiPackageExportRequest,
) -> Result<(Vec<u8>, Vec<u8>), AppError> {
    let parameters = serde_json::Map::new();
    let mut outputs = members
        .iter()
        .map(|member| member.manifest.path.clone())
        .collect::<Vec<_>>();
    outputs.push(WIKI_PROVENANCE_PATH.to_string());
    let provenance = ExchangeProvenance {
        version: 1,
        activities: vec![ExchangeActivity {
            id: EXPORT_ACTIVITY.to_string(),
            kind: "vulcan.wiki-export".to_string(),
            started_at: None,
            ended_at: None,
            tools: vec![ExchangeTool {
                name: "vulcan".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                revision: None,
                package_url: None,
            }],
            models: Vec::new(),
            inputs: Vec::new(),
            outputs,
            depends_on: Vec::new(),
            parameters_digest: blake3_digest(&canonical_json(&serde_json::Value::Object(
                parameters.clone(),
            ))),
            parameters,
        }],
        redactions: Vec::new(),
    };
    let provenance_bytes = to_json_bytes(&provenance)?;
    let mut declared = members
        .iter()
        .map(|member| member.manifest.clone())
        .collect::<Vec<_>>();
    declared.push(WikiPackageMemberV2 {
        path: WIKI_PROVENANCE_PATH.to_string(),
        role: WikiPackageMemberRole::Provenance,
        media_type: "application/json".to_string(),
        size: provenance_bytes.len() as u64,
        digest: blake3_digest(&provenance_bytes),
        created_by: EXPORT_ACTIVITY.to_string(),
        document_id: None,
        schema: None,
        namespace: None,
    });
    declared.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let manifest = WikiPackageManifestV2 {
        format: WIKI_PACKAGE_FORMAT.to_string(),
        version: WIKI_PACKAGE_VERSION,
        title: request
            .title
            .clone()
            .filter(|title| !title.trim().is_empty()),
        root: None,
        producer: ExchangeProducer {
            name: "vulcan".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            revision: None,
        },
        members: declared,
        sources: Vec::new(),
        derived_from: Vec::new(),
    };
    Ok((to_json_bytes(&manifest)?, provenance_bytes))
}

fn to_json_bytes(value: &impl Serialize) -> Result<Vec<u8>, AppError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(AppError::operation)?;
    bytes.push(b'\n');
    Ok(bytes)
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
                .filter(|item| item.severity
                    == vulcan_core::exchange::ExchangeDiagnosticSeverity::Error)
                .take(8)
                .map(|item| format!("{}: {}", item.code, item.message))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    let content = package.content_members();
    let target = |path: &str| {
        format!(
            "{destination}/{}",
            path.strip_prefix("content/")
                .expect("validated content path")
        )
    };
    let members = content
        .iter()
        .map(|member| target(&member.path))
        .collect::<Vec<_>>();
    let annotations = source_annotations(&package, request.source_locators)?;
    if !request.dry_run {
        let apply = (|| -> Result<(), Box<dyn std::error::Error>> {
            for (member, target) in content.iter().zip(&members) {
                if let Some(annotated) = annotations.get(&member.path) {
                    secure_create(paths.vault_root(), Path::new(target), annotated)?;
                    continue;
                }
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
        package_identity: package.identity.clone(),
        format_version: package.version.unwrap_or(WIKI_PACKAGE_VERSION),
        source_locators: request.source_locators,
        destination_root: destination.clone(),
        notes: package.summary.notes,
        assets: package.summary.assets,
        annotated_notes: annotations.keys().map(|path| target(path)).collect(),
        summary: package.summary,
        changed_paths: members.clone(),
        members,
    })
}

/// Render annotated note text for every note with source-map mappings.
/// Conflicts with existing `vulcan.source` frontmatter fail before writing.
fn source_annotations(
    package: &WikiPackage,
    detail: WikiSourceLocators,
) -> Result<BTreeMap<String, String>, AppError> {
    let Some(source_map) = package.source_map.as_ref() else {
        return Ok(BTreeMap::new());
    };
    let mut by_note: BTreeMap<&str, Vec<&WikiSourceMapping>> = BTreeMap::new();
    for mapping in &source_map.mappings {
        by_note
            .entry(mapping.note.as_str())
            .or_default()
            .push(mapping);
    }
    let mut annotations = BTreeMap::new();
    for (note, mappings) in by_note {
        let mut bytes = Vec::new();
        package
            .copy_member_to(note, &mut bytes)
            .map_err(AppError::operation)?;
        let text = String::from_utf8(bytes).map_err(AppError::operation)?;
        let provenance = match detail {
            WikiSourceLocators::Summary => serde_yaml::to_value(NoteSourceSummary {
                artifact: package.identity.clone(),
                member: note.to_string(),
                mappings: mappings.len(),
            }),
            WikiSourceLocators::Full => serde_yaml::to_value(NoteSourceProvenance {
                artifact: package.identity.clone(),
                member: note.to_string(),
                spans: full_spans(&mappings),
            }),
        }
        .map_err(AppError::operation)?;
        annotations.insert(
            note.to_string(),
            add_source_frontmatter(&text, note, provenance)?,
        );
    }
    Ok(annotations)
}

/// Group consecutive mappings of the same span, in source-map order.
fn full_spans(mappings: &[&WikiSourceMapping]) -> Vec<NoteSourceSpan> {
    let mut spans: Vec<NoteSourceSpan> = Vec::new();
    for mapping in mappings {
        let locator = NoteSourceLocator {
            source_id: mapping.source.source_id.clone(),
            selectors: mapping.source.selectors.clone(),
            confidence: mapping.confidence,
            method: mapping.method.clone(),
        };
        match spans.last_mut() {
            Some(span)
                if span.start == mapping.document.start && span.end == mapping.document.end =>
            {
                span.locators.push(locator);
            }
            _ => spans.push(NoteSourceSpan {
                start: mapping.document.start,
                end: mapping.document.end,
                locators: vec![locator],
            }),
        }
    }
    spans
}

fn add_source_frontmatter(
    content: &str,
    note: &str,
    provenance: YamlValue,
) -> Result<String, AppError> {
    let (frontmatter, body) =
        parse_frontmatter_document(content, false).map_err(AppError::operation)?;
    let mut frontmatter = frontmatter.unwrap_or_default();
    let vulcan = frontmatter
        .entry(YamlValue::String("vulcan".to_string()))
        .or_insert_with(|| YamlValue::Mapping(YamlMapping::new()));
    let vulcan = vulcan.as_mapping_mut().ok_or_else(|| {
        AppError::operation(format!(
            "{note}: existing `vulcan` frontmatter must be a mapping for wiki import"
        ))
    })?;
    let source_key = YamlValue::String("source".to_string());
    if vulcan.contains_key(&source_key) {
        return Err(AppError::operation(format!(
            "{note}: existing `vulcan.source` frontmatter would be overwritten by wiki import"
        )));
    }
    vulcan.insert(source_key, provenance);
    render_note_from_parts(Some(&frontmatter), &body).map_err(AppError::operation)
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
            let role = if is_markdown_path(&relative) {
                WikiPackageMemberRole::Note
            } else {
                WikiPackageMemberRole::Asset
            };
            members.push(ExportMember {
                manifest: WikiPackageMemberV2 {
                    path: format!("content/{relative}"),
                    role,
                    media_type: if role == WikiPackageMemberRole::Note {
                        "text/markdown".to_string()
                    } else {
                        "application/octet-stream".to_string()
                    },
                    size,
                    digest,
                    created_by: EXPORT_ACTIVITY.to_string(),
                    document_id: None,
                    schema: None,
                    namespace: None,
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
    control: &[(&str, &[u8])],
    members: &[ExportMember],
) -> Result<(), AppError> {
    fs::create_dir(output).map_err(AppError::operation)?;
    let result = (|| -> Result<(), std::io::Error> {
        for (path, bytes) in control {
            fs::write(output.join(path), bytes)?;
        }
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

fn write_zip(
    output: &Path,
    control: &[(&str, &[u8])],
    members: &[ExportMember],
) -> Result<(), AppError> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut zip = zip::ZipWriter::new(File::create(output)?);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (path, bytes) in control {
            zip.start_file(*path, options)?;
            zip.write_all(bytes)?;
        }
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
        assert_eq!(inspected.version, Some(WIKI_PACKAGE_VERSION));
        assert_eq!(report.identity, inspected.identity);
        assert_eq!(inspected.summary.provenance_activities, 1);
        assert!(!inspected
            .content_members()
            .iter()
            .any(|member| member.path.contains(".vulcan")));
        let preview = import_wiki_package(
            &paths,
            &WikiPackageImportRequest {
                package: output.clone(),
                destination: "Imported".to_string(),
                source_locators: WikiSourceLocators::Summary,
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
                source_locators: WikiSourceLocators::Summary,
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
                source_locators: WikiSourceLocators::Summary,
                dry_run: false,
            },
        );
        assert!(failed.is_err());
        assert!(!vault.join("Rollback").exists());
    }

    fn spec_example(path: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo")
            .join("docs/specs/wiki-package")
            .join(path)
    }

    fn empty_vault() -> (tempfile::TempDir, VaultPaths) {
        let temp = tempdir().expect("temp");
        let vault = temp.path().join("vault");
        fs::create_dir_all(&vault).expect("vault");
        let paths = VaultPaths::new(&vault);
        initialize_vulcan_dir(&paths).expect("init");
        scan_vault(&paths, ScanMode::Full).expect("scan");
        (temp, paths)
    }

    #[test]
    fn v2_import_records_source_locators_and_keeps_evidence_external() {
        let (_temp, paths) = empty_vault();
        let package = spec_example("v2/examples/sourced.wikibundle");
        let report = import_wiki_package(
            &paths,
            &WikiPackageImportRequest {
                package: package.clone(),
                destination: "Lore".to_string(),
                source_locators: WikiSourceLocators::Full,
                dry_run: false,
            },
        )
        .expect("import");
        assert_eq!(report.format_version, 2);
        assert_eq!((report.notes, report.assets), (3, 1));
        assert_eq!(report.summary.claims, 1);
        assert_eq!(
            report.annotated_notes,
            vec!["Lore/Characters/Alice.md", "Lore/Places/Harbor.md"]
        );
        let vault = paths.vault_root();
        assert_eq!(
            fs::read(vault.join("Lore/Home.md")).expect("home"),
            fs::read(package.join("content/Home.md")).expect("package home")
        );
        let alice = fs::read_to_string(vault.join("Lore/Characters/Alice.md")).expect("alice");
        let (frontmatter, body) = parse_frontmatter_document(&alice, false).expect("parse");
        let frontmatter = frontmatter.expect("frontmatter");
        assert_eq!(frontmatter["title"], YamlValue::String("Alice".to_string()));
        let source = &frontmatter["vulcan"]["source"];
        assert_eq!(
            source["artifact"].as_str(),
            Some(report.package_identity.as_str())
        );
        assert_eq!(
            source["member"].as_str(),
            Some("content/Characters/Alice.md")
        );
        assert_eq!(
            source["spans"][0]["locators"][0]["source_id"].as_str(),
            Some("script")
        );
        assert!(body.contains("Alice has lived by the harbor"));
        assert!(!vault.join("Lore/sources").exists());
        assert!(!vault.join("Lore/knowledge.jsonl").exists());
    }

    #[test]
    fn v2_import_defaults_to_compact_source_summaries() {
        let (_temp, paths) = empty_vault();
        let report = import_wiki_package(
            &paths,
            &WikiPackageImportRequest {
                package: spec_example("v2/examples/sourced.wikibundle"),
                destination: "Lore".to_string(),
                source_locators: WikiSourceLocators::default(),
                dry_run: false,
            },
        )
        .expect("import");
        assert_eq!(report.source_locators, WikiSourceLocators::Summary);
        assert_eq!(report.annotated_notes.len(), 2);
        let harbor =
            fs::read_to_string(paths.vault_root().join("Lore/Places/Harbor.md")).expect("harbor");
        let (frontmatter, _) = parse_frontmatter_document(&harbor, false).expect("parse");
        let source = &frontmatter.expect("frontmatter")["vulcan"]["source"];
        assert_eq!(
            source["artifact"].as_str(),
            Some(report.package_identity.as_str())
        );
        assert_eq!(source["member"].as_str(), Some("content/Places/Harbor.md"));
        assert_eq!(source["mappings"].as_u64(), Some(1));
        assert!(source.get("spans").is_none());
    }

    #[test]
    fn v2_import_refuses_to_overwrite_existing_source_frontmatter() {
        let (temp, paths) = empty_vault();
        let package = temp.path().join("conflict.wikibundle");
        copy_tree(&spec_example("v2/examples/sourced.wikibundle"), &package);
        let note = package.join("content/Places/Harbor.md");
        let bytes =
            b"---\nvulcan:\n  source: kept\n---\n# Harbor\n\nThe harbor bells ring at dawn.\n";
        fs::write(&note, bytes).expect("note");
        // Keep the package valid: shift source-map spans and redeclare the note.
        let shift = bytes.len()
            - fs::read(spec_example(
                "v2/examples/sourced.wikibundle/content/Places/Harbor.md",
            ))
            .expect("orig")
            .len();
        let map_path = package.join("source-map.jsonl");
        let map = fs::read_to_string(&map_path).expect("map");
        let map = map
            .replace(
                "\"start\":2,\"end\":8",
                &format!("\"start\":{},\"end\":{}", 2 + shift, 8 + shift),
            )
            .replace(
                "\"start\":10,\"end\":40",
                &format!("\"start\":{},\"end\":{}", 10 + shift, 40 + shift),
            );
        fs::write(&map_path, &map).expect("map");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(package.join("wiki.json")).expect("wiki"))
                .expect("json");
        for member in manifest["members"].as_array_mut().expect("members") {
            let path = member["path"].as_str().expect("path").to_string();
            if path == "content/Places/Harbor.md" || path == "source-map.jsonl" {
                let data = fs::read(package.join(&path)).expect("member");
                member["size"] = serde_json::json!(data.len());
                member["digest"] = serde_json::json!(blake3_digest(&data));
            }
        }
        fs::write(
            package.join("wiki.json"),
            serde_json::to_vec_pretty(&manifest).expect("json"),
        )
        .expect("wiki");
        assert!(inspect_wiki_package(&package).expect("inspect").valid);
        let error = import_wiki_package(
            &paths,
            &WikiPackageImportRequest {
                package,
                destination: "Lore".to_string(),
                source_locators: WikiSourceLocators::Summary,
                dry_run: true,
            },
        )
        .expect_err("conflict");
        assert!(error.to_string().contains("vulcan.source"), "{error}");
        assert!(!paths.vault_root().join("Lore").exists());
    }

    #[test]
    fn v1_packages_still_import_byte_for_byte() {
        let (_temp, paths) = empty_vault();
        let package = spec_example("v1/examples/minimal.wikibundle");
        let report = import_wiki_package(
            &paths,
            &WikiPackageImportRequest {
                package: package.clone(),
                destination: "Old".to_string(),
                source_locators: WikiSourceLocators::Summary,
                dry_run: false,
            },
        )
        .expect("import v1");
        assert_eq!(report.format_version, 1);
        assert!(report.annotated_notes.is_empty());
        assert_eq!(
            fs::read(paths.vault_root().join("Old/Home.md")).expect("imported"),
            fs::read(package.join("content/Home.md")).expect("package")
        );
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).expect("dir");
        for entry in fs::read_dir(from).expect("read") {
            let entry = entry.expect("entry");
            let target = to.join(entry.file_name());
            if entry.file_type().expect("type").is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).expect("copy");
            }
        }
    }
}
