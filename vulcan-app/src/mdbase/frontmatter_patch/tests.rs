use super::*;
use crate::mdbase::tests::fixture;
use crate::mdbase::MdbaseQuerySession;
use std::fs;
use std::time::Duration;

const PUBLIC: &str = "---\n# kept comment\ntype: task\ntitle: Public\nextra: 'quoted'\n---\nBody line\n\n- keep [[links]]\n";

fn pilot() -> (tempfile::TempDir, VaultPaths) {
    let (directory, paths) = fixture();
    fs::write(directory.path().join("tasks/public.md"), PUBLIC).unwrap();
    vulcan_core::initialize_vulcan_dir(&paths).unwrap();
    (directory, paths)
}

fn request(revision: &str) -> MdbaseFrontmatterPatchRequest {
    MdbaseFrontmatterPatchRequest {
        path: "tasks/public.md".to_string(),
        if_revision: revision.to_string(),
        set: BTreeMap::from([("title".to_string(), serde_json::json!("Patched"))]),
        unset: vec!["extra".to_string()],
        permission_profile: None,
    }
}

fn options() -> MdbaseFrontmatterPatchOptions {
    MdbaseFrontmatterPatchOptions {
        no_commit: true,
        verbosity: Verbosity::Quiet,
        ..MdbaseFrontmatterPatchOptions::default()
    }
}

#[test]
fn patches_only_named_fields_and_the_next_watched_read_sees_them() {
    let (directory, paths) = pilot();
    let monitor = vulcan_core::mdbase::MdbaseChangeMonitor::watch(paths.vault_root()).unwrap();
    let session = MdbaseQuerySession::new(paths.clone())
        .with_change_monitor(monitor, Duration::from_secs(30));
    let before = session.read_metadata("tasks/public.md", None).unwrap();
    assert_eq!(before.record.revision, mdbase_content_revision(PUBLIC));

    let report =
        patch_mdbase_frontmatter(&paths, &request(&before.record.revision), &options()).unwrap();
    assert!(report.changed);
    let persisted = fs::read_to_string(directory.path().join("tasks/public.md")).unwrap();
    assert_eq!(report.revision, mdbase_content_revision(&persisted));
    assert!(persisted.contains("# kept comment"), "{persisted}");
    assert!(persisted.contains("title: Patched"), "{persisted}");
    assert!(!persisted.contains("extra"), "{persisted}");
    assert!(
        persisted.ends_with("---\nBody line\n\n- keep [[links]]\n"),
        "{persisted}"
    );
    // The read default (`status: open`) stays effective, never persisted.
    assert!(!persisted.contains("status"), "{persisted}");

    // Read-after-write: the retained, watched session shows the new revision.
    let after = session.read_metadata("tasks/public.md", None).unwrap();
    assert_eq!(after.record.revision, report.revision);
    assert_eq!(after.record.frontmatter["title"], "Patched");
    assert_eq!(after.record.effective_frontmatter["status"], "open");

    // Re-applying the same patch at the new revision changes nothing.
    let again = patch_mdbase_frontmatter(&paths, &request(&report.revision), &options()).unwrap();
    assert!(!again.changed);
    assert_eq!(again.revision, report.revision);
}

