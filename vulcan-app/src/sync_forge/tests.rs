use super::init::{forge_init_with, ForgeInitRequest};
use super::shared::{publish_shared_forge, SharedForge};
use super::*;
use crate::device_identity::DeviceIdentityStore;
use crate::sync_registration::{
    register_placeholder, revoke_registration, RegistrationSource, RegistrationStatus,
};
use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use vulcan_sync::GitEngine;

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A fresh device identity: `(device_id, canonical public key)`.
fn identity(dir: &Path, name: &str) -> (String, String) {
    let store = DeviceIdentityStore::at(dir.join(name));
    store.initialize(false).unwrap();
    (
        store.device_id().unwrap().unwrap(),
        store.public_key().unwrap(),
    )
}

fn summary(
    (device_id, public_key): &(String, String),
    status: RegistrationStatus,
    label: Option<&str>,
) -> RegistrationSummary {
    RegistrationSummary {
        fingerprint: identity_from_public_key(public_key, false)
            .unwrap()
            .fingerprint,
        device_id: device_id.clone(),
        public_key: public_key.clone(),
        label: label.map(str::to_owned),
        status,
        created_at_unix: 1,
        claimed_at_unix: (status == RegistrationStatus::Registered).then_some(1),
        signed: false,
        current_device: false,
        revision: "0".repeat(40),
        source: RegistrationSource::Remote,
    }
}

fn key(id: u64, title: &str, public_key: &str, read_only: bool) -> DeployKey {
    DeployKey {
        id,
        title: title.to_owned(),
        key: public_key.to_owned(),
        read_only,
    }
}

fn marker(device_id: &str) -> String {
    format!("{DEVICE_KEY_TITLE_PREFIX}{device_id}")
}

fn actions(entries: &[ForgeSyncEntry]) -> Vec<(ForgeSyncAction, Option<&str>)> {
    entries
        .iter()
        .map(|entry| (entry.action, entry.detail.as_deref()))
        .collect()
}

#[test]
#[allow(clippy::many_single_char_names)] // Six labelled devices keep the table readable.
fn planner_covers_every_registration_and_key_combination() {
    let dir = TempDir::new().unwrap();
    let a = identity(dir.path(), "a");
    let b = identity(dir.path(), "b");
    let c = identity(dir.path(), "c");
    let d = identity(dir.path(), "d");
    let e = identity(dir.path(), "e");
    let f = identity(dir.path(), "f");
    let stranger = identity(dir.path(), "stranger");
    let registrations = vec![
        summary(&a, RegistrationStatus::Registered, Some("Laptop")),
        summary(&b, RegistrationStatus::Placeholder, None),
        summary(&c, RegistrationStatus::Registered, None),
        summary(&d, RegistrationStatus::Registered, None),
        summary(&e, RegistrationStatus::Revoked, None),
        summary(&f, RegistrationStatus::Revoked, None),
    ];
    let keys = vec![
        // c: managed and write-capable, with a comment the forge kept.
        key(1, &marker(&c.0), &format!("{} c@host", c.1), false),
        // d: managed but read-only.
        key(2, &marker(&d.0), &d.1, true),
        // e: revoked, managed.
        key(3, &marker(&e.0), &e.1, false),
        // f: revoked, but a hand-added key without the marker.
        key(4, "my laptop", &f.1, false),
        // An orphan: marker for a device that has no registration.
        key(5, &marker(&stranger.0), &stranger.1, false),
        // A foreign key unrelated to any registration.
        key(6, "ci runner", "ssh-ed25519 AAAAunrelated", false),
    ];
    let mut sorted = registrations;
    sorted.sort_by(|x, y| x.device_id.cmp(&y.device_id));
    let (entries, orphans, foreign) = plan_forge_sync(&sorted, &keys);
    let by_device = |id: &str| entries.iter().find(|entry| entry.device_id == id).unwrap();

    assert_eq!(by_device(&a.0).action, ForgeSyncAction::Add);
    assert_eq!(
        by_device(&a.0).title,
        format!("vulcan-device:{} Laptop", a.0)
    );
    assert_eq!(by_device(&b.0).action, ForgeSyncAction::Add);
    assert_eq!(by_device(&b.0).title, format!("vulcan-device:{}", b.0));
    assert_eq!(by_device(&c.0).action, ForgeSyncAction::Present);
    assert_eq!(by_device(&d.0).action, ForgeSyncAction::Replace);
    assert_eq!(by_device(&d.0).remove_key_ids, [2]);
    assert_eq!(by_device(&e.0).action, ForgeSyncAction::Remove);
    assert_eq!(by_device(&e.0).remove_key_ids, [3]);
    assert_eq!(by_device(&f.0).action, ForgeSyncAction::ForeignKeyKept);
    assert!(by_device(&f.0)
        .detail
        .as_deref()
        .unwrap()
        .contains("remove it at the forge"));

    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].key_id, 5);
    assert_eq!(orphans[0].device_id.as_deref(), Some(stranger.0.as_str()));
    // `my laptop` and `ci runner` are foreign; the managed keys are not.
    assert_eq!(foreign, 2);
    assert_eq!(actions(&entries).len(), 6);
}

#[test]
fn planner_treats_a_key_without_registration_as_untouchable_and_adds_nothing_for_absence() {
    let dir = TempDir::new().unwrap();
    let a = identity(dir.path(), "a");
    // No registrations at all: an empty list must never delete anything.
    let keys = vec![key(1, &marker(&a.0), &a.1, false)];
    let (entries, orphans, foreign) = plan_forge_sync(&[], &keys);
    assert!(entries.is_empty());
    assert_eq!(orphans.len(), 1, "reported, not removed");
    assert_eq!(foreign, 0);
}

