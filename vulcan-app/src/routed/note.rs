//! Single-note reads a client may route: `note info`, the links and
//! backlinks of one note (`note links`/`links`, `note backlinks`/`backlinks`),
//! and a note's evaluated live fields (`numerals eval`, `metabind list`),
//! which a resident daemon answers without starting a JavaScript engine.

use crate::notes::{build_note_info_report, NoteInfoReport};
use crate::AppError;
use serde::{Deserialize, Serialize};
use vulcan_core::{
    query_backlinks_with_filter, query_links_with_filter, resolve_note_reference, BacklinksReport,
    OutgoingLinksReport, PermissionFilter, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "read", rename_all = "snake_case")]
pub enum NoteReadRequest {
    Info {
        note: String,
    },
    Links {
        note: String,
    },
    Backlinks {
        note: String,
    },
    /// `vulcan numerals eval` of a vault-relative note path.
    Numerals {
        path: String,
    },
    /// `vulcan metabind list` of a vault-relative note path.
    Metabind {
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "report", content = "value", rename_all = "snake_case")]
pub enum NoteReadAnswer {
    Info(NoteInfoReport),
    Links(OutgoingLinksReport),
    Backlinks(BacklinksReport),
    Numerals(crate::numerals::NumeralsReport),
    Metabind(crate::metabind::MetaBindReport),
}

/// Answers `request` from the vault's cache under `guard`, resolved from
/// `permission_profile`. `note info` checks that the note itself is readable
/// and reports it unfiltered, as the CLI always has; links and backlinks
/// apply the guard's read filter; live fields read the note when the guard
/// allows it and evaluate it under the profile, as the CLI does.
pub fn run_note_read(
    paths: &VaultPaths,
    request: &NoteReadRequest,
    guard: &ProfilePermissionGuard,
    permission_profile: Option<&str>,
) -> Result<NoteReadAnswer, AppError> {
    let read_source = |path: &str| -> Result<(vulcan_core::VaultConfig, String), AppError> {
        guard.check_read_path(path).map_err(AppError::operation)?;
        let source = std::fs::read_to_string(paths.vault_root().join(path))
            .map_err(|error| AppError::operation(format!("{path}: {error}")))?;
        Ok((vulcan_core::load_vault_config(paths).config, source))
    };
    match request {
        NoteReadRequest::Numerals { path } => {
            let (config, source) = read_source(path)?;
            crate::numerals::evaluate_numerals_note(
                paths,
                &config,
                path,
                &source,
                permission_profile,
            )
            .map(NoteReadAnswer::Numerals)
        }
        NoteReadRequest::Metabind { path } => {
            let (config, source) = read_source(path)?;
            crate::metabind::list_metabind_fields(paths, &config, path, &source, permission_profile)
                .map(NoteReadAnswer::Metabind)
        }
        NoteReadRequest::Info { note } => {
            if !guard.read_filter().path_permission().is_unrestricted() || guard.has_policy_hook() {
                let resolved = resolve_note_reference(paths, note).map_err(AppError::operation)?;
                guard
                    .check_read_path(&resolved.path)
                    .map_err(AppError::operation)?;
            }
            build_note_info_report(paths, note, None).map(NoteReadAnswer::Info)
        }
        NoteReadRequest::Links { note } => {
            query_links_with_filter(paths, note, restricting_filter(guard).as_ref())
                .map(NoteReadAnswer::Links)
                .map_err(AppError::operation)
        }
        NoteReadRequest::Backlinks { note } => {
            query_backlinks_with_filter(paths, note, restricting_filter(guard).as_ref())
                .map(NoteReadAnswer::Backlinks)
                .map_err(AppError::operation)
        }
    }
}

