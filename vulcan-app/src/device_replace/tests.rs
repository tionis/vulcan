use super::*;
use crate::device_config::{DeviceConfig, ForgeEntry, LoginMode};
use crate::sync_forge::{
    set_forge_config_with, AuthorizeAction, DeployKey, ForgeAuthorizeReport, ForgeConfig,
    ForgeKind, DEVICE_KEY_TITLE_PREFIX,
};
use crate::sync_registration::{list_registrations, RegistrationStatus};
use crate::vault_enroll::AuthorityError;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use tempfile::TempDir;

const URL: &str = "git@forge.example.com:eric/mimir.git";
const OTHER_REPO: &str = "eric/other";

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

/// The forge as the tests see it: which `(device, repo)` pairs are authorized
/// and which deploy keys each repo holds.
#[derive(Default)]
struct World {
    authorized: RefCell<BTreeSet<(String, String)>>,
    keys: RefCell<Vec<(String, DeployKey)>>,
    /// Repos whose authorization is refused (no credential).
    refuse: RefCell<BTreeSet<String>>,
    next_id: RefCell<u64>,
}

struct Authority<'a>(&'a World);

impl ForgeAuthority for Authority<'_> {
    fn authorize(
        &self,
        config: &ForgeConfig,
        device_id: &str,
        public_key: &str,
        _label: Option<&str>,
        _login_allowed: bool,
    ) -> Result<ForgeAuthorizeReport, AuthorityError> {
        if self.0.refuse.borrow().contains(&config.repo) {
            return Err(AuthorityError::NoCredential("no credential".to_owned()));
        }
        self.0
            .authorized
            .borrow_mut()
            .insert((device_id.to_owned(), config.repo.clone()));
        let mut next = self.0.next_id.borrow_mut();
        *next += 1;
        self.0.keys.borrow_mut().push((
            config.repo.clone(),
            DeployKey {
                id: *next,
                title: format!("{DEVICE_KEY_TITLE_PREFIX}{device_id}"),
                key: public_key.to_owned(),
                read_only: false,
            },
        ));
        Ok(ForgeAuthorizeReport {
            version: 1,
            repo: config.repo.clone(),
            device_id: device_id.to_owned(),
            fingerprint: "SHA256:test".to_owned(),
            action: AuthorizeAction::Added,
            dry_run: false,
        })
    }
}

struct RepoAdapter {
    world: Rc<World>,
    repo: String,
}

impl ForgeDeployKeyAdapter for RepoAdapter {
    fn list_deploy_keys(&self) -> Result<Vec<DeployKey>, AppError> {
        Ok(self
            .world
            .keys
            .borrow()
            .iter()
            .filter(|(repo, _)| *repo == self.repo)
            .map(|(_, key)| key.clone())
            .collect())
    }
    fn add_deploy_key(&self, _: &str, _: &str) -> Result<DeployKey, AppError> {
        unreachable!("revocation never adds keys")
    }
    fn remove_deploy_key(&self, id: u64) -> Result<(), AppError> {
        self.world.keys.borrow_mut().retain(|(_, key)| key.id != id);
        Ok(())
    }
}

struct Fixture {
    dir: TempDir,
    world: Rc<World>,
    state: SyncStateStore,
    identity: DeviceIdentityStore,
    exe: PathBuf,
    config: DeviceConfig,
    vaults: Vec<ReplaceVault>,
}