#[test]
fn a_marked_key_with_a_different_key_than_its_registration_is_an_orphan_and_the_real_key_is_added()
{
    let dir = TempDir::new().unwrap();
    let a = identity(dir.path(), "a");
    let other = identity(dir.path(), "other");
    let keys = vec![key(1, &marker(&a.0), &other.1, false)];
    let (entries, orphans, _) =
        plan_forge_sync(&[summary(&a, RegistrationStatus::Registered, None)], &keys);
    assert_eq!(entries[0].action, ForgeSyncAction::Add);
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].key_id, 1);
}

#[derive(Default)]
struct FakeForge {
    keys: RefCell<Vec<DeployKey>>,
    next_id: RefCell<u64>,
    fail_add_for: RefCell<Vec<String>>,
    calls: RefCell<Vec<String>>,
}

impl FakeForge {
    fn seeded(keys: Vec<DeployKey>) -> Self {
        let next = keys.iter().map(|key| key.id).max().unwrap_or(0) + 1;
        Self {
            keys: RefCell::new(keys),
            next_id: RefCell::new(next),
            ..Self::default()
        }
    }

    fn mutations(&self) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .filter(|call| !call.starts_with("list"))
            .cloned()
            .collect()
    }
}

impl ForgeDeployKeyAdapter for FakeForge {
    fn list_deploy_keys(&self) -> Result<Vec<DeployKey>, AppError> {
        self.calls.borrow_mut().push("list".to_owned());
        Ok(self.keys.borrow().clone())
    }

    fn add_deploy_key(&self, public_key: &str, title: &str) -> Result<DeployKey, AppError> {
        self.calls.borrow_mut().push(format!("add {title}"));
        if self
            .fail_add_for
            .borrow()
            .iter()
            .any(|needle| title.contains(needle.as_str()))
        {
            return Err(AppError::operation("forge refused the key"));
        }
        let mut next = self.next_id.borrow_mut();
        let added = key(*next, title, public_key, false);
        *next += 1;
        self.keys.borrow_mut().push(added.clone());
        Ok(added)
    }

    fn remove_deploy_key(&self, id: u64) -> Result<(), AppError> {
        self.calls.borrow_mut().push(format!("remove {id}"));
        self.keys.borrow_mut().retain(|key| key.id != id);
        Ok(())
    }
}

#[test]
fn authorizing_a_device_is_idempotent_and_touches_only_its_own_key() {
    let dir = TempDir::new().unwrap();
    let laptop = identity(dir.path(), "laptop");
    let forge = FakeForge::seeded(vec![key(1, "ci runner", "ssh-ed25519 AAAAci", false)]);

    let preview =
        authorize_device(&forge, "o/r", &laptop.0, &laptop.1, Some("Laptop"), true).unwrap();
    assert_eq!(preview.action, AuthorizeAction::Added);
    assert!(preview.dry_run);
    assert!(forge.mutations().is_empty(), "a dry run adds nothing");

    let added =
        authorize_device(&forge, "o/r", &laptop.0, &laptop.1, Some("Laptop"), false).unwrap();
    assert_eq!(added.action, AuthorizeAction::Added);
    assert_eq!(forge.keys.borrow().len(), 2);
    let installed = forge.keys.borrow()[1].clone();
    assert_eq!(
        installed.title,
        format!("vulcan-device:{} Laptop", laptop.0)
    );
    assert!(!installed.read_only);
    assert!(
        forge.keys.borrow()[0].title == "ci runner",
        "the foreign key is untouched"
    );

    let again = authorize_device(&forge, "o/r", &laptop.0, &laptop.1, None, false).unwrap();
    assert_eq!(again.action, AuthorizeAction::AlreadyPresent);
    assert_eq!(forge.keys.borrow().len(), 2, "no duplicate");
}

#[test]
fn authorizing_replaces_only_a_managed_read_only_key_and_keeps_a_foreign_one() {
    let dir = TempDir::new().unwrap();
    let laptop = identity(dir.path(), "laptop");
    let managed = FakeForge::seeded(vec![key(1, &marker(&laptop.0), &laptop.1, true)]);
    let report = authorize_device(&managed, "o/r", &laptop.0, &laptop.1, None, false).unwrap();
    assert_eq!(report.action, AuthorizeAction::Replaced);
    assert_eq!(managed.mutations()[0], "remove 1", "the old key goes first");
    assert!(!managed.keys.borrow()[0].read_only);

    // A hand-added key with the same material is never modified.
    let foreign = FakeForge::seeded(vec![key(2, "my laptop", &laptop.1, true)]);
    let report = authorize_device(&foreign, "o/r", &laptop.0, &laptop.1, None, false).unwrap();
    assert_eq!(report.action, AuthorizeAction::AlreadyPresent);
    assert!(foreign.mutations().is_empty());
}

#[test]
fn authorizing_refuses_a_mismatched_id_and_a_hostile_label() {
    let dir = TempDir::new().unwrap();
    let laptop = identity(dir.path(), "laptop");
    let phone = identity(dir.path(), "phone");
    let forge = FakeForge::default();
    let error = authorize_device(&forge, "o/r", &phone.0, &laptop.1, None, false).unwrap_err();
    assert!(error.to_string().contains("does not match"), "{error}");
    assert!(authorize_device(
        &forge,
        "o/r",
        &laptop.0,
        &laptop.1,
        Some("evil\u{202e}"),
        false
    )
    .is_err());
    assert!(authorize_device(
        &forge,
        "o/r",
        &laptop.0,
        "ssh-ed25519 AAAA-not-a-key",
        None,
        false
    )
    .is_err());
    assert!(
        forge.calls.borrow().is_empty(),
        "nothing was sent to the forge"
    );
}

struct Repo {
    dir: TempDir,
    paths: VaultPaths,
    remote: GitRemote,
}