#[test]
fn stale_revisions_invalid_values_and_bad_requests_change_nothing() {
    let (directory, paths) = pilot();
    let read = || fs::read_to_string(directory.path().join("tasks/public.md")).unwrap();
    let stale = patch_mdbase_frontmatter(&paths, &request("sha256:stale"), &options()).unwrap_err();
    assert_eq!(stale.code(), Some("concurrent_modification"));
    assert_eq!(read(), PUBLIC);

    let current = mdbase_content_revision(PUBLIC);
    let mut invalid = request(&current);
    invalid.set = BTreeMap::from([("title".to_string(), serde_json::json!(42))]);
    invalid.unset.clear();
    assert!(patch_mdbase_frontmatter(&paths, &invalid, &options()).is_err());
    assert_eq!(read(), PUBLIC);

    // `title` is required in persisted frontmatter.
    let mut required = request(&current);
    required.set.clear();
    required.unset = vec!["title".to_string()];
    assert!(patch_mdbase_frontmatter(&paths, &required, &options()).is_err());
    assert_eq!(read(), PUBLIC);

    let mut empty = request(&current);
    empty.set.clear();
    empty.unset.clear();
    assert_eq!(
        patch_mdbase_frontmatter(&paths, &empty, &options())
            .unwrap_err()
            .code(),
        Some("invalid_request")
    );
    let mut both = request(&current);
    both.unset = vec!["title".to_string()];
    assert_eq!(
        patch_mdbase_frontmatter(&paths, &both, &options())
            .unwrap_err()
            .code(),
        Some("invalid_request")
    );

    let dry = patch_mdbase_frontmatter(
        &paths,
        &request(&current),
        &MdbaseFrontmatterPatchOptions {
            dry_run: true,
            ..options()
        },
    )
    .unwrap();
    assert!(dry.dry_run && dry.changed);
    assert_ne!(dry.revision, current);
    assert_eq!(read(), PUBLIC);
}

#[test]
fn unreadable_or_ungoverned_paths_look_missing_and_writes_need_write_authority() {
    let (directory, paths) = pilot();
    let config = directory.path().join(".vulcan/config.toml");
    let existing = fs::read_to_string(&config).unwrap_or_default();
    let profile = |name: &str, write: &str| {
        format!(
            "\n[permissions.profiles.{name}]\nread = {{ allow = [\"note:mdbase.yaml\", \"note:mdbase.lock.yaml\", \"folder:_types/**\", \"folder:_contracts/**\", \"folder:tasks/public.md\", \"note:tasks/public.md\"] }}\nwrite = {write}\nrefactor = \"none\"\ngit = \"deny\"\nnetwork = \"deny\"\nindex = \"deny\"\nconfig = \"read\"\nexecute = \"deny\"\nshell = \"deny\"\n"
        )
    };
    fs::write(
        &config,
        format!(
            "{existing}{}{}",
            profile("reader", "\"none\""),
            profile("writer", "{ allow = [\"note:tasks/public.md\"] }")
        ),
    )
    .unwrap();
    let current = mdbase_content_revision(PUBLIC);
    let as_profile = |name: &str, path: &str| {
        let mut request = request(&current);
        request.path = path.to_string();
        request.permission_profile = Some(name.to_string());
        patch_mdbase_frontmatter(&paths, &request, &options())
    };
    for path in ["tasks/private/secret.md", "tasks/absent.md"] {
        assert_eq!(
            as_profile("writer", path).unwrap_err().code(),
            Some("record_not_found"),
            "{path}"
        );
    }
    assert!(as_profile("reader", "tasks/public.md").is_err());
    assert_eq!(
        fs::read_to_string(directory.path().join("tasks/public.md")).unwrap(),
        PUBLIC
    );
    assert!(as_profile("writer", "tasks/public.md").unwrap().changed);
}

#[test]
fn an_interrupted_write_blocks_patches_until_recovered() {
    let (directory, paths) = pilot();
    let interrupted = PUBLIC.replace("title: Public", "title: Interrupted");
    super::super::write_repair::interrupt_update_for_gate(&paths, "tasks/public.md", &interrupted)
        .unwrap();
    // The committed bytes are on disk, but reconciliation is pending.
    let on_disk = fs::read_to_string(directory.path().join("tasks/public.md")).unwrap();
    assert_eq!(on_disk, interrupted);
    assert!(patch_mdbase_frontmatter(
        &paths,
        &request(&mdbase_content_revision(&on_disk)),
        &options()
    )
    .is_err());
    crate::mdbase::recover_mdbase_write(&paths, None, false).unwrap();
    let report = patch_mdbase_frontmatter(
        &paths,
        &request(&mdbase_content_revision(&on_disk)),
        &options(),
    )
    .unwrap();
    assert!(report.changed);
    let persisted = fs::read_to_string(directory.path().join("tasks/public.md")).unwrap();
    assert!(persisted.contains("title: Patched"), "{persisted}");
}