impl Fixture {
    /// Two vaults, bound to the active identity (which the forge accepts).
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let world = Rc::new(World::default());
        let identity = DeviceIdentityStore::at(dir.path().join("data/device"));
        identity.initialize(false).unwrap();
        let exe = dir.path().join("bin/vulcan");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let state = SyncStateStore::at(dir.path().join("state/sync/repositories"));
        let config = DeviceConfig {
            forges: vec![ForgeEntry {
                host: "forge.example.com".to_owned(),
                kind: Some(ForgeKind::Forgejo),
                oauth_client_id: Some("client-abc".to_owned()),
                token_env: None,
                transport: None,
                login: LoginMode::Auto,
            }],
            ..DeviceConfig::default()
        };
        let mut vaults = Vec::new();
        for name in ["mimir", "other"] {
            let bare = dir.path().join(format!("{name}.git"));
            let vault = dir.path().join(name);
            fs::create_dir_all(&vault).unwrap();
            git(
                dir.path(),
                &["init", "--bare", "-q", bare.to_str().unwrap()],
            );
            git(&vault, &["init", "-q"]);
            git(&vault, &["remote", "add", "origin", bare.to_str().unwrap()]);
            let paths = VaultPaths::new(&vault);
            if name == "other" {
                let forge = ForgeConfig::new(
                    ForgeKind::Forgejo,
                    "https://forge.example.com",
                    OTHER_REPO,
                    None,
                    Some("client-abc"),
                )
                .unwrap();
                set_forge_config_with(&paths, &state, &forge, false).unwrap();
            }
            vaults.push(ReplaceVault {
                wiki: name.to_owned(),
                paths,
                remote: GitRemote::parse("origin").unwrap(),
                permissions_profile: None,
            });
        }
        let fixture = Self {
            dir,
            world,
            state,
            identity,
            exe,
            config,
            vaults,
        };
        // Enroll the old key in both vaults, the way `vault add` would have.
        let old = fixture.identity.device_id().unwrap().unwrap();
        for vault in &fixture.vaults {
            fixture
                .world
                .authorized
                .borrow_mut()
                .insert((old.clone(), Fixture::repo_of(vault)));
            let id = 100 + fixture.world.keys.borrow().len() as u64;
            fixture.world.keys.borrow_mut().push((
                Fixture::repo_of(vault),
                DeployKey {
                    id,
                    title: format!("{DEVICE_KEY_TITLE_PREFIX}{old}"),
                    key: fixture.identity.public_key().unwrap(),
                    read_only: false,
                },
            ));
        }
        let report = fixture.replace_enroll_old();
        assert!(report, "both vaults start bound");
        fixture
    }

    fn repo_of(vault: &ReplaceVault) -> String {
        if vault.wiki == "other" {
            OTHER_REPO.to_owned()
        } else {
            "eric/mimir".to_owned()
        }
    }

    fn accepts(&self, store: &DeviceIdentityStore, dir: Option<&Path>) -> bool {
        let device = store.device_id().unwrap().unwrap();
        let Some(dir) = dir else { return false };
        let vault = self.vaults.iter().find(|v| v.paths.vault_root() == dir);
        vault.is_some_and(|vault| {
            self.world
                .authorized
                .borrow()
                .contains(&(device, Self::repo_of(vault)))
        })
    }

    fn replace_enroll_old(&self) -> bool {
        let probe = |target: &str, dir: Option<&Path>| {
            let _ = target;
            Ok(if self.accepts(&self.identity, dir) {
                ProbeOutcome::Accepted
            } else {
                ProbeOutcome::Denied("no".to_owned())
            })
        };
        let authority = Authority(&self.world);
        let env = EnrollEnvironment {
            device_config: &self.config,
            identity: &self.identity,
            state: &self.state,
            executable: &self.exe,
            probe: &probe,
            authority: &authority,
            sleep: &|_| {},
            remote_url_override: Some(URL),
        };
        self.vaults.iter().all(|vault| {
            enroll_vault(
                &vault.paths,
                &env,
                &request_for(
                    vault,
                    ReplaceRequest {
                        login: LoginPolicy::Never,
                        activate_anyway: false,
                        revoke_old: false,
                        dry_run: false,
                    },
                ),
            )
            .unwrap()
            .state
                == EnrollState::Bound
        })
    }

    fn run(&self, request: ReplaceRequest) -> ReplaceReport {
        let authority =
            |_: &ReplaceVault| Box::new(Authority(&self.world)) as Box<dyn ForgeAuthority + '_>;
        let probe = |store: &DeviceIdentityStore, _: &str, dir: Option<&Path>| {
            Ok(if self.accepts(store, dir) {
                ProbeOutcome::Accepted
            } else {
                ProbeOutcome::Denied("no".to_owned())
            })
        };
        let forge_access = |vault: &ReplaceVault| {
            Ok((
                Box::new(RepoAdapter {
                    world: Rc::clone(&self.world),
                    repo: Self::repo_of(vault),
                }) as Box<dyn ForgeDeployKeyAdapter>,
                Self::repo_of(vault),
            ))
        };
        let env = ReplaceEnvironment {
            device_config: &self.config,
            identity: &self.identity,
            state: &self.state,
            executable: &self.exe,
            probe: &probe,
            authority: &authority,
            sleep: &|_| {},
            forge_access: &forge_access,
            remote_url_override: Some(URL),
        };
        replace_device(&env, &self.vaults, request).unwrap()
    }

    fn bound_id(&self, vault: &ReplaceVault) -> Option<String> {
        transport_status_with_store(&vault.paths, &self.state, &self.identity)
            .unwrap()
            .bound_device_id
    }
}

fn request() -> ReplaceRequest {
    ReplaceRequest {
        login: LoginPolicy::Never,
        activate_anyway: false,
        revoke_old: false,
        dry_run: false,
    }
}

#[test]
fn replacing_proves_the_new_key_everywhere_then_swaps_and_rebinds() {
    let fx = Fixture::new();
    let old = fx.identity.device_id().unwrap().unwrap();

    let report = fx.run(request());
    assert!(report.activated);
    let new = report.new_device_id.clone().unwrap();
    assert_ne!(new, old);
    assert_eq!(fx.identity.device_id().unwrap(), Some(new.clone()));
    for (vault, entry) in fx.vaults.iter().zip(&report.vaults) {
        assert!(entry.affected);
        assert_eq!(entry.prepare.as_ref().unwrap().state, EnrollState::Ready);
        assert_eq!(entry.rebind.as_ref().unwrap().state, EnrollState::Bound);
        assert_eq!(fx.bound_id(vault), Some(new.clone()));
        assert!(entry.revoke_old.is_none(), "old device kept unless asked");
    }
    assert!(report.next_steps.iter().any(|step| step.contains(&old)));
    assert!(
        fx.identity.retired_directory(&old).unwrap().is_dir(),
        "old identity archived"
    );
}