fn repo() -> Repo {
    let dir = TempDir::new().unwrap();
    let remote_path = dir.path().join("remote.git");
    let vault = dir.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    git(dir.path(), &["init", "--bare", "-q", "remote.git"]);
    git(&vault, &["init", "-q"]);
    git(
        &vault,
        &["remote", "add", "origin", remote_path.to_str().unwrap()],
    );
    Repo {
        paths: VaultPaths::new(&vault),
        remote: GitRemote::parse("origin").unwrap(),
        dir,
    }
}

impl Repo {
    fn register(&self, who: &(String, String), label: Option<&str>) {
        register_placeholder(&self.paths, &self.remote, &who.1, label, false).unwrap();
    }

    fn sync(&self, forge: &FakeForge, dry_run: bool) -> Result<ForgeSyncReport, AppError> {
        forge_sync(&self.paths, &self.remote, forge, "owner/vault", dry_run)
    }
}

#[test]
fn forge_sync_installs_registered_keys_and_revokes_idempotently() {
    let repo = repo();
    let laptop = identity(repo.dir.path(), "laptop");
    let phone = identity(repo.dir.path(), "phone");
    repo.register(&laptop, Some("Laptop"));
    repo.register(&phone, None);
    let forge = FakeForge::seeded(vec![key(1, "ci runner", "ssh-ed25519 AAAAci", false)]);

    let preview = repo.sync(&forge, true).unwrap();
    assert!(preview.dry_run);
    assert_eq!(preview.entries.len(), 2);
    assert!(preview
        .entries
        .iter()
        .all(|entry| entry.action == ForgeSyncAction::Add
            && entry.result == ForgeSyncResult::Planned));
    assert_eq!(preview.foreign_keys, 1);
    assert!(forge.mutations().is_empty(), "dry run mutates nothing");

    let applied = repo.sync(&forge, false).unwrap();
    assert_eq!((applied.applied, applied.failed), (2, 0));
    let titles = forge
        .keys
        .borrow()
        .iter()
        .map(|key| key.title.clone())
        .collect::<Vec<_>>();
    assert!(titles.contains(&format!("vulcan-device:{} Laptop", laptop.0)));
    assert!(titles.contains(&format!("vulcan-device:{}", phone.0)));
    assert!(
        titles.contains(&"ci runner".to_owned()),
        "foreign key untouched"
    );

    let calls_before = forge.mutations().len();
    let again = repo.sync(&forge, false).unwrap();
    assert!(again
        .entries
        .iter()
        .all(|entry| entry.action == ForgeSyncAction::Present
            && entry.result == ForgeSyncResult::Unchanged));
    assert_eq!(forge.mutations().len(), calls_before, "idempotent");

    revoke_registration(&repo.paths, &repo.remote, &phone.0, false).unwrap();
    let revoked = repo.sync(&forge, false).unwrap();
    assert_eq!(revoked.applied, 1);
    assert!(!forge
        .keys
        .borrow()
        .iter()
        .any(|key| key.title.starts_with(&marker(&phone.0))));
    assert!(forge
        .keys
        .borrow()
        .iter()
        .any(|key| key.title.contains(&laptop.0)));
    let converged = repo.sync(&forge, false).unwrap();
    assert_eq!(converged.applied, 0);
    assert!(converged
        .entries
        .iter()
        .any(|entry| entry.action == ForgeSyncAction::AlreadyAbsent));
}

fn marked(id: u64, who: &(String, String)) -> DeployKey {
    key(id, &marker(&who.0), &who.1, false)
}

#[test]
fn revoking_a_device_tombstones_it_and_removes_only_its_own_key() {
    use crate::device_revoke::{revoke_device_in_vault, ForgeAccess, RevokeStepStatus};
    let repo = repo();
    let laptop = identity(repo.dir.path(), "laptop");
    let phone = identity(repo.dir.path(), "phone");
    repo.register(&laptop, None);
    repo.register(&phone, None);
    let forge = FakeForge::seeded(vec![
        key(1, "ci runner", "ssh-ed25519 AAAAci", false),
        marked(2, &laptop),
        marked(3, &phone),
    ]);
    let access = ForgeAccess::Adapter {
        adapter: &forge,
        repo: "owner/vault",
    };
    let run = |dry_run| {
        revoke_device_in_vault(
            &repo.paths,
            "wiki",
            &repo.remote,
            &phone.0,
            &access,
            dry_run,
        )
    };

    let preview = run(true);
    assert_eq!(preview.registration.status, RevokeStepStatus::Done);
    assert_eq!(preview.forge.status, RevokeStepStatus::Done);
    assert!(forge.mutations().is_empty(), "dry run mutates nothing");

    let report = run(false);
    assert!(!report.incomplete());
    assert_eq!(report.forge_removed.as_ref().unwrap().removed, vec![3]);
    assert_eq!(forge.mutations(), vec!["remove 3".to_owned()]);
    let ids = forge
        .keys
        .borrow()
        .iter()
        .map(|key| key.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![1, 2], "foreign and other-device keys survive");

    let again = run(false);
    assert_eq!(again.registration.status, RevokeStepStatus::Already);
    assert_eq!(again.forge.status, RevokeStepStatus::Already);
    assert_eq!(forge.mutations().len(), 1, "idempotent");
}

