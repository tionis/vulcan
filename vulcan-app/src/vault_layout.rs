//! Describes how a vault sits inside an enclosing Git repository or MkDocs
//! project, with actionable hints for common nested-vault mistakes.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use vulcan_core::vault_discovery::{enclosing_mkdocs_project, MkDocsProject};
use vulcan_core::{git_repository_layout, GitRepositoryLayout, VaultPaths};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

impl VaultLayoutReport {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.repository.is_none() && self.mkdocs.is_none() && self.hints.is_empty()
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
    let hints = mkdocs
        .as_ref()
        .map(|(project, vault_is_docs_dir)| {
            mkdocs_hints(paths.vault_root(), &vault_root, project, *vault_is_docs_dir)
        })
        .unwrap_or_default();
    VaultLayoutReport {
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

        let report = inspect_vault_layout(&VaultPaths::new(site.join("docs")));

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
