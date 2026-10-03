use super::*;
use crate::device_config::{ForgeEntry, LoginMode};
use crate::sync_forge::ForgeKind;
use std::cell::RefCell;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

const URL: &str = "git@forge.example.com:eric/mimir.git";

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .expect("git");
    assert!(output.status.success(), "git {args:?}");
}

struct Fixture {
    dir: TempDir,
    state: SyncStateStore,
    identity: DeviceIdentityStore,
    exe: std::path::PathBuf,
    vaults: Vec<BindVault>,
}

impl Fixture {
    fn new(names: &[&str]) -> Self {
        let dir = TempDir::new().unwrap();
        let identity = DeviceIdentityStore::at(dir.path().join("device"));
        identity.initialize(false).unwrap();
        let exe = dir.path().join("bin/vulcan");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let vaults = names
            .iter()
            .map(|name| {
                let vault = dir.path().join(name);
                fs::create_dir_all(&vault).unwrap();
                git(&vault, &["init", "-q"]);
                git(&vault, &["remote", "add", "origin", URL]);
                BindVault {
                    wiki: (*name).to_owned(),
                    paths: VaultPaths::new(&vault),
                    blocked: None,
                }
            })
            .collect();
        Self {
            state: SyncStateStore::at(dir.path().join("state/sync/repositories")),
            identity,
            exe,
            vaults,
            dir,
        }
    }

    fn run(
        &self,
        config: &DeviceConfig,
        outcomes: &[(&str, ProbeOutcome)],
        url: Option<&str>,
        dry_run: bool,
    ) -> (BindAllReport, Vec<String>) {
        let probed = RefCell::new(Vec::new());
        let probe = |_: &str, dir: &Path| {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            probed.borrow_mut().push(name.clone());
            Ok(outcomes
                .iter()
                .find(|(wiki, _)| *wiki == name)
                .map_or(ProbeOutcome::Accepted, |(_, outcome)| outcome.clone()))
        };
        let env = BindAllEnvironment {
            device_config: config,
            identity: &self.identity,
            state: &self.state,
            executable: &self.exe,
            probe: &probe,
            remote_url_override: url,
        };
        let report = bind_all(
            &env,
            &self.vaults,
            &GitRemote::parse("origin").unwrap(),
            GitConfigMode::Auto,
            dry_run,
        )
        .unwrap();
        (report, probed.into_inner())
    }

    fn ssh_command(&self, wiki: &str) -> String {
        let output = Command::new("git")
            .current_dir(self.dir.path().join(wiki))
            .args(["config", "--local", "--get", "core.sshCommand"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}

fn outcomes(report: &BindAllReport) -> Vec<(&str, BindOutcome)> {
    report
        .wikis
        .iter()
        .map(|e| (e.wiki.as_str(), e.outcome))
        .collect()
}

#[test]
fn only_vaults_whose_remote_accepts_the_key_are_bound_and_each_is_independent() {
    let fx = Fixture::new(&["a", "b", "c", "d"]);
    let (report, probed) = fx.run(
        &DeviceConfig::default(),
        &[
            ("b", ProbeOutcome::Denied("not authorized".into())),
            ("c", ProbeOutcome::Unreachable("offline".into())),
        ],
        Some(URL),
        false,
    );
    assert_eq!(
        outcomes(&report),
        vec![
            ("a", BindOutcome::Bound),
            ("b", BindOutcome::Skipped),
            ("c", BindOutcome::Skipped),
            ("d", BindOutcome::Bound),
        ]
    );
    assert_eq!((report.bound, report.skipped, report.failed), (2, 2, 0));
    assert_eq!(
        probed,
        vec!["a", "b", "c", "d"],
        "every vault was probed first"
    );
    assert!(fx.ssh_command("a").contains("device ssh-command"));
    assert!(fx.ssh_command("d").contains("device ssh-command"));
    for refused in ["b", "c"] {
        assert!(
            fx.ssh_command(refused).is_empty(),
            "{refused} stays untouched"
        );
    }
    let denied = report.wikis[1].reason.as_deref().unwrap();
    assert!(
        denied.contains("vulcan vault enroll b"),
        "names the fix: {denied}"
    );
}

#[test]
fn binding_again_is_already_bound_and_a_dry_run_writes_nothing() {
    let fx = Fixture::new(&["a"]);
    let (preview, _) = fx.run(&DeviceConfig::default(), &[], Some(URL), true);
    assert_eq!(preview.wikis[0].outcome, BindOutcome::Bound);
    assert!(preview.dry_run);
    assert!(fx.ssh_command("a").is_empty(), "dry run wrote nothing");

    fx.run(&DeviceConfig::default(), &[], Some(URL), false);
    let (again, _) = fx.run(&DeviceConfig::default(), &[], Some(URL), false);
    assert_eq!(again.wikis[0].outcome, BindOutcome::Already);
    assert_eq!((again.bound, again.already), (0, 1));
}

#[test]
fn ambient_hosts_and_non_ssh_remotes_are_skipped_without_probing() {
    let fx = Fixture::new(&["a"]);
    let ambient = DeviceConfig {
        forges: vec![ForgeEntry {
            host: "forge.example.com".into(),
            kind: Some(ForgeKind::Forgejo),
            oauth_client_id: None,
            token_env: None,
            transport: Some(TransportPolicy::Ambient),
            login: LoginMode::Auto,
        }],
        ..DeviceConfig::default()
    };
    let (report, probed) = fx.run(&ambient, &[], Some(URL), false);
    assert_eq!(report.wikis[0].outcome, BindOutcome::Skipped);
    assert!(probed.is_empty());

    let (https, probed) = fx.run(
        &DeviceConfig::default(),
        &[],
        Some("https://forge.example.com/eric/mimir.git"),
        false,
    );
    assert_eq!(https.wikis[0].outcome, BindOutcome::Skipped);
    assert!(probed.is_empty());
    assert!(fx.ssh_command("a").is_empty());
}

#[test]
fn a_blocked_vault_is_skipped_untouched_and_never_probed() {
    let mut fx = Fixture::new(&["a", "b"]);
    fx.vaults[0].blocked = Some("the permission profile forbids Git".to_owned());
    let (report, probed) = fx.run(&DeviceConfig::default(), &[], Some(URL), false);
    assert_eq!(
        outcomes(&report),
        vec![("a", BindOutcome::Skipped), ("b", BindOutcome::Bound)]
    );
    assert_eq!(probed, vec!["b"]);
    assert!(fx.ssh_command("a").is_empty());
}

#[test]
fn a_missing_device_identity_is_one_clear_error_not_one_failure_per_vault() {
    let dir = TempDir::new().unwrap();
    let identity = DeviceIdentityStore::at(dir.path().join("device"));
    let state = SyncStateStore::at(dir.path().join("state"));
    let config = DeviceConfig::default();
    let probe = |_: &str, _: &Path| Ok(ProbeOutcome::Accepted);
    let env = BindAllEnvironment {
        device_config: &config,
        identity: &identity,
        state: &state,
        executable: Path::new("/bin/true"),
        probe: &probe,
        remote_url_override: None,
    };
    let error = bind_all(
        &env,
        &[],
        &GitRemote::parse("origin").unwrap(),
        GitConfigMode::Auto,
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("device init"), "{error}");
}