fn restricting_filter(guard: &ProfilePermissionGuard) -> Option<PermissionFilter> {
    let filter = guard.read_filter();
    (!filter.path_permission().is_unrestricted()).then_some(filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routed::test_support::copy_fixture;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode};

    #[test]
    fn note_reads_answer_like_the_direct_queries_and_survive_the_wire() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("vault");
        copy_fixture("basic", &root);
        let paths = VaultPaths::new(&root);
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let guard =
            ProfilePermissionGuard::new(&paths, resolve_permission_profile(&paths, None).unwrap());
        for note in ["Home", "Projects/Alpha"] {
            for request in [
                NoteReadRequest::Info {
                    note: note.to_string(),
                },
                NoteReadRequest::Links {
                    note: note.to_string(),
                },
                NoteReadRequest::Backlinks {
                    note: note.to_string(),
                },
            ] {
                let answer = run_note_read(&paths, &request, &guard, None).unwrap();
                let direct = match &request {
                    NoteReadRequest::Info { note } => {
                        NoteReadAnswer::Info(build_note_info_report(&paths, note, None).unwrap())
                    }
                    NoteReadRequest::Links { note } => {
                        NoteReadAnswer::Links(query_links_with_filter(&paths, note, None).unwrap())
                    }
                    NoteReadRequest::Backlinks { note } => NoteReadAnswer::Backlinks(
                        query_backlinks_with_filter(&paths, note, None).unwrap(),
                    ),
                    _ => unreachable!("only note reads above"),
                };
                assert_eq!(answer, direct, "{request:?}");
                let decoded: NoteReadAnswer =
                    serde_json::from_slice(&serde_json::to_vec(&answer).unwrap()).unwrap();
                assert_eq!(decoded, answer, "{request:?}");
                let request_decoded: NoteReadRequest =
                    serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
                assert_eq!(request_decoded, request);
            }
        }
        assert!(run_note_read(
            &paths,
            &NoteReadRequest::Info {
                note: "Missing".to_string()
            },
            &guard,
            None
        )
        .is_err());
    }

    #[test]
    fn live_field_reads_answer_like_the_cli_and_survive_the_wire() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("vault");
        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::write(
            root.join(".vulcan/config.toml"),
            "[numerals]\nenabled = true\n\n[permissions.profiles.other]\nread = { allow = [\"folder:Other/**\"] }\n",
        )
        .unwrap();
        let note =
            "---\nlevel: 2\n---\n`INPUT[slider(maxValue(5)):level]`\n\n```math\n2 + 3\n```\n";
        std::fs::write(root.join("Hero.md"), note).unwrap();
        let paths = VaultPaths::new(&root);
        scan_vault(&paths, ScanMode::Full).unwrap();
        let config = vulcan_core::load_vault_config(&paths).config;
        let guard =
            ProfilePermissionGuard::new(&paths, resolve_permission_profile(&paths, None).unwrap());
        let read = |request: NoteReadRequest| {
            let answer = run_note_read(&paths, &request, &guard, None).unwrap();
            let decoded: NoteReadAnswer =
                serde_json::from_slice(&serde_json::to_vec(&answer).unwrap()).unwrap();
            assert_eq!(
                serde_json::to_value(&decoded).unwrap(),
                serde_json::to_value(&answer).unwrap()
            );
            decoded
        };
        let NoteReadAnswer::Numerals(numerals) = read(NoteReadRequest::Numerals {
            path: "Hero.md".to_string(),
        }) else {
            panic!("numerals");
        };
        let direct =
            crate::numerals::evaluate_numerals_note(&paths, &config, "Hero.md", note, None)
                .unwrap();
        assert_eq!(numerals, direct);
        let NoteReadAnswer::Metabind(metabind) = read(NoteReadRequest::Metabind {
            path: "Hero.md".to_string(),
        }) else {
            panic!("metabind");
        };
        assert_eq!(
            serde_json::to_value(&metabind).unwrap(),
            serde_json::to_value(
                crate::metabind::list_metabind_fields(&paths, &config, "Hero.md", note, None)
                    .unwrap()
            )
            .unwrap()
        );
        assert_eq!(
            vulcan_core::metabind::field_markdown(&metabind.fields[0]).1,
            "2 / 5"
        );
        let restricted = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("other")).unwrap(),
        );
        assert!(run_note_read(
            &paths,
            &NoteReadRequest::Metabind {
                path: "Hero.md".to_string()
            },
            &restricted,
            Some("other"),
        )
        .is_err());
    }

    #[test]
    fn a_restricted_guard_hides_notes_it_cannot_read() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("vault");
        copy_fixture("basic", &root);
        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::write(
            root.join(".vulcan/config.toml"),
            "[permissions.profiles.projects]\nread = { allow = [\"folder:Projects/**\"] }\n",
        )
        .unwrap();
        let paths = VaultPaths::new(&root);
        scan_vault(&paths, ScanMode::Full).unwrap();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("projects")).unwrap(),
        );
        assert!(run_note_read(
            &paths,
            &NoteReadRequest::Info {
                note: "Home".to_string()
            },
            &guard,
            None
        )
        .is_err());
        let NoteReadAnswer::Backlinks(report) = run_note_read(
            &paths,
            &NoteReadRequest::Backlinks {
                note: "Projects/Alpha".to_string(),
            },
            &guard,
            None,
        )
        .unwrap() else {
            panic!("backlinks");
        };
        assert!(report
            .backlinks
            .iter()
            .all(|backlink| backlink.source_path.starts_with("Projects/")));
    }
}
