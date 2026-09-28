//! Describes how a vault sits inside an enclosing Git repository or MkDocs
//! project, with actionable hints for common nested-vault mistakes.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use vulcan_core::vault_discovery::{
    enclosing_git_work_tree, enclosing_mkdocs_project, read_vault_pointer, render_vault_pointer,
    MkDocsProject, VAULT_POINTER_FILE_NAME,
};
use vulcan_core::{git_repository_layout, GitRepositoryLayout, VaultPaths};

use crate::AppError;

/// The MkDocs project that contains a vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MkDocsLayout {
    pub config_file: PathBuf,
    pub docs_dir: PathBuf,
    /// Whether the vault root is exactly the configured `docs_dir`.
    pub vault_is_docs_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultLayoutReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<GitRepositoryLayout>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mkdocs: Option<MkDocsLayout>,
    /// Repository-root `.vulcan.toml` that names this vault.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_pointer: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

impl VaultLayoutReport {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.repository.is_none()
            && self.mkdocs.is_none()
            && self.repository_pointer.is_none()
            && self.hints.is_empty()
    }
}

/// Inspects the vault's enclosing Git work tree and MkDocs project.
#[must_use]
pub fn inspect_vault_layout(paths: &VaultPaths) -> VaultLayoutReport {
    let vault_root = canonical(paths.vault_root());
    let repository = git_repository_layout(paths.vault_root());
    let mkdocs = enclosing_mkdocs_project(&vault_root).map(|project| {
        let vault_is_docs_dir = canonical(&project.docs_dir) == vault_root;
        (project, vault_is_docs_dir)
    });
    let mut hints = mkdocs
        .as_ref()
        .map(|(project, vault_is_docs_dir)| {
            mkdocs_hints(paths.vault_root(), &vault_root, project, *vault_is_docs_dir)
        })
        .unwrap_or_default();
    let repository_pointer = inspect_repository_pointer(&vault_root, &mut hints);
    VaultLayoutReport {
        repository_pointer,
        repository,
        mkdocs: mkdocs.map(|(project, vault_is_docs_dir)| MkDocsLayout {
            config_file: project.config_file,
            docs_dir: project.docs_dir,
            vault_is_docs_dir,
        }),
        hints,
    }
}

fn mkdocs_hints(
    vault_path: &Path,
    vault_root: &Path,
    project: &MkDocsProject,
    vault_is_docs_dir: bool,
) -> Vec<String> {
    let mut hints = Vec::new();
    let config_name = project
        .config_file
        .file_name()
        .map_or_else(|| "mkdocs.yml".into(), |name| name.to_string_lossy());
    if vault_is_docs_dir {
        let config = fs::read_to_string(&project.config_file).unwrap_or_default();
        if vault_path.join("AGENTS.md").is_file() && !config.contains("AGENTS.md") {
            hints.push(format!(
                "MkDocs will publish AGENTS.md as a site page; add `AGENTS.md` to `exclude_docs` in {config_name}"
            ));
        }
    } else if canonical(&project.project_root) == vault_root && project.docs_dir.is_dir() {
        let docs_dir = project
            .docs_dir
            .strip_prefix(&project.project_root)
            .unwrap_or(&project.docs_dir)
            .display()
            .to_string();
        hints.push(format!(
            "this vault is the MkDocs project root, but MkDocs only builds `{docs_dir}/`; use `--vault {docs_dir}` or run Vulcan from inside `{docs_dir}/` to scope indexing to the site content"
        ));
    }
    hints
}

/// Reports the repository-root pointer naming this vault, or a hint when a
/// nested vault has none or the pointer names another directory.
fn inspect_repository_pointer(vault_root: &Path, hints: &mut Vec<String>) -> Option<PathBuf> {
    let work_tree = enclosing_git_work_tree(vault_root).filter(|root| root != vault_root)?;
    match read_vault_pointer(&work_tree) {
        Ok(Some(pointer)) if canonical(&pointer.vault_root) == vault_root => Some(pointer.file),
        Ok(Some(pointer)) => {
            hints.push(format!(
                "{} names `{}` as the repository's vault, not this vault",
                pointer.file.display(),
                pointer.vault
            ));
            None
        }
        Ok(None) => {
            hints.push(format!(
                "run `vulcan init --repository-pointer` to write {} so Vulcan finds this vault from anywhere in the repository",
                work_tree.join(VAULT_POINTER_FILE_NAME).display()
            ));
            None
        }
        Err(error) => {
            hints.push(error.to_string());
            None
        }
    }
}

/// Outcome of writing a repository-root vault pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryPointerStatus {
    Created,
    Kept,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryPointerReport {
    pub path: PathBuf,
    pub vault: String,
    pub status: RepositoryPointerStatus,
}