#[test]
fn revoking_works_without_a_registration_or_without_forge_access() {
    use crate::device_revoke::{revoke_device_in_vault, ForgeAccess, RevokeStepStatus};
    let repo = repo();
    let phone = identity(repo.dir.path(), "phone");
    let laptop = identity(repo.dir.path(), "laptop");
    repo.register(&laptop, None);
    let forge = FakeForge::seeded(vec![marked(7, &phone)]);

    let no_registration = revoke_device_in_vault(
        &repo.paths,
        "wiki",
        &repo.remote,
        &phone.0,
        &ForgeAccess::Adapter {
            adapter: &forge,
            repo: "owner/vault",
        },
        false,
    );
    assert_eq!(
        no_registration.registration.status,
        RevokeStepStatus::NotRegistered
    );
    assert_eq!(no_registration.forge.status, RevokeStepStatus::Done);
    assert!(forge.keys.borrow().is_empty());

    let offline = revoke_device_in_vault(
        &repo.paths,
        "wiki",
        &repo.remote,
        &laptop.0,
        &ForgeAccess::Unavailable("no credential".to_owned()),
        false,
    );
    assert_eq!(offline.registration.status, RevokeStepStatus::Done);
    assert_eq!(offline.forge.status, RevokeStepStatus::Pending);
    assert!(offline.incomplete());
}

#[test]
fn a_forge_failure_is_reported_without_undoing_the_registration() {
    use crate::device_revoke::{revoke_device_in_vault, ForgeAccess, RevokeStepStatus};
    struct Broken;
    impl ForgeDeployKeyAdapter for Broken {
        fn list_deploy_keys(&self) -> Result<Vec<DeployKey>, AppError> {
            Err(AppError::operation("forge unreachable"))
        }
        fn add_deploy_key(&self, _: &str, _: &str) -> Result<DeployKey, AppError> {
            unreachable!("revocation never adds keys")
        }
        fn remove_deploy_key(&self, _: u64) -> Result<(), AppError> {
            unreachable!("nothing was listed")
        }
    }
    let repo = repo();
    let phone = identity(repo.dir.path(), "phone");
    repo.register(&phone, None);
    let report = revoke_device_in_vault(
        &repo.paths,
        "wiki",
        &repo.remote,
        &phone.0,
        &ForgeAccess::Adapter {
            adapter: &Broken,
            repo: "owner/vault",
        },
        false,
    );
    assert_eq!(report.registration.status, RevokeStepStatus::Done);
    assert_eq!(report.forge.status, RevokeStepStatus::Failed);
    assert!(report
        .forge
        .detail
        .as_deref()
        .unwrap()
        .contains("unreachable"));
}

#[test]
fn an_unreachable_remote_changes_nothing() {
    let repo = repo();
    let laptop = identity(repo.dir.path(), "laptop");
    repo.register(&laptop, None);
    let forge = FakeForge::seeded(vec![key(1, &marker(&laptop.0), &laptop.1, false)]);
    fs::remove_dir_all(repo.dir.path().join("remote.git")).unwrap();

    let error = repo.sync(&forge, false).unwrap_err();
    assert!(error.to_string().contains("no deploy keys were changed"));
    assert!(
        forge.calls.borrow().is_empty(),
        "the forge was not even asked"
    );
}

#[test]
fn an_empty_registration_list_never_deletes_managed_keys() {
    let repo = repo();
    let stranger = identity(repo.dir.path(), "stranger");
    let forge = FakeForge::seeded(vec![key(7, &marker(&stranger.0), &stranger.1, false)]);

    let report = repo.sync(&forge, false).unwrap();
    assert!(report.entries.is_empty());
    assert_eq!(report.orphans.len(), 1);
    assert!(forge.mutations().is_empty());
    assert_eq!(forge.keys.borrow().len(), 1);
}

#[test]
fn a_failure_on_one_key_is_reported_and_a_rerun_converges() {
    let repo = repo();
    let laptop = identity(repo.dir.path(), "laptop");
    let phone = identity(repo.dir.path(), "phone");
    repo.register(&laptop, None);
    repo.register(&phone, None);
    let forge = FakeForge::default();
    forge.fail_add_for.borrow_mut().push(phone.0.clone());

    let first = repo.sync(&forge, false).unwrap();
    assert_eq!((first.applied, first.failed), (1, 1));
    let failed = first
        .entries
        .iter()
        .find(|entry| entry.result == ForgeSyncResult::Failed)
        .unwrap();
    assert_eq!(failed.device_id, phone.0);
    assert!(failed.detail.as_deref().unwrap().contains("refused"));

    forge.fail_add_for.borrow_mut().clear();
    let second = repo.sync(&forge, false).unwrap();
    assert_eq!((second.applied, second.failed), (1, 0));
    assert_eq!(forge.keys.borrow().len(), 2);
}

#[test]
fn a_read_only_managed_key_is_replaced_with_a_write_key() {
    let repo = repo();
    let laptop = identity(repo.dir.path(), "laptop");
    repo.register(&laptop, None);
    let forge = FakeForge::seeded(vec![key(1, &marker(&laptop.0), &laptop.1, true)]);

    let report = repo.sync(&forge, false).unwrap();
    assert_eq!(report.entries[0].action, ForgeSyncAction::Replace);
    assert_eq!(report.applied, 1);
    let keys = forge.keys.borrow();
    assert_eq!(keys.len(), 1);
    assert!(!keys[0].read_only);
    // The old key must go first: the same public key cannot be added twice.
    assert_eq!(forge.mutations()[0], "remove 1");
}

#[test]
fn config_validation_protects_the_token_destination() {
    let ok = |url: &str, repo: &str, env: &str| {
        ForgeConfig::new(ForgeKind::Forgejo, url, repo, Some(env), None)
    };
    assert!(ok("https://git.example.com", "owner/vault", "FORGEJO_TOKEN").is_ok());
    assert!(ok("https://git.example.com/sub/path/", "o.w_n-er/va.ult", "T").is_ok());
    assert!(ok("http://localhost:3000", "o/r", "T").is_ok());
    assert!(ok("http://127.0.0.1:3000", "o/r", "T").is_ok());
    for bad_url in [
        "http://git.example.com",
        "ftp://git.example.com",
        "git.example.com",
        "https://user:pass@git.example.com",
        "https://git.example.com?x=1",
        "https://git.example.com#frag",
        "https://",
        "https://git.example.com/ a",
    ] {
        assert!(ok(bad_url, "o/r", "T").is_err(), "{bad_url}");
    }
    for bad_repo in [
        "owner", "owner/", "/repo", "a/b/c", "o/..", "o/r.git", "o/r r", "../r",
    ] {
        assert!(
            ok("https://g.example", bad_repo, "T").is_err(),
            "{bad_repo}"
        );
    }
    for bad_env in ["", "1TOKEN", "TOKEN NAME", "TOKEN=abc", "token$"] {
        assert!(
            ok("https://g.example", "o/r", bad_env).is_err(),
            "{bad_env}"
        );
    }
}

