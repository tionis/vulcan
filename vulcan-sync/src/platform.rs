//! Deterministic target-filesystem preflight for immutable Git trees.

use crate::{
    GitCaseRenamePolicy, GitExecutableBitsPolicy, GitOid, GitPlatformPolicy,
    GitReservedNamesPolicy, GitSymlinkPolicy, GitTreeEntry,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;

pub const GIT_PLATFORM_PREFLIGHT_VERSION: u32 = 1;
const MAX_EXAMPLE_PATHS: usize = 20;
const LONG_PATH_WARNING_BYTES: usize = 240;
const LONG_COMPONENT_WARNING_BYTES: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPlatformDiagnosticSeverity {
    Pass,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitPlatformDiagnostic {
    pub code: String,
    pub severity: GitPlatformDiagnosticSeverity,
    pub count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitPlatformPreflight {
    pub version: u32,
    pub revision: GitOid,
    pub policy: GitPlatformPolicy,
    pub entries: usize,
    pub compatible: bool,
    pub diagnostics: Vec<GitPlatformDiagnostic>,
}

/// Checks a tree against a selected target platform without consulting the host filesystem.
///
/// Errors describe names that cannot be materialized without aliasing or loss. Warnings describe
/// Git metadata that the target profile intentionally represents differently.
#[must_use]
pub fn inspect_git_tree_platform(
    revision: GitOid,
    entries: &[GitTreeEntry],
    policy: GitPlatformPolicy,
) -> GitPlatformPreflight {
    let mut diagnostics = vec![diagnostic(
        "platform.profile",
        GitPlatformDiagnosticSeverity::Pass,
        Vec::new(),
        format!(
            "selected `{}` target platform policy",
            policy.profile.as_str()
        ),
    )];
    diagnostics.extend(path_identity_diagnostics(entries, &policy));
    diagnostics.push(reserved_name_diagnostic(entries, &policy));
    diagnostics.push(executable_bit_diagnostic(entries, &policy));
    diagnostics.push(symlink_diagnostic(entries, &policy));
    diagnostics.push(path_length_diagnostic(entries));

    let compatible = diagnostics
        .iter()
        .all(|item| item.severity != GitPlatformDiagnosticSeverity::Error);
    GitPlatformPreflight {
        version: GIT_PLATFORM_PREFLIGHT_VERSION,
        revision,
        policy,
        entries: entries.len(),
        compatible,
        diagnostics,
    }
}

fn path_identity_diagnostics(
    entries: &[GitTreeEntry],
    policy: &GitPlatformPolicy,
) -> Vec<GitPlatformDiagnostic> {
    if policy.case_only_renames == GitCaseRenamePolicy::IntermediatePath {
        vec![
            collision_diagnostic(
                "platform.case-collision",
                collision_paths(entries, case_folded_path),
                "case-folded paths collide on the selected target",
            ),
            collision_diagnostic(
                "platform.unicode-normalization-collision",
                collision_paths(entries, normalized_path),
                "canonically equivalent Unicode paths collide on the selected target",
            ),
        ]
    } else {
        vec![
            diagnostic(
                "platform.case-collision",
                GitPlatformDiagnosticSeverity::Pass,
                Vec::new(),
                "the selected target supports native case-distinct paths",
            ),
            diagnostic(
                "platform.unicode-normalization-collision",
                GitPlatformDiagnosticSeverity::Pass,
                Vec::new(),
                "the selected target does not impose portable Unicode normalization",
            ),
        ]
    }
}

fn reserved_name_diagnostic(
    entries: &[GitTreeEntry],
    policy: &GitPlatformPolicy,
) -> GitPlatformDiagnostic {
    let paths = if matches!(
        policy.reserved_names,
        GitReservedNamesPolicy::WindowsRestricted | GitReservedNamesPolicy::WindowsPortable
    ) {
        matching_paths(entries, has_windows_restricted_component)
    } else {
        Vec::new()
    };
    if paths.is_empty() {
        diagnostic(
            "platform.reserved-name",
            GitPlatformDiagnosticSeverity::Pass,
            paths,
            "tree paths satisfy the selected target's reserved-name policy",
        )
    } else {
        diagnostic(
            "platform.reserved-name",
            GitPlatformDiagnosticSeverity::Error,
            paths,
            "tree paths contain Windows-reserved names or characters",
        )
    }
}

fn executable_bit_diagnostic(
    entries: &[GitTreeEntry],
    policy: &GitPlatformPolicy,
) -> GitPlatformDiagnostic {
    let paths = matching_paths(entries, |entry| entry.mode == "100755");
    let (severity, message) = if paths.is_empty() {
        (
            GitPlatformDiagnosticSeverity::Pass,
            "tree contains no executable file modes",
        )
    } else if policy.executable_bits == GitExecutableBitsPolicy::NotRepresentable {
        (
            GitPlatformDiagnosticSeverity::Warning,
            "executable file modes remain in Git but are not representable in this worktree",
        )
    } else {
        (
            GitPlatformDiagnosticSeverity::Info,
            "executable file modes rely on Git's target-filesystem probe",
        )
    };
    diagnostic("platform.executable-bit", severity, paths, message)
}

fn symlink_diagnostic(
    entries: &[GitTreeEntry],
    policy: &GitPlatformPolicy,
) -> GitPlatformDiagnostic {
    let paths = matching_paths(entries, |entry| entry.mode == "120000");
    let (severity, message) = if paths.is_empty() {
        (
            GitPlatformDiagnosticSeverity::Pass,
            "tree contains no symbolic links",
        )
    } else if policy.symlinks == GitSymlinkPolicy::LinkFiles {
        (
            GitPlatformDiagnosticSeverity::Warning,
            "symbolic links will materialize as ordinary link-content files",
        )
    } else {
        (
            GitPlatformDiagnosticSeverity::Info,
            "symbolic-link materialization relies on Git's target-filesystem probe",
        )
    };
    diagnostic("platform.symlink", severity, paths, message)
}

fn path_length_diagnostic(entries: &[GitTreeEntry]) -> GitPlatformDiagnostic {
    let paths = matching_paths(entries, |entry| {
        entry.path.len() >= LONG_PATH_WARNING_BYTES
            || entry
                .path
                .split('/')
                .any(|component| component.len() >= LONG_COMPONENT_WARNING_BYTES)
    });
    if paths.is_empty() {
        diagnostic(
            "platform.path-length",
            GitPlatformDiagnosticSeverity::Pass,
            paths,
            "tree has no paths near common target-filesystem limits",
        )
    } else {
        diagnostic(
            "platform.path-length",
            GitPlatformDiagnosticSeverity::Warning,
            paths,
            "path limits are filesystem-dependent; verify these long paths on the target device",
        )
    }
}

fn diagnostic(
    code: &str,
    severity: GitPlatformDiagnosticSeverity,
    paths: Vec<String>,
    message: impl Into<String>,
) -> GitPlatformDiagnostic {
    let count = paths.len();
    GitPlatformDiagnostic {
        code: code.to_string(),
        severity,
        count,
        paths: paths.into_iter().take(MAX_EXAMPLE_PATHS).collect(),
        message: message.into(),
    }
}

fn collision_diagnostic(code: &str, paths: Vec<String>, message: &str) -> GitPlatformDiagnostic {
    if paths.is_empty() {
        diagnostic(
            code,
            GitPlatformDiagnosticSeverity::Pass,
            paths,
            "tree paths are unique under the selected target's normalization",
        )
    } else {
        diagnostic(code, GitPlatformDiagnosticSeverity::Error, paths, message)
    }
}

fn matching_paths(
    entries: &[GitTreeEntry],
    predicate: impl Fn(&GitTreeEntry) -> bool,
) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| predicate(entry))
        .map(|entry| entry.path.clone())
        .collect()
}

fn collision_paths(entries: &[GitTreeEntry], key: impl Fn(&str) -> String) -> Vec<String> {
    let mut grouped = BTreeMap::<String, Vec<&str>>::new();
    for entry in entries {
        grouped
            .entry(key(&entry.path))
            .or_default()
            .push(&entry.path);
    }
    grouped
        .into_values()
        .filter(|paths| paths.len() > 1)
        .flatten()
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn normalized_path(path: &str) -> String {
    path.nfc().collect()
}

fn case_folded_path(path: &str) -> String {
    path.nfc().flat_map(char::to_lowercase).collect()
}

fn has_windows_restricted_component(entry: &GitTreeEntry) -> bool {
    entry.path.split('/').any(is_windows_restricted_component)
}

fn is_windows_restricted_component(component: &str) -> bool {
    if component.is_empty()
        || component.ends_with([' ', '.'])
        || component
            .chars()
            .any(|character| character < ' ' || r#"<>:"\|?*"#.contains(character))
    {
        return true;
    }
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GitPlatformProfile, GitTreeEntry};

    fn entry(path: &str, mode: &str) -> GitTreeEntry {
        GitTreeEntry {
            path: path.to_string(),
            oid: GitOid::parse("1111111111111111111111111111111111111111").expect("oid"),
            mode: mode.to_string(),
            kind: "blob".to_string(),
        }
    }

    fn inspect(entries: &[GitTreeEntry], profile: GitPlatformProfile) -> GitPlatformPreflight {
        inspect_git_tree_platform(
            GitOid::parse("2222222222222222222222222222222222222222").expect("revision"),
            entries,
            profile.policy(),
        )
    }

    #[test]
    fn windows_and_android_profiles_reject_aliasing_and_reserved_names() {
        let entries = vec![
            entry("Notes/Alpha.md", "100644"),
            entry("notes/alpha.md", "100644"),
            entry("CON.txt", "100644"),
        ];
        for profile in [
            GitPlatformProfile::WindowsNative,
            GitPlatformProfile::AndroidShared,
        ] {
            let report = inspect(&entries, profile);
            assert!(!report.compatible);
            assert_eq!(
                report
                    .diagnostics
                    .iter()
                    .find(|item| item.code == "platform.case-collision")
                    .expect("case diagnostic")
                    .count,
                2
            );
            assert_eq!(
                report
                    .diagnostics
                    .iter()
                    .find(|item| item.code == "platform.reserved-name")
                    .expect("reserved diagnostic")
                    .paths,
                vec!["CON.txt"]
            );
        }
    }

    #[test]
    fn normalization_collisions_are_deterministic_and_linux_native_keeps_them_distinct() {
        let entries = vec![
            entry("Cafe\u{301}.md", "100644"),
            entry("Caf\u{e9}.md", "100644"),
        ];
        let windows = inspect(&entries, GitPlatformProfile::WindowsNative);
        let collision = windows
            .diagnostics
            .iter()
            .find(|item| item.code == "platform.unicode-normalization-collision")
            .expect("normalization diagnostic");
        assert_eq!(collision.count, 2);
        assert_eq!(collision.paths, vec!["Cafe\u{301}.md", "Caf\u{e9}.md"]);

        let linux = inspect(&entries, GitPlatformProfile::LinuxNative);
        assert!(linux.compatible);
        assert_eq!(
            linux
                .diagnostics
                .iter()
                .find(|item| item.code == "platform.unicode-normalization-collision")
                .expect("normalization diagnostic")
                .severity,
            GitPlatformDiagnosticSeverity::Pass
        );
    }

    #[test]
    fn android_reports_lossy_modes_and_bounded_long_path_examples() {
        let mut entries = vec![entry("script.sh", "100755"), entry("linked-note", "120000")];
        entries
            .extend((0..25).map(|index| entry(&format!("{}-{index}", "x".repeat(245)), "100644")));
        let report = inspect(&entries, GitPlatformProfile::AndroidShared);
        assert!(report.compatible);
        for code in ["platform.executable-bit", "platform.symlink"] {
            assert_eq!(
                report
                    .diagnostics
                    .iter()
                    .find(|item| item.code == code)
                    .expect("mode diagnostic")
                    .severity,
                GitPlatformDiagnosticSeverity::Warning
            );
        }
        let long = report
            .diagnostics
            .iter()
            .find(|item| item.code == "platform.path-length")
            .expect("path length diagnostic");
        assert_eq!(long.count, 25);
        assert_eq!(long.paths.len(), MAX_EXAMPLE_PATHS);
    }
}