/// Writes `.vulcan.toml` at the root of the Git work tree enclosing the vault
/// so commands run anywhere in the repository use this vault. An existing
/// pointer naming the same vault is kept; one naming anything else is never
/// overwritten.
pub fn write_repository_pointer(
    paths: &VaultPaths,
    dry_run: bool,
) -> Result<RepositoryPointerReport, AppError> {
    let vault_root = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let work_tree = enclosing_git_work_tree(&vault_root)
        .filter(|root| root != &vault_root)
        .ok_or_else(|| {
            AppError::operation(
                "a repository pointer needs a vault nested below a Git work-tree root; a vault at the repository root is found without one",
            )
        })?;
    let vault = vault_root
        .strip_prefix(&work_tree)
        .map_err(AppError::operation)?
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let path = work_tree.join(VAULT_POINTER_FILE_NAME);
    match read_vault_pointer(&work_tree) {
        Ok(Some(existing)) if canonical(&existing.vault_root) == vault_root => {
            return Ok(RepositoryPointerReport {
                path,
                vault: existing.vault,
                status: RepositoryPointerStatus::Kept,
            });
        }
        Ok(Some(existing)) => {
            return Err(AppError::operation(format!(
                "{} already names `{}` as the repository's vault; edit or remove it deliberately",
                path.display(),
                existing.vault
            )));
        }
        Ok(None) if path.exists() => {
            return Err(AppError::operation(format!(
                "{} exists but is not a regular file",
                path.display()
            )));
        }
        Ok(None) => {}
        Err(error) => {
            return Err(AppError::operation(format!(
                "{error}; fix or remove it before writing a new pointer"
            )));
        }
    }
    if !dry_run {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| {
                std::io::Write::write_all(&mut file, render_vault_pointer(&vault).as_bytes())
            })
            .map_err(AppError::operation)?;
    }
    Ok(RepositoryPointerReport {
        path,
        vault,
        status: RepositoryPointerStatus::Created,
    })
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn git_init(path: &Path) {
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(path)
            .status()
            .expect("git should run");
        assert!(status.success());
    }

    #[test]
    fn docs_dir_vault_reports_repository_prefix_and_agents_hint() {
        let temp = TempDir::new().expect("temp dir");
        let site = temp.path().join("site");
        fs::create_dir_all(site.join("docs")).expect("docs");
        git_init(&site);
        fs::write(site.join("mkdocs.yml"), "site_name: Demo\n").expect("config");
        fs::write(site.join("docs/AGENTS.md"), "# Agents\n").expect("agents");
        write_repository_pointer(&VaultPaths::new(site.join("docs")), false).expect("pointer");

        let report = inspect_vault_layout(&VaultPaths::new(site.join("docs")));
        assert!(report.repository_pointer.is_some());

        let repository = report.repository.expect("repository layout");
        assert_eq!(repository.vault_prefix, "docs");
        let mkdocs = report.mkdocs.expect("mkdocs layout");
        assert!(mkdocs.vault_is_docs_dir);
        assert_eq!(report.hints.len(), 1);
        assert!(report.hints[0].contains("exclude_docs"));

        fs::write(
            site.join("mkdocs.yml"),
            "site_name: Demo\nexclude_docs: |\n  AGENTS.md\n",
        )
        .expect("config");
        assert!(inspect_vault_layout(&VaultPaths::new(site.join("docs")))
            .hints
            .is_empty());
    }

    #[test]
    fn project_root_vault_suggests_scoping_to_docs_dir() {
        let temp = TempDir::new().expect("temp dir");
        let site = temp.path().join("site");
        fs::create_dir_all(site.join("content")).expect("content");
        fs::write(site.join("mkdocs.yml"), "docs_dir: content\n").expect("config");

        let report = inspect_vault_layout(&VaultPaths::new(&site));

        let mkdocs = report.mkdocs.expect("mkdocs layout");
        assert!(!mkdocs.vault_is_docs_dir);
        assert_eq!(report.hints.len(), 1);
        assert!(report.hints[0].contains("--vault content"));
    }

    #[test]
    fn repository_pointer_is_suggested_written_once_and_never_overwritten() {
        let temp = TempDir::new().expect("temp dir");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join("notes/wiki")).expect("vault");
        fs::create_dir_all(repo.join("other")).expect("other");
        git_init(&repo);
        let paths = VaultPaths::new(repo.join("notes/wiki"));

        let before = inspect_vault_layout(&paths);
        assert!(before.repository_pointer.is_none());
        assert_eq!(before.hints.len(), 1);
        assert!(before.hints[0].contains("--repository-pointer"));

        let preview = write_repository_pointer(&paths, true).expect("dry run");
        assert_eq!(preview.status, RepositoryPointerStatus::Created);
        assert!(!repo.join(VAULT_POINTER_FILE_NAME).exists());

        let created = write_repository_pointer(&paths, false).expect("write");
        assert_eq!(created.status, RepositoryPointerStatus::Created);
        assert_eq!(created.vault, "notes/wiki");
        let kept = write_repository_pointer(&paths, false).expect("rewrite");
        assert_eq!(kept.status, RepositoryPointerStatus::Kept);
        let after = inspect_vault_layout(&paths);
        assert!(after.repository_pointer.is_some());
        assert!(after.hints.is_empty());

        let other = VaultPaths::new(repo.join("other"));
        let error = write_repository_pointer(&other, false).expect_err("conflicting pointer");
        assert!(error.to_string().contains("already names `notes/wiki`"));
        assert!(inspect_vault_layout(&other).hints[0].contains("not this vault"));

        let root_error =
            write_repository_pointer(&VaultPaths::new(&repo), false).expect_err("root vault");
        assert!(root_error.to_string().contains("nested below"));
    }

    #[test]
    fn plain_directory_has_an_empty_layout() {
        let temp = TempDir::new().expect("temp dir");
        let vault = temp.path().join("vault");
        fs::create_dir_all(&vault).expect("vault");
        git_init(&vault);
        let report = inspect_vault_layout(&VaultPaths::new(&vault));
        assert_eq!(
            report.repository.map(|layout| layout.vault_prefix),
            Some(String::new())
        );
        assert!(report.mkdocs.is_none());
        assert!(report.hints.is_empty());
    }
}