#[test]
fn config_roundtrips_in_device_local_state_and_never_in_the_work_tree() {
    let repo = repo();
    let state = SyncStateStore::at(repo.dir.path().join("state/sync/repositories"));
    assert!(load_config(&repo.paths, &state).unwrap().is_none());

    let config = ForgeConfig::new(
        ForgeKind::Forgejo,
        "https://git.example.com",
        "owner/vault",
        Some("FORGEJO_TOKEN"),
        None,
    )
    .unwrap();
    let preview = set_forge_config_with(&repo.paths, &state, &config, true).unwrap();
    assert!(preview.changed && preview.dry_run);
    assert!(load_config(&repo.paths, &state).unwrap().is_none());

    assert!(
        set_forge_config_with(&repo.paths, &state, &config, false)
            .unwrap()
            .changed
    );
    assert!(
        !set_forge_config_with(&repo.paths, &state, &config, false)
            .unwrap()
            .changed
    );
    assert_eq!(load_config(&repo.paths, &state).unwrap(), Some(config));

    let entries = fs::read_dir(repo.paths.vault_root())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries, [std::ffi::OsString::from(".git")]);
    let stored =
        fs::read_to_string(state.vault_local_dir(&repo.paths).join(FORGE_CONFIG_FILE)).unwrap();
    assert!(
        stored.contains("FORGEJO_TOKEN"),
        "only the variable name is stored"
    );

    assert!(
        clear_forge_config_with(&repo.paths, &state, true)
            .unwrap()
            .changed
    );
    assert!(load_config(&repo.paths, &state).unwrap().is_some());
    assert!(
        clear_forge_config_with(&repo.paths, &state, false)
            .unwrap()
            .changed
    );
    assert!(load_config(&repo.paths, &state).unwrap().is_none());
    assert!(
        !clear_forge_config_with(&repo.paths, &state, false)
            .unwrap()
            .changed
    );
}

#[test]
fn a_tampered_config_file_is_rejected_not_trusted() {
    let repo = repo();
    let state = SyncStateStore::at(repo.dir.path().join("state/sync/repositories"));
    state
        .write_vault_local(
            &repo.paths,
            FORGE_CONFIG_FILE,
            br#"{"version":1,"kind":"forgejo","url":"http://evil.example","repo":"o/r","token_env":"T"}"#,
        )
        .unwrap();
    assert!(load_config(&repo.paths, &state).is_err());
    state
        .write_vault_local(
            &repo.paths,
            FORGE_CONFIG_FILE,
            br#"{"version":1,"kind":"forgejo","url":"https://g.example","repo":"o/r","token_env":"T","token":"leak"}"#,
        )
        .unwrap();
    assert!(
        load_config(&repo.paths, &state).is_err(),
        "unknown fields are refused"
    );
}

const FORGE_REMOTE_URL: &str = "git@forge.example.com:eric/mimir.git";

fn init_request() -> ForgeInitRequest {
    ForgeInitRequest {
        kind: Some(ForgeKind::Forgejo),
        oauth_client_id: Some("c1ient-id-1234".to_owned()),
        ..ForgeInitRequest::default()
    }
}

/// A second administrator machine: its own vault clone and device-local state,
/// sharing only the Git remote.
fn second_machine(repo: &Repo) -> (VaultPaths, SyncStateStore) {
    let vault = repo.dir.path().join("vault-two");
    fs::create_dir_all(&vault).unwrap();
    git(&vault, &["init", "-q"]);
    git(
        &vault,
        &[
            "remote",
            "add",
            "origin",
            repo.dir.path().join("remote.git").to_str().unwrap(),
        ],
    );
    (
        VaultPaths::new(&vault),
        SyncStateStore::at(repo.dir.path().join("state-two/sync/repositories")),
    )
}

fn state_of(repo: &Repo) -> SyncStateStore {
    SyncStateStore::at(repo.dir.path().join("state/sync/repositories"))
}

fn init(
    paths: &VaultPaths,
    state: &SyncStateStore,
    remote: &GitRemote,
    request: &ForgeInitRequest,
) -> Result<init::ForgeInitReport, AppError> {
    forge_init_with(paths, state, remote, request, Some(FORGE_REMOTE_URL))
}

#[test]
fn init_derives_everything_but_the_kind_and_credential_from_the_remote() {
    let repo = repo();
    let state = state_of(&repo);

    let preview = init(
        &repo.paths,
        &state,
        &repo.remote,
        &ForgeInitRequest {
            dry_run: true,
            ..init_request()
        },
    )
    .unwrap();
    assert!(!preview.saved && preview.config.is_some());
    assert!(
        load_config(&repo.paths, &state).unwrap().is_none(),
        "a dry run saves nothing"
    );

    let report = init(&repo.paths, &state, &repo.remote, &init_request()).unwrap();
    assert!(report.saved);
    let config = load_config(&repo.paths, &state).unwrap().unwrap();
    assert_eq!(config.url, "https://forge.example.com");
    assert_eq!(config.repo, "eric/mimir");
    assert_eq!(config.oauth_client_id.as_deref(), Some("c1ient-id-1234"));
    assert_eq!(config.token_env, None);

    assert!(
        !init(&repo.paths, &state, &repo.remote, &init_request())
            .unwrap()
            .saved,
        "idempotent"
    );
}