#[test]
fn a_vault_that_cannot_be_proven_keeps_the_old_key_active() {
    let fx = Fixture::new();
    let old = fx.identity.device_id().unwrap().unwrap();
    fx.world.refuse.borrow_mut().insert(OTHER_REPO.to_owned());

    let report = fx.run(request());
    assert!(!report.activated);
    assert_eq!(fx.identity.device_id().unwrap(), Some(old.clone()));
    for vault in &fx.vaults {
        assert_eq!(fx.bound_id(vault), Some(old.clone()), "nothing was rebound");
    }
    assert_eq!(
        report.vaults[1].prepare.as_ref().unwrap().state,
        EnrollState::Pending
    );
    assert!(!report.next_steps.is_empty());

    // Once the forge cooperates, the same command finishes the job, reusing the staged key.
    fx.world.refuse.borrow_mut().clear();
    let staged = report.new_device_id.clone();
    let done = fx.run(request());
    assert!(done.activated);
    assert_eq!(done.new_device_id, staged);
}

#[test]
fn activating_anyway_leaves_the_unproven_vault_pending_and_protects_it_from_revocation() {
    let fx = Fixture::new();
    let old = fx.identity.device_id().unwrap().unwrap();
    fx.world.refuse.borrow_mut().insert(OTHER_REPO.to_owned());

    let report = fx.run(ReplaceRequest {
        activate_anyway: true,
        revoke_old: true,
        ..request()
    });
    assert!(report.activated);
    let (proven, pending) = (&report.vaults[0], &report.vaults[1]);
    assert_eq!(proven.rebind.as_ref().unwrap().state, EnrollState::Bound);
    assert_eq!(pending.rebind.as_ref().unwrap().state, EnrollState::Pending);

    let revoked = proven.revoke_old.as_ref().expect("revoked where proven");
    assert!(!revoked.incomplete());
    assert!(pending.revoke_old.is_none());
    assert!(pending.revoke_skipped.is_some(), "never strand a vault");

    let keys = fx.world.keys.borrow();
    let old_marker = format!("{DEVICE_KEY_TITLE_PREFIX}{old}");
    assert!(
        !keys
            .iter()
            .any(|(repo, key)| repo == "eric/mimir" && key.title == old_marker),
        "old key gone where the new one is proven"
    );
    drop(keys);
    let registrations =
        list_registrations(&fx.vaults[0].paths, &fx.vaults[0].remote, true).unwrap();
    assert!(registrations
        .registrations
        .iter()
        .any(|entry| entry.device_id == old && entry.status == RegistrationStatus::Revoked));
}

#[test]
fn a_dry_run_changes_nothing() {
    let fx = Fixture::new();
    let old = fx.identity.device_id().unwrap().unwrap();
    let report = fx.run(ReplaceRequest {
        dry_run: true,
        revoke_old: true,
        ..request()
    });
    assert!(!report.activated && report.dry_run);
    assert_eq!(fx.identity.device_id().unwrap(), Some(old.clone()));
    assert!(
        !fx.dir.path().join("data/device-staged").exists(),
        "no key was generated"
    );
    for vault in &fx.vaults {
        assert_eq!(fx.bound_id(vault), Some(old.clone()));
    }
    assert_eq!(
        fx.world
            .keys
            .borrow()
            .iter()
            .filter(|(_, key)| key.title.ends_with(&old))
            .count(),
        2,
        "old deploy keys untouched"
    );
}

#[test]
fn replacing_without_an_identity_points_at_device_init() {
    let dir = TempDir::new().unwrap();
    let identity = DeviceIdentityStore::at(dir.path().join("device"));
    let state = SyncStateStore::at(dir.path().join("state"));
    let config = DeviceConfig::default();
    let world = World::default();
    let authority = |_: &ReplaceVault| Box::new(Authority(&world)) as Box<dyn ForgeAuthority + '_>;
    let probe = |_: &DeviceIdentityStore, _: &str, _: Option<&Path>| Ok(ProbeOutcome::Accepted);
    let forge_access = |_: &ReplaceVault| Err("none".to_owned());
    let env = ReplaceEnvironment {
        device_config: &config,
        identity: &identity,
        state: &state,
        executable: Path::new("/bin/true"),
        probe: &probe,
        authority: &authority,
        sleep: &|_| {},
        forge_access: &forge_access,
        remote_url_override: None,
    };
    let error = replace_device(&env, &[], request()).unwrap_err();
    assert!(error.to_string().contains("device init"));
}
