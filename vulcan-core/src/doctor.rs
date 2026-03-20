use crate::maintenance::{repair_fts, RepairFtsQuery};
use crate::scan::discover_relative_paths;
use crate::{initialize_vault, VaultPaths};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum DoctorError {
    Init(crate::InitError),
    Maintenance(crate::MaintenanceError),
    Scan(crate::ScanError),
    Sqlite(rusqlite::Error),
}

impl Display for DoctorError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init(error) => write!(formatter, "{error}"),
            Self::Maintenance(error) => write!(formatter, "{error}"),
            Self::Scan(error) => write!(formatter, "{error}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for DoctorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Init(error) => Some(error),
            Self::Maintenance(error) => Some(error),
            Self::Scan(error) => Some(error),
            Self::Sqlite(error) => Some(error),
        }
    }
}

impl From<crate::InitError> for DoctorError {
    fn from(error: crate::InitError) -> Self {
        Self::Init(error)
    }
}

impl From<crate::MaintenanceError> for DoctorError {
    fn from(error: crate::MaintenanceError) -> Self {
        Self::Maintenance(error)
    }
}

impl From<crate::ScanError> for DoctorError {
    fn from(error: crate::ScanError) -> Self {
        Self::Scan(error)
    }
}

impl From<rusqlite::Error> for DoctorError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub summary: DoctorSummary,
    pub unresolved_links: Vec<DoctorLinkIssue>,
    pub ambiguous_links: Vec<DoctorLinkIssue>,
    pub parse_failures: Vec<DoctorDiagnosticIssue>,
    pub stale_index_rows: Vec<String>,
    pub missing_index_rows: Vec<String>,
    pub orphan_notes: Vec<String>,
    pub html_links: Vec<DoctorDiagnosticIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorSummary {
    pub unresolved_links: usize,
    pub ambiguous_links: usize,
    pub parse_failures: usize,
    pub stale_index_rows: usize,
    pub missing_index_rows: usize,
    pub orphan_notes: usize,
    pub html_links: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorFixReport {
    pub dry_run: bool,
    pub issues_before: DoctorSummary,
    pub issues_after: Option<DoctorSummary>,
    pub fixes: Vec<DoctorFixAction>,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorFixAction {
    pub kind: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorLinkIssue {
    pub document_path: Option<String>,
    pub message: String,
    pub target: Option<String>,
    pub matches: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorDiagnosticIssue {
    pub document_path: Option<String>,
    pub message: String,
    pub byte_range: Option<DoctorByteRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedDocument {
    id: String,
    path: String,
    extension: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredDiagnostic {
    document_path: Option<String>,
    kind: String,
    message: String,
    detail: String,
}

pub fn doctor_vault(paths: &VaultPaths) -> Result<DoctorReport, DoctorError> {
    let on_disk_paths = discover_relative_paths(paths.vault_root())?
        .into_iter()
        .collect::<BTreeSet<_>>();

    let Some(connection) = open_existing_cache(paths)? else {
        return Ok(empty_report(on_disk_paths));
    };

    let indexed_documents = load_indexed_documents(&connection)?;
    let path_by_id = indexed_documents
        .iter()
        .map(|document| (document.id.clone(), document.path.clone()))
        .collect::<HashMap<_, _>>();
    let reconciliation = reconcile_paths(&indexed_documents, &on_disk_paths);
    let sections = classify_diagnostics(load_diagnostics(&connection)?, &path_by_id);
    let orphan_notes = load_orphan_notes(&connection, &indexed_documents, &reconciliation.on_disk)?;

    Ok(DoctorReport::new(
        sections.unresolved_links,
        sections.ambiguous_links,
        sections.parse_failures,
        reconciliation.stale_index_rows,
        reconciliation.missing_index_rows,
        orphan_notes,
        sections.html_links,
    ))
}

pub fn doctor_fix(paths: &VaultPaths, dry_run: bool) -> Result<DoctorFixReport, DoctorError> {
    let before = doctor_vault(paths)?;
    let needs_scaffold = !paths.vulcan_dir().exists()
        || !paths.config_file().exists()
        || !paths.cache_db().exists()
        || !paths.gitignore_file().exists();
    let needs_scan = needs_scaffold
        || before.summary.stale_index_rows > 0
        || before.summary.missing_index_rows > 0;
    let needs_fts_repair = !needs_scaffold && fts_repair_needed(paths)?;
    let fixes = planned_fixes(needs_scaffold, needs_scan, needs_fts_repair);

    if dry_run {
        let issues_before = before.summary.clone();
        return Ok(DoctorFixReport {
            dry_run: true,
            issues_before,
            issues_after: None,
            fixes,
            suggestions: fix_suggestions(&before.summary),
        });
    }

    if needs_scaffold {
        initialize_vault(paths)?;
    }
    if needs_scan {
        crate::scan_vault(paths, crate::ScanMode::Incremental)?;
    }
    if needs_fts_repair {
        repair_fts(paths, &RepairFtsQuery { dry_run: false })?;
    }

    let after = doctor_vault(paths)?;
    Ok(DoctorFixReport {
        dry_run: false,
        issues_before: before.summary,
        issues_after: Some(after.summary.clone()),
        fixes,
        suggestions: fix_suggestions(&after.summary),
    })
}

fn empty_report(on_disk_paths: BTreeSet<String>) -> DoctorReport {
    DoctorReport::new(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        on_disk_paths.into_iter().collect(),
        Vec::new(),
        Vec::new(),
    )
}

fn planned_fixes(
    needs_scaffold: bool,
    needs_scan: bool,
    needs_fts_repair: bool,
) -> Vec<DoctorFixAction> {
    let mut fixes = Vec::new();
    if needs_scaffold {
        fixes.push(DoctorFixAction {
            kind: "initialize".to_string(),
            description: "Create or repair .vulcan scaffolding".to_string(),
        });
    }
    if needs_scan {
        fixes.push(DoctorFixAction {
            kind: "scan".to_string(),
            description: "Refresh the cache from disk".to_string(),
        });
    }
    if needs_fts_repair {
        fixes.push(DoctorFixAction {
            kind: "repair_fts".to_string(),
            description: "Rebuild the full-text search index from cached chunks".to_string(),
        });
    }
    fixes
}

fn fix_suggestions(summary: &DoctorSummary) -> Vec<String> {
    let mut suggestions = Vec::new();
    if summary.unresolved_links > 0 {
        suggestions.push(
            "Review unresolved links manually; automatic retargeting is not safe yet.".to_string(),
        );
    }
    if summary.ambiguous_links > 0 {
        suggestions
            .push("Disambiguate ambiguous links by using longer or explicit targets.".to_string());
    }
    if summary.parse_failures > 0 {
        suggestions.push(
            "Fix malformed frontmatter before expecting property queries to be correct."
                .to_string(),
        );
    }
    if summary.html_links > 0 {
        suggestions.push("Convert raw HTML links to Markdown or wikilinks if they should participate in the graph.".to_string());
    }
    suggestions
}

fn fts_repair_needed(paths: &VaultPaths) -> Result<bool, DoctorError> {
    let Some(connection) = open_existing_cache(paths)? else {
        return Ok(false);
    };
    let chunk_rows: i64 =
        connection.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?;
    let search_rows: i64 =
        connection.query_row("SELECT COUNT(*) FROM search_chunk_content", [], |row| {
            row.get(0)
        })?;
    Ok(chunk_rows != search_rows)
}

impl DoctorReport {
    fn new(
        unresolved_links: Vec<DoctorLinkIssue>,
        ambiguous_links: Vec<DoctorLinkIssue>,
        parse_failures: Vec<DoctorDiagnosticIssue>,
        stale_index_rows: Vec<String>,
        missing_index_rows: Vec<String>,
        orphan_notes: Vec<String>,
        html_links: Vec<DoctorDiagnosticIssue>,
    ) -> Self {
        Self {
            summary: DoctorSummary {
                unresolved_links: unresolved_links.len(),
                ambiguous_links: ambiguous_links.len(),
                parse_failures: parse_failures.len(),
                stale_index_rows: stale_index_rows.len(),
                missing_index_rows: missing_index_rows.len(),
                orphan_notes: orphan_notes.len(),
                html_links: html_links.len(),
            },
            unresolved_links,
            ambiguous_links,
            parse_failures,
            stale_index_rows,
            missing_index_rows,
            orphan_notes,
            html_links,
        }
    }
}

fn open_existing_cache(paths: &VaultPaths) -> Result<Option<Connection>, DoctorError> {
    if !paths.cache_db().exists() {
        return Ok(None);
    }

    Ok(Some(Connection::open(paths.cache_db())?))
}

struct Reconciliation {
    stale_index_rows: Vec<String>,
    missing_index_rows: Vec<String>,
    on_disk: HashSet<String>,
}

fn reconcile_paths(
    indexed_documents: &[IndexedDocument],
    on_disk_paths: &BTreeSet<String>,
) -> Reconciliation {
    let indexed_paths = indexed_documents
        .iter()
        .map(|document| document.path.clone())
        .collect::<BTreeSet<_>>();

    Reconciliation {
        stale_index_rows: indexed_paths.difference(on_disk_paths).cloned().collect(),
        missing_index_rows: on_disk_paths.difference(&indexed_paths).cloned().collect(),
        on_disk: on_disk_paths.iter().cloned().collect(),
    }
}

fn load_indexed_documents(connection: &Connection) -> Result<Vec<IndexedDocument>, DoctorError> {
    let mut statement =
        connection.prepare("SELECT id, path, extension FROM documents ORDER BY path")?;
    let rows = statement.query_map([], |row| {
        Ok(IndexedDocument {
            id: row.get(0)?,
            path: row.get(1)?,
            extension: row.get(2)?,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(DoctorError::from)
}

fn load_diagnostics(connection: &Connection) -> Result<Vec<StoredDiagnostic>, DoctorError> {
    let mut statement = connection.prepare(
        "
        SELECT documents.path, diagnostics.kind, diagnostics.message, diagnostics.detail
        FROM diagnostics
        LEFT JOIN documents ON documents.id = diagnostics.document_id
        ORDER BY documents.path, diagnostics.created_at, diagnostics.id
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(StoredDiagnostic {
            document_path: row.get(0)?,
            kind: row.get(1)?,
            message: row.get(2)?,
            detail: row.get(3)?,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(DoctorError::from)
}

fn load_orphan_notes(
    connection: &Connection,
    indexed_documents: &[IndexedDocument],
    paths_on_disk: &HashSet<String>,
) -> Result<Vec<String>, DoctorError> {
    let mut statement = connection.prepare(
        "
        SELECT source_document_id, resolved_target_id, link_kind
        FROM links
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut outbound = HashSet::new();
    let mut inbound = HashSet::new();
    for row in rows {
        let (source_document_id, resolved_target_id, link_kind) = row?;
        if link_kind == "external" {
            continue;
        }

        outbound.insert(source_document_id);
        if let Some(target_document_id) = resolved_target_id {
            inbound.insert(target_document_id);
        }
    }

    let mut orphan_notes = indexed_documents
        .iter()
        .filter(|document| document.extension == "md")
        .filter(|document| paths_on_disk.contains(&document.path))
        .filter(|document| !outbound.contains(&document.id) && !inbound.contains(&document.id))
        .map(|document| document.path.clone())
        .collect::<Vec<_>>();
    orphan_notes.sort();
    Ok(orphan_notes)
}

struct DiagnosticSections {
    unresolved_links: Vec<DoctorLinkIssue>,
    ambiguous_links: Vec<DoctorLinkIssue>,
    parse_failures: Vec<DoctorDiagnosticIssue>,
    html_links: Vec<DoctorDiagnosticIssue>,
}

fn classify_diagnostics(
    diagnostics: Vec<StoredDiagnostic>,
    path_by_id: &HashMap<String, String>,
) -> DiagnosticSections {
    let mut sections = DiagnosticSections {
        unresolved_links: Vec::new(),
        ambiguous_links: Vec::new(),
        parse_failures: Vec::new(),
        html_links: Vec::new(),
    };

    for diagnostic in diagnostics {
        let detail = serde_json::from_str::<Value>(&diagnostic.detail).ok();

        match diagnostic.kind.as_str() {
            "unresolved_link" => {
                classify_link_issue(&mut sections, diagnostic, detail.as_ref(), path_by_id);
            }
            "parse_error" => sections.parse_failures.push(DoctorDiagnosticIssue {
                document_path: diagnostic.document_path,
                message: diagnostic.message,
                byte_range: detail_byte_range(detail.as_ref()),
            }),
            "unsupported_syntax" if diagnostic.message.contains("HTML link detected") => {
                sections.html_links.push(DoctorDiagnosticIssue {
                    document_path: diagnostic.document_path,
                    message: diagnostic.message,
                    byte_range: detail_byte_range(detail.as_ref()),
                });
            }
            _ => {}
        }
    }

    sections.unresolved_links.sort_by(|left, right| {
        left.document_path
            .cmp(&right.document_path)
            .then(left.target.cmp(&right.target))
            .then(left.message.cmp(&right.message))
    });
    sections.ambiguous_links.sort_by(|left, right| {
        left.document_path
            .cmp(&right.document_path)
            .then(left.target.cmp(&right.target))
            .then(left.message.cmp(&right.message))
    });
    sections.parse_failures.sort_by(|left, right| {
        left.document_path
            .cmp(&right.document_path)
            .then(left.message.cmp(&right.message))
    });
    sections.html_links.sort_by(|left, right| {
        left.document_path
            .cmp(&right.document_path)
            .then(left.message.cmp(&right.message))
    });

    sections
}

fn classify_link_issue(
    sections: &mut DiagnosticSections,
    diagnostic: StoredDiagnostic,
    detail: Option<&Value>,
    path_by_id: &HashMap<String, String>,
) {
    let target = detail_target(detail);

    if detail_reason(detail) == Some("ambiguous") {
        sections.ambiguous_links.push(DoctorLinkIssue {
            document_path: diagnostic.document_path,
            message: diagnostic.message,
            target,
            matches: detail_matches(detail, path_by_id),
        });
    } else {
        sections.unresolved_links.push(DoctorLinkIssue {
            document_path: diagnostic.document_path,
            message: diagnostic.message,
            target,
            matches: Vec::new(),
        });
    }
}

fn detail_reason(detail: Option<&Value>) -> Option<&str> {
    detail
        .and_then(|value| value.get("reason"))
        .and_then(Value::as_str)
}

fn detail_target(detail: Option<&Value>) -> Option<String> {
    detail
        .and_then(|value| value.get("target"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn detail_matches(detail: Option<&Value>, path_by_id: &HashMap<String, String>) -> Vec<String> {
    let mut matches = detail
        .and_then(|value| value.get("matches"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(|value| {
                    path_by_id
                        .get(value)
                        .cloned()
                        .unwrap_or_else(|| value.to_string())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    matches.sort();
    matches
}

fn detail_byte_range(detail: Option<&Value>) -> Option<DoctorByteRange> {
    let byte_range = detail
        .and_then(|value| value.get("byte_range"))
        .and_then(Value::as_object)?;

    Some(DoctorByteRange {
        start: usize::try_from(byte_range.get("start")?.as_u64()?).ok()?,
        end: usize::try_from(byte_range.get("end")?.as_u64()?).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan_vault, search_vault, ScanMode, SearchQuery};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn doctor_reports_clean_basic_vault() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = doctor_vault(&paths).expect("doctor should succeed");

        assert_eq!(
            report.summary,
            DoctorSummary {
                unresolved_links: 0,
                ambiguous_links: 0,
                parse_failures: 0,
                stale_index_rows: 0,
                missing_index_rows: 0,
                orphan_notes: 0,
                html_links: 0,
            }
        );
    }

    #[test]
    fn doctor_reports_zero_issues_on_clean_fixtures() {
        for fixture in ["basic", "move-rewrite"] {
            let temp_dir = TempDir::new().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault(fixture, &vault_root);
            let paths = VaultPaths::new(&vault_root);

            scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
            let report = doctor_vault(&paths).expect("doctor should succeed");

            assert_eq!(
                report.summary,
                DoctorSummary {
                    unresolved_links: 0,
                    ambiguous_links: 0,
                    parse_failures: 0,
                    stale_index_rows: 0,
                    missing_index_rows: 0,
                    orphan_notes: 0,
                    html_links: 0,
                },
                "fixture {fixture} should be doctor-clean"
            );
        }
    }

    #[test]
    fn doctor_reports_broken_frontmatter() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("broken-frontmatter", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = doctor_vault(&paths).expect("doctor should succeed");

        assert_eq!(report.summary.parse_failures, 1);
        assert_eq!(report.parse_failures.len(), 1);
        assert_eq!(
            report.parse_failures[0].document_path.as_deref(),
            Some("Broken.md")
        );
    }

    #[test]
    fn doctor_reports_ambiguous_and_unresolved_links() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let ambiguous_root = temp_dir.path().join("ambiguous");
        copy_fixture_vault("ambiguous-links", &ambiguous_root);
        let ambiguous_paths = VaultPaths::new(&ambiguous_root);

        scan_vault(&ambiguous_paths, ScanMode::Full).expect("scan should succeed");
        let ambiguous_report = doctor_vault(&ambiguous_paths).expect("doctor should succeed");

        assert_eq!(ambiguous_report.summary.ambiguous_links, 1);
        assert_eq!(ambiguous_report.summary.unresolved_links, 0);
        assert_eq!(
            ambiguous_report.ambiguous_links[0].matches,
            vec![
                "Archive/Topic.md".to_string(),
                "Projects/Topic.md".to_string()
            ]
        );

        let missing_root = temp_dir.path().join("missing");
        fs::create_dir_all(&missing_root).expect("vault root should be created");
        fs::write(
            missing_root.join("Home.md"),
            "# Home\n\nMissing target [[Ghost]].\n",
        )
        .expect("note should be written");
        let missing_paths = VaultPaths::new(&missing_root);

        scan_vault(&missing_paths, ScanMode::Full).expect("scan should succeed");
        let missing_report = doctor_vault(&missing_paths).expect("doctor should succeed");

        assert_eq!(missing_report.summary.unresolved_links, 1);
        assert_eq!(
            missing_report.unresolved_links[0].target.as_deref(),
            Some("Ghost")
        );
    }

    #[test]
    fn doctor_reports_stale_missing_orphan_and_html_rows() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(&vault_root).expect("vault root should be created");
        fs::write(
            vault_root.join("Alpha.md"),
            "# Alpha\n\n<a href=\"https://example.com\">Example</a>\n",
        )
        .expect("alpha note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        fs::remove_file(vault_root.join("Alpha.md")).expect("alpha note should be removed");
        fs::write(vault_root.join("Beta.md"), "# Beta\n").expect("beta note should be written");

        let report = doctor_vault(&paths).expect("doctor should succeed");

        assert_eq!(report.stale_index_rows, vec!["Alpha.md".to_string()]);
        assert_eq!(report.missing_index_rows, vec!["Beta.md".to_string()]);
        assert!(report.orphan_notes.is_empty());
        assert_eq!(report.summary.html_links, 1);

        scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");
        let rescanned_report = doctor_vault(&paths).expect("doctor should succeed");

        assert_eq!(rescanned_report.stale_index_rows, Vec::<String>::new());
        assert_eq!(rescanned_report.missing_index_rows, Vec::<String>::new());
        assert_eq!(rescanned_report.orphan_notes, vec!["Beta.md".to_string()]);
        assert!(rescanned_report.html_links.is_empty());
    }

    #[test]
    fn doctor_without_cache_reports_missing_index_rows_only() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(&vault_root).expect("vault root should be created");
        fs::write(vault_root.join("Home.md"), "# Home\n").expect("home note should be written");
        let paths = VaultPaths::new(&vault_root);

        let report = doctor_vault(&paths).expect("doctor should succeed");

        assert_eq!(report.summary.missing_index_rows, 1);
        assert_eq!(report.missing_index_rows, vec!["Home.md".to_string()]);
        assert_eq!(report.summary.stale_index_rows, 0);
    }

    #[test]
    fn doctor_fix_dry_run_reports_scaffold_and_scan_without_mutating() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        let report = doctor_fix(&paths, true).expect("doctor fix dry run should succeed");

        assert!(report.dry_run);
        assert_eq!(report.fixes.len(), 2);
        assert!(!paths.vulcan_dir().exists());
    }

    #[test]
    fn doctor_fix_repairs_missing_cache_rows_and_search_rows() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        fs::write(vault_root.join("New.md"), "# New\n\nFresh note.\n")
            .expect("new note should write");
        let connection = Connection::open(paths.cache_db()).expect("db should open");
        connection
            .execute("DELETE FROM search_chunk_content", [])
            .expect("search rows should delete");

        let report = doctor_fix(&paths, false).expect("doctor fix should succeed");

        assert!(!report.dry_run);
        assert!(report.fixes.iter().any(|fix| fix.kind == "scan"));
        assert!(search_vault(
            &paths,
            &SearchQuery {
                text: "Fresh".to_string(),
                ..SearchQuery::default()
            }
        )
        .expect("search should succeed")
        .hits
        .iter()
        .any(|hit| hit.document_path == "New.md"));
        assert_eq!(
            report
                .issues_after
                .expect("after summary should be present")
                .missing_index_rows,
            0
        );
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
}