#[test]
fn init_requires_some_credential_and_a_known_forge_shape() {
    let repo = repo();
    let state = state_of(&repo);
    let bare = ForgeInitRequest {
        kind: Some(ForgeKind::Forgejo),
        ..ForgeInitRequest::default()
    };
    let error = init(&repo.paths, &state, &repo.remote, &bare)
        .unwrap_err()
        .to_string();
    assert!(error.contains("--oauth-client-id"), "{error}");

    let error = forge_init_with(
        &repo.paths,
        &state,
        &repo.remote,
        &init_request(),
        Some("/srv/git/local.git"),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("--url"), "{error}");

    // Explicit settings work for a remote that implies no forge, once the
    // unverifiable host is confirmed.
    let explicit = ForgeInitRequest {
        url: Some("https://forge.example.com".to_owned()),
        repo: Some("eric/mimir".to_owned()),
        ..init_request()
    };
    let unconfirmed = forge_init_with(
        &repo.paths,
        &state,
        &repo.remote,
        &explicit,
        Some("/srv/git/local.git"),
    )
    .unwrap_err()
    .to_string();
    assert!(unconfirmed.contains("cannot be checked"), "{unconfirmed}");
    let confirmed = ForgeInitRequest {
        allow_other_host: true,
        ..explicit
    };
    let report = forge_init_with(
        &repo.paths,
        &state,
        &repo.remote,
        &confirmed,
        Some("/srv/git/local.git"),
    )
    .unwrap();
    assert!(report.saved && report.derived.is_none());
}

#[test]
fn the_api_url_must_stay_on_the_remotes_host_unless_allowed() {
    let repo = repo();
    let state = state_of(&repo);
    let other = ForgeInitRequest {
        url: Some("https://elsewhere.example.org".to_owned()),
        ..init_request()
    };
    let error = init(&repo.paths, &state, &repo.remote, &other)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not on the Git remote's host"), "{error}");
    assert!(load_config(&repo.paths, &state).unwrap().is_none());

    let allowed = ForgeInitRequest {
        allow_other_host: true,
        ..other
    };
    assert!(
        init(&repo.paths, &state, &repo.remote, &allowed)
            .unwrap()
            .saved
    );
    assert_eq!(
        load_config(&repo.paths, &state).unwrap().unwrap().url,
        "https://elsewhere.example.org"
    );
}

#[test]
fn published_settings_are_non_secret_and_other_machines_must_adopt_them() {
    let repo = repo();
    let state = state_of(&repo);
    init(
        &repo.paths,
        &state,
        &repo.remote,
        &ForgeInitRequest {
            publish: true,
            token_env: Some("MY_PRIVATE_TOKEN".to_owned()),
            ..init_request()
        },
    )
    .unwrap();

    // The shared descriptor carries kind and client ID only: no token variable,
    // no repository path, and no URL because it equals the derived one.
    let (two_paths, two_state) = second_machine(&repo);
    let preview = init(
        &two_paths,
        &two_state,
        &repo.remote,
        &ForgeInitRequest::default(),
    )
    .unwrap();
    let shared = preview.shared.as_ref().expect("shared settings found");
    assert_eq!(shared.settings.api_url, None);
    assert_eq!(
        shared.settings.oauth_client_id.as_deref(),
        Some("c1ient-id-1234")
    );
    let raw = serde_json::to_string(&shared.settings).unwrap();
    assert!(
        !raw.contains("MY_PRIVATE_TOKEN") && !raw.contains("eric/mimir"),
        "{raw}"
    );
    assert!(!preview.saved && preview.config.is_none());
    assert!(preview.note.as_deref().unwrap().contains("--adopt"));
    assert!(
        load_config(&two_paths, &two_state).unwrap().is_none(),
        "never adopted implicitly"
    );

    let adopted = init(
        &two_paths,
        &two_state,
        &repo.remote,
        &ForgeInitRequest {
            adopt: true,
            token_env: Some("OTHER_TOKEN".to_owned()),
            ..ForgeInitRequest::default()
        },
    )
    .unwrap();
    assert!(adopted.saved);
    let config = load_config(&two_paths, &two_state).unwrap().unwrap();
    assert_eq!(
        (config.url.as_str(), config.repo.as_str()),
        ("https://forge.example.com", "eric/mimir")
    );
    assert_eq!(config.oauth_client_id.as_deref(), Some("c1ient-id-1234"));
    assert_eq!(
        config.token_env.as_deref(),
        Some("OTHER_TOKEN"),
        "device-specific values stay local"
    );

    // Republishing identical settings changes nothing.
    let again = init(
        &repo.paths,
        &state,
        &repo.remote,
        &ForgeInitRequest {
            publish: true,
            ..init_request()
        },
    )
    .unwrap();
    assert!(!again.published);
}

