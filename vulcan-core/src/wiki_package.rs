//! Reader and validator for Markdown Wiki Package snapshots.

use crate::textbundle::{
    copy_member, observe_members, read_member, TextBundleError, TextBundleRepresentation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::io::Write;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub const WIKI_PACKAGE_FORMAT: &str = "dev.tionis.markdown-wiki-package";
pub const WIKI_PACKAGE_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_NOTE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiPackageProducer {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WikiPackageMemberRole {
    Note,
    Asset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiPackageMember {
    pub path: String,
    pub role: WikiPackageMemberRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    pub size: u64,
    pub digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WikiPackageManifest {
    pub format: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub producer: WikiPackageProducer,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<String>,
    pub members: Vec<WikiPackageMember>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WikiPackageDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WikiPackageDiagnostic {
    pub severity: WikiPackageDiagnosticSeverity,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WikiPackage {
    #[serde(skip)]
    pub package_path: PathBuf,
    pub representation: TextBundleRepresentation,
    pub identity: String,
    pub valid: bool,
    pub manifest: Option<WikiPackageManifest>,
    pub diagnostics: Vec<WikiPackageDiagnostic>,
}

impl WikiPackage {
    pub fn copy_member_to(
        &self,
        path: &str,
        writer: &mut impl Write,
    ) -> Result<(), WikiPackageError> {
        let expected = self
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.members.iter().find(|member| member.path == path))
            .ok_or_else(|| WikiPackageError::Invalid(format!("undeclared member: {path}")))?;
        let (size, digest) = copy_member(&self.package_path, self.representation, path, writer)?;
        if size != expected.size || digest != expected.digest {
            return Err(WikiPackageError::Invalid(format!(
                "member changed since inspection: {path}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum WikiPackageError {
    Container(TextBundleError),
    Invalid(String),
}

impl Display for WikiPackageError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Container(error) => write!(formatter, "portable package error: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid wiki package: {message}"),
        }
    }
}

impl std::error::Error for WikiPackageError {}

impl From<TextBundleError> for WikiPackageError {
    fn from(error: TextBundleError) -> Self {
        Self::Container(error)
    }
}

#[allow(clippy::too_many_lines)]
pub fn inspect_wiki_package(path: &Path) -> Result<WikiPackage, WikiPackageError> {
    let representation = if path.is_dir() {
        TextBundleRepresentation::Directory
    } else {
        TextBundleRepresentation::Zip
    };
    let observed = observe_members(path, representation)?;
    let mut diagnostics = Vec::new();
    let manifest = match read_member(path, representation, "wiki.json", MAX_MANIFEST_BYTES) {
        Ok(bytes) => match serde_json::from_slice::<WikiPackageManifest>(&bytes) {
            Ok(manifest) => Some(manifest),
            Err(error) => {
                push_error(
                    &mut diagnostics,
                    "manifest_invalid",
                    error.to_string(),
                    "wiki.json",
                );
                None
            }
        },
        Err(error) => {
            push_error(
                &mut diagnostics,
                "manifest_missing",
                error.to_string(),
                "wiki.json",
            );
            None
        }
    };
    let mut identity = format!("blake3:{}", blake3::hash(b""));
    if let Some(manifest) = manifest.as_ref() {
        if manifest.format != WIKI_PACKAGE_FORMAT {
            push_error(
                &mut diagnostics,
                "format_unsupported",
                "unsupported package format",
                "wiki.json",
            );
        }
        if manifest.version != WIKI_PACKAGE_VERSION {
            push_error(
                &mut diagnostics,
                "version_unsupported",
                "unsupported package version",
                "wiki.json",
            );
        }
        if manifest.producer.name.trim().is_empty() {
            push_error(
                &mut diagnostics,
                "producer_invalid",
                "producer name must not be empty",
                "wiki.json",
            );
        }
        let mut declared = BTreeMap::new();
        let mut folded = BTreeSet::new();
        for member in &manifest.members {
            if !valid_member_path(&member.path) {
                push_error(
                    &mut diagnostics,
                    "member_path_invalid",
                    "member path must be below content/",
                    &member.path,
                );
                continue;
            }
            if !folded.insert(member.path.to_lowercase())
                || declared.insert(member.path.clone(), member).is_some()
            {
                push_error(
                    &mut diagnostics,
                    "member_duplicate",
                    "duplicate or case-fold-colliding declaration",
                    &member.path,
                );
            }
            if !is_blake3_digest(&member.digest) {
                push_error(
                    &mut diagnostics,
                    "member_digest_invalid",
                    "digest must use blake3 lowercase hex",
                    &member.path,
                );
            }
            let is_markdown = Path::new(&member.path)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("md"));
            if (member.role == WikiPackageMemberRole::Note) != is_markdown {
                push_error(
                    &mut diagnostics,
                    "member_role_invalid",
                    "note role must correspond exactly to .md paths",
                    &member.path,
                );
            }
        }
        for member in observed
            .values()
            .filter(|member| member.path != "wiki.json")
        {
            match declared.get(&member.path) {
                None => push_error(
                    &mut diagnostics,
                    "member_undeclared",
                    "package member is not declared",
                    &member.path,
                ),
                Some(expected) if expected.size != member.size => push_error(
                    &mut diagnostics,
                    "member_size_mismatch",
                    "declared size does not match bytes",
                    &member.path,
                ),
                Some(expected) if expected.digest != member.digest => push_error(
                    &mut diagnostics,
                    "member_digest_mismatch",
                    "declared digest does not match bytes",
                    &member.path,
                ),
                Some(expected) if expected.role == WikiPackageMemberRole::Note => {
                    if read_member(path, representation, &member.path, MAX_NOTE_BYTES)
                        .and_then(|bytes| {
                            String::from_utf8(bytes)
                                .map_err(|error| TextBundleError::InvalidMember(error.to_string()))
                        })
                        .is_err()
                    {
                        push_error(
                            &mut diagnostics,
                            "note_not_utf8",
                            "note is not valid UTF-8",
                            &member.path,
                        );
                    }
                }
                Some(_) => {}
            }
        }
        for member in &manifest.members {
            if !observed.contains_key(&member.path) {
                push_error(
                    &mut diagnostics,
                    "member_missing",
                    "declared member is missing",
                    &member.path,
                );
            }
        }
        identity = logical_identity(&manifest.members);
    }
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.code.cmp(&right.code))
    });
    let valid = !diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == WikiPackageDiagnosticSeverity::Error);
    Ok(WikiPackage {
        package_path: path.to_path_buf(),
        representation,
        identity,
        valid,
        manifest,
        diagnostics,
    })
}

#[must_use]
pub fn logical_identity(members: &[WikiPackageMember]) -> String {
    let mut members = members.to_vec();
    members.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = blake3::Hasher::new();
    for member in members {
        let path = serde_json::to_string(&member.path).expect("path serializes");
        hasher.update(
            format!(
                "{{\"path\":{path},\"role\":\"{}\",\"size\":{},\"digest\":\"{}\"}}\n",
                match member.role {
                    WikiPackageMemberRole::Note => "note",
                    WikiPackageMemberRole::Asset => "asset",
                },
                member.size,
                member.digest
            )
            .as_bytes(),
        );
    }
    format!("blake3:{}", hasher.finalize())
}

fn is_blake3_digest(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_member_path(path: &str) -> bool {
    path.starts_with("content/")
        && path != "content/"
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.nfc().collect::<String>() == path
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn push_error(
    diagnostics: &mut Vec<WikiPackageDiagnostic>,
    code: &str,
    message: impl Into<String>,
    path: &str,
) {
    diagnostics.push(WikiPackageDiagnostic {
        severity: WikiPackageDiagnosticSeverity::Error,
        code: code.to_string(),
        message: message.into(),
        path: Some(path.to_string()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn manifest(note: &[u8]) -> WikiPackageManifest {
        WikiPackageManifest {
            format: WIKI_PACKAGE_FORMAT.to_string(),
            version: 1,
            title: Some("Synthetic".to_string()),
            producer: WikiPackageProducer {
                name: "test".to_string(),
                version: None,
                extensions: serde_json::Map::new(),
            },
            lineage: Vec::new(),
            members: vec![WikiPackageMember {
                path: "content/Home.md".to_string(),
                role: WikiPackageMemberRole::Note,
                media_type: Some("text/markdown".to_string()),
                size: note.len() as u64,
                digest: format!("blake3:{}", blake3::hash(note)),
                document_id: None,
                extensions: serde_json::Map::new(),
            }],
            extensions: serde_json::Map::new(),
        }
    }

    #[test]
    fn inspects_valid_directory_and_detects_changed_bytes() {
        let temp = tempdir().expect("temp");
        let root = temp.path().join("sample.wikibundle");
        fs::create_dir_all(root.join("content")).expect("dirs");
        let note = b"# Home\n";
        fs::write(root.join("content/Home.md"), note).expect("note");
        fs::write(
            root.join("wiki.json"),
            serde_json::to_vec_pretty(&manifest(note)).expect("json"),
        )
        .expect("manifest");
        let package = inspect_wiki_package(&root).expect("inspect");
        assert!(package.valid, "{:?}", package.diagnostics);
        fs::write(root.join("content/Home.md"), "changed").expect("change");
        let package = inspect_wiki_package(&root).expect("inspect changed");
        assert!(!package.valid);
        assert!(package
            .diagnostics
            .iter()
            .any(|item| item.code == "member_digest_mismatch"));
    }

    #[test]
    fn rejects_casefold_duplicate_declarations() {
        let temp = tempdir().expect("temp");
        let root = temp.path().join("duplicate.wikibundle");
        fs::create_dir_all(root.join("content")).expect("dirs");
        let note = b"# Home\n";
        fs::write(root.join("content/Home.md"), note).expect("note");
        let mut manifest = manifest(note);
        let mut duplicate = manifest.members[0].clone();
        duplicate.path = "content/home.md".to_string();
        manifest.members.push(duplicate);
        fs::write(
            root.join("wiki.json"),
            serde_json::to_vec_pretty(&manifest).expect("json"),
        )
        .expect("manifest");
        let package = inspect_wiki_package(&root).expect("inspect");
        assert!(!package.valid);
        assert!(package
            .diagnostics
            .iter()
            .any(|item| item.code == "member_duplicate"));
    }

    #[test]
    fn checked_in_synthetic_example_matches_identity_vector() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo");
        let package = inspect_wiki_package(
            &repo.join("docs/specs/wiki-package/v1/examples/minimal.wikibundle"),
        )
        .expect("inspect example");
        assert!(package.valid, "{:?}", package.diagnostics);
        assert!(package
            .manifest
            .as_ref()
            .expect("manifest")
            .extensions
            .contains_key("example.extension"));
        let vector: Value = serde_json::from_slice(
            &fs::read(repo.join("docs/specs/wiki-package/v1/identity-test-vector.json"))
                .expect("vector"),
        )
        .expect("vector json");
        assert_eq!(vector["identity"].as_str(), Some(package.identity.as_str()));
        let _: Value = serde_json::from_slice(
            &fs::read(repo.join("docs/specs/wiki-package/v1/wiki.schema.json")).expect("schema"),
        )
        .expect("schema json");
    }
}