#[test]
fn a_hostile_shared_descriptor_cannot_redirect_the_credential() {
    let repo = repo();
    publish_shared_forge(
        &repo.paths,
        &repo.remote,
        &SharedForge::new(
            ForgeKind::Forgejo,
            Some("https://evil.example.net".to_owned()),
            Some("attacker-app".to_owned()),
        ),
    )
    .unwrap();
    let (paths, state) = second_machine(&repo);

    let error = init(
        &paths,
        &state,
        &repo.remote,
        &ForgeInitRequest {
            adopt: true,
            ..ForgeInitRequest::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("not on the Git remote's host"), "{error}");
    assert!(
        load_config(&paths, &state).unwrap().is_none(),
        "nothing was saved"
    );
}

#[test]
fn init_without_a_kind_needs_published_settings_and_rejects_malformed_ones() {
    let repo = repo();
    let state = state_of(&repo);
    let error = init(
        &repo.paths,
        &state,
        &repo.remote,
        &ForgeInitRequest::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("no shared forge settings"), "{error}");

    // A malformed descriptor is an error to adopt, but publishing repairs it.
    let (engine, repository) = {
        let engine = crate::sync_transport::git_engine(&repo.paths);
        let repository = engine
            .discover_repository(&fs::canonicalize(repo.paths.vault_root()).unwrap())
            .unwrap();
        (engine, repository)
    };
    let blob = engine
        .write_blob(
            &repository,
            b"{\"version\":1,\"kind\":\"forgejo\",\"extra\":true}",
        )
        .unwrap();
    let tree = engine
        .create_single_file_tree(&repository, "forge.json", &blob)
        .unwrap();
    let commit = engine
        .create_commit(&repository, &tree, &[], "x\n")
        .unwrap();
    engine
        .push_ref(
            &repository,
            &repo.remote,
            &commit,
            &vulcan_sync::GitRefName::parse(vulcan_sync::REMOTE_FORGE_DESCRIPTOR_REF).unwrap(),
            None,
        )
        .unwrap();
    assert!(init(
        &repo.paths,
        &state,
        &repo.remote,
        &ForgeInitRequest {
            adopt: true,
            ..ForgeInitRequest::default()
        }
    )
    .is_err());

    let repaired = init(
        &repo.paths,
        &state,
        &repo.remote,
        &ForgeInitRequest {
            publish: true,
            ..init_request()
        },
    )
    .unwrap();
    assert!(repaired.published);
    let (paths, other_state) = second_machine(&repo);
    assert!(init(
        &paths,
        &other_state,
        &repo.remote,
        &ForgeInitRequest::default()
    )
    .unwrap()
    .shared
    .is_some());
}

#[test]
fn shared_settings_parsing_is_strict() {
    let good = br#"{"version":1,"kind":"forgejo","oauth_client_id":"abc-123"}"#;
    assert!(SharedForge::parse(good).is_ok());
    for bad in [
        &br#"{"version":2,"kind":"forgejo"}"#[..],
        br#"{"version":1,"kind":"gitlab"}"#,
        br#"{"version":1,"kind":"forgejo","token":"leak"}"#,
        br#"{"version":1,"kind":"forgejo","oauth_client_id":"bad id"}"#,
        br#"{"version":1,"kind":"forgejo","oauth_client_id":""}"#,
        br#"{"version":1,"kind":"forgejo","api_url":"http://insecure.example"}"#,
        br#"{"version":1,"kind":"forgejo","api_url":"https://u:p@h.example"}"#,
        b"not json",
    ] {
        assert!(
            SharedForge::parse(bad).is_err(),
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
    assert!(SharedForge::parse(&vec![b' '; 4096]).is_err());
}

#[cfg(feature = "web")]
mod http {
    use super::*;
    use crate::sync_forge::ForgejoDeployKeys;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;

    #[derive(Default)]
    struct ServerState {
        keys: Vec<(u64, String, String, bool)>,
        next_id: u64,
        requests: Vec<String>,
        forced: Option<(u16, String)>,
        redirect_to: Option<String>,
    }

    struct FakeForgejo {
        url: String,
        state: Arc<Mutex<ServerState>>,
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl Drop for FakeForgejo {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn query_value(query: &str, name: &str) -> Option<usize> {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{name}="))?.parse().ok())
    }

    fn handle(state: &Arc<Mutex<ServerState>>, request: &str, body: &str) -> (u16, String) {
        let mut state = state.lock().unwrap();
        let mut lines = request.lines();
        let first = lines.next().unwrap_or_default().to_owned();
        let auth = request
            .lines()
            .find_map(|line| {
                line.strip_prefix("authorization: ")
                    .or_else(|| line.strip_prefix("Authorization: "))
            })
            .unwrap_or("none")
            .to_owned();
        state.requests.push(format!("{first} | {auth} | {body}"));
        if let Some(target) = state.redirect_to.clone() {
            return (302, target);
        }
        if let Some((status, body)) = state.forced.clone() {
            return (status, body);
        }
        let mut parts = first.split_whitespace();
        let method = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let Some(rest) = path.strip_prefix("/api/v1/repos/owner/vault/keys") else {
            return (404, r#"{"message":"not found"}"#.to_owned());
        };
        let render = |entry: &(u64, String, String, bool)| serde_json::json!({"id": entry.0, "key": entry.1, "title": entry.2, "read_only": entry.3});
        match (method, rest) {
            ("GET", "") => {
                let page = query_value(query, "page").unwrap_or(1);
                let limit = query_value(query, "limit").unwrap_or(30);
                let slice = state
                    .keys
                    .iter()
                    .skip((page - 1) * limit)
                    .take(limit)
                    .map(render)
                    .collect::<Vec<_>>();
                (200, serde_json::to_string(&slice).unwrap())
            }
            ("POST", "") => {
                let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
                let key = parsed["key"].as_str().unwrap_or_default().to_owned();
                if state.keys.iter().any(|entry| entry.1 == key) {
                    return (
                        422,
                        r#"{"message":"Key content has been used as a deploy key"}"#.to_owned(),
                    );
                }
                state.next_id += 1;
                let entry = (
                    state.next_id,
                    key,
                    parsed["title"].as_str().unwrap_or_default().to_owned(),
                    parsed["read_only"].as_bool().unwrap_or(true),
                );
                let rendered = render(&entry).to_string();
                state.keys.push(entry);
                (201, rendered)
            }
            ("DELETE", id) => {
                let id: u64 = id.trim_start_matches('/').parse().unwrap_or(0);
                let before = state.keys.len();
                state.keys.retain(|entry| entry.0 != id);
                if state.keys.len() == before {
                    (404, r#"{"message":"not found"}"#.to_owned())
                } else {
                    (204, String::new())
                }
            }
            _ => (405, r#"{"message":"method not allowed"}"#.to_owned()),
        }
    }

    fn serve() -> FakeForgejo {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(ServerState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_state, thread_stop) = (Arc::clone(&state), Arc::clone(&stop));
        let handle = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut buffer = Vec::new();
                let mut chunk = [0_u8; 4096];
                let (head, body) = loop {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    buffer.extend_from_slice(&chunk[..read]);
                    let text = String::from_utf8_lossy(&buffer).into_owned();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let head = text[..end].to_owned();
                        let length = head
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if buffer.len() >= end + 4 + length || read == 0 {
                            let body = text[end + 4..].to_owned();
                            break (head, body);
                        }
                    } else if read == 0 {
                        break (text, String::new());
                    }
                };
                let (status, payload) = handle(&thread_state, &head, &body);
                let response = if status == 302 {
                    format!("HTTP/1.1 302 Found\r\nLocation: {payload}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                } else {
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    )
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        FakeForgejo {
            url,
            state,
            stop,
            handle: Some(handle),
        }
    }

    fn client(server: &FakeForgejo) -> ForgejoDeployKeys {
        let config = ForgeConfig::new(
            ForgeKind::Forgejo,
            &server.url,
            "owner/vault",
            Some("TOKEN_ENV"),
            None,
        )
        .unwrap();
        ForgejoDeployKeys::new(&config, "s3cret", Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn lists_every_page_and_authenticates_with_the_token() {
        let server = serve();
        {
            let mut state = server.state.lock().unwrap();
            for id in 1..=120_u64 {
                state
                    .keys
                    .push((id, format!("ssh-ed25519 K{id}"), format!("k{id}"), false));
            }
            state.next_id = 120;
        }
        let keys = client(&server).list_deploy_keys().unwrap();
        assert_eq!(keys.len(), 120);
        assert_eq!(keys[119].title, "k120");
        let state = server.state.lock().unwrap();
        assert_eq!(state.requests.len(), 3, "50 + 50 + 20 across three pages");
        assert!(state
            .requests
            .iter()
            .all(|request| request.contains("token s3cret")));
        assert!(state.requests[0].contains("page=1&limit=50"));
    }

    #[test]
    fn adds_write_keys_and_removes_by_id() {
        let server = serve();
        let adapter = client(&server);
        let added = adapter
            .add_deploy_key("ssh-ed25519 AAAAkey", "vulcan-device:vdev1_x")
            .unwrap();
        assert!(!added.read_only, "write access is always requested");
        assert_eq!(added.title, "vulcan-device:vdev1_x");
        assert!(server.state.lock().unwrap().requests[0].contains("\"read_only\":false"));

        adapter.remove_deploy_key(added.id).unwrap();
        assert!(server.state.lock().unwrap().keys.is_empty());
        adapter.remove_deploy_key(added.id).unwrap(); // already gone is fine
    }

    #[test]
    fn forge_errors_carry_the_forge_message_and_a_hint() {
        let server = serve();
        let adapter = client(&server);
        adapter.add_deploy_key("ssh-ed25519 AAAAdup", "t").unwrap();
        let error = adapter
            .add_deploy_key("ssh-ed25519 AAAAdup", "t2")
            .unwrap_err()
            .to_string();
        assert!(error.contains("422"), "{error}");
        assert!(error.contains("used as a deploy key"), "{error}");

        server.state.lock().unwrap().forced = Some((401, r#"{"message":"bad\ntoken"}"#.to_owned()));
        let error = adapter.list_deploy_keys().unwrap_err().to_string();
        assert!(error.contains("check the API token"), "{error}");
        assert!(!error.contains('\n'), "control characters are stripped");
        assert!(
            !error.contains("s3cret"),
            "the token never appears in errors"
        );

        server.state.lock().unwrap().forced = Some((403, "not json at all".to_owned()));
        assert!(adapter
            .list_deploy_keys()
            .unwrap_err()
            .to_string()
            .contains("write access"));
    }

    #[test]
    fn redirects_are_never_followed_so_the_token_stays_on_the_configured_host() {
        let elsewhere = serve();
        let server = serve();
        server.state.lock().unwrap().redirect_to =
            Some(format!("{}/api/v1/repos/owner/vault/keys", elsewhere.url));
        let error = client(&server).list_deploy_keys().unwrap_err().to_string();
        assert!(error.contains("302"), "{error}");
        assert!(
            elsewhere.state.lock().unwrap().requests.is_empty(),
            "the redirect target was never contacted"
        );
    }

    #[test]
    fn an_empty_token_is_refused_before_any_request() {
        let server = serve();
        let config = ForgeConfig::new(
            ForgeKind::Forgejo,
            &server.url,
            "owner/vault",
            Some("TOKEN_ENV"),
            None,
        )
        .unwrap();
        assert!(ForgejoDeployKeys::new(&config, "  ", Duration::from_secs(1)).is_err());
        assert!(server.state.lock().unwrap().requests.is_empty());
    }

    #[test]
    fn end_to_end_against_a_stateful_forge() {
        let server = serve();
        let repo = repo();
        let laptop = identity(repo.dir.path(), "laptop");
        repo.register(&laptop, Some("Laptop"));
        let adapter = client(&server);

        let report = forge_sync(&repo.paths, &repo.remote, &adapter, "owner/vault", false).unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(server.state.lock().unwrap().keys.len(), 1);

        revoke_registration(&repo.paths, &repo.remote, &laptop.0, false).unwrap();
        let report = forge_sync(&repo.paths, &repo.remote, &adapter, "owner/vault", false).unwrap();
        assert_eq!(report.entries[0].action, ForgeSyncAction::Remove);
        assert!(server.state.lock().unwrap().keys.is_empty());
    }
}
