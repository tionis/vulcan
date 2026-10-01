use super::*;
use crate::device_config::{DeviceConfig, ForgeEntry, TransportPolicy};
use crate::sync_forge::{AuthorizeAction, ForgeKind};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

const URL: &str = "git@forge.example.com:eric/mimir.git";
const BINDING_FILE: &str = "git-transport.json";

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

fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .expect("git");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

struct Fixture {
    dir: TempDir,
    paths: VaultPaths,
    state: SyncStateStore,
    identity: DeviceIdentityStore,
    exe: PathBuf,
}

impl Fixture {
    fn new(init_identity: bool) -> Self {
        let dir = TempDir::new().unwrap();
        let bare = dir.path().join("remote.git");
        let vault = dir.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        git(dir.path(), &["init", "--bare", "-q", "remote.git"]);
        git(&vault, &["init", "-q"]);
        git(&vault, &["remote", "add", "origin", bare.to_str().unwrap()]);
        let identity = DeviceIdentityStore::at(dir.path().join("identity"));
        if init_identity {
            identity.initialize(false).unwrap();
        }
        let exe = dir.path().join("bin").join("vulcan");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            paths: VaultPaths::new(&vault),
            state: SyncStateStore::at(dir.path().join("state/sync/repositories")),
            identity,
            exe,
            dir,
        }
    }

    fn root(&self) -> &Path {
        self.paths.vault_root()
    }

    fn bound(&self) -> bool {
        self.state
            .vault_local_dir(&self.paths)
            .join(BINDING_FILE)
            .is_file()
    }

    fn ssh_command(&self) -> String {
        git_stdout(
            self.root(),
            &["config", "--local", "--get", "core.sshCommand"],
        )
    }

    fn registrations_on_remote(&self) -> Vec<String> {
        git_stdout(
            &self.dir.path().join("remote.git"),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/__vulcan-sync/registrations",
            ],
        )
        .lines()
        .map(str::to_owned)
        .collect()
    }

    fn forge_config(&self) -> Option<ForgeConfig> {
        crate::sync_forge::load_config(&self.paths, &self.state).unwrap()
    }
}

/// A probe that returns scripted outcomes and records, at the moment of each
/// call, whether the vault was already bound or had a `core.sshCommand`.
struct ScriptedProbe<'a> {
    fixture: &'a Fixture,
    outcomes: RefCell<VecDeque<ProbeOutcome>>,
    calls: Cell<u32>,
    bound_at_call: RefCell<Vec<(bool, String)>>,
}

impl<'a> ScriptedProbe<'a> {
    fn new(fixture: &'a Fixture, outcomes: Vec<ProbeOutcome>) -> Self {
        Self {
            fixture,
            outcomes: RefCell::new(outcomes.into()),
            calls: Cell::new(0),
            bound_at_call: RefCell::new(Vec::new()),
        }
    }

    #[allow(clippy::unnecessary_wraps)] // Matches the injected probe's signature.
    fn probe(&self) -> Result<ProbeOutcome, AppError> {
        self.calls.set(self.calls.get() + 1);
        self.bound_at_call
            .borrow_mut()
            .push((self.fixture.bound(), self.fixture.ssh_command()));
        let mut outcomes = self.outcomes.borrow_mut();
        // The last scripted outcome repeats.
        Ok(if outcomes.len() > 1 {
            outcomes.pop_front().unwrap()
        } else {
            outcomes.front().cloned().expect("a scripted outcome")
        })
    }
}

#[derive(Default)]
struct FakeAuthority {
    result: RefCell<Option<Result<AuthorizeAction, AuthorityError>>>,
    calls: RefCell<Vec<(String, String, bool)>>,
}

impl FakeAuthority {
    fn returning(result: Result<AuthorizeAction, AuthorityError>) -> Self {
        Self {
            result: RefCell::new(Some(result)),
            calls: RefCell::default(),
        }
    }
}

impl ForgeAuthority for FakeAuthority {
    fn authorize(
        &self,
        config: &ForgeConfig,
        device_id: &str,
        _public_key: &str,
        _label: Option<&str>,
        login_allowed: bool,
    ) -> Result<ForgeAuthorizeReport, AuthorityError> {
        self.calls
            .borrow_mut()
            .push((config.repo.clone(), device_id.to_owned(), login_allowed));
        match self.result.borrow().clone().expect("a scripted authority") {
            Ok(action) => Ok(ForgeAuthorizeReport {
                version: 1,
                repo: config.repo.clone(),
                device_id: device_id.to_owned(),
                fingerprint: "SHA256:test".to_owned(),
                action,
                dry_run: false,
            }),
            Err(error) => Err(error),
        }
    }
}

fn forge_entry(host: &str) -> ForgeEntry {
    ForgeEntry {
        host: host.to_owned(),
        kind: Some(ForgeKind::Forgejo),
        oauth_client_id: Some("client-abc".to_owned()),
        token_env: None,
        transport: None,
        login: LoginMode::Auto,
    }
}

fn config_with_forge() -> DeviceConfig {
    DeviceConfig {
        forges: vec![forge_entry("forge.example.com")],
        ..DeviceConfig::default()
    }
}

fn request(fx: &Fixture, dry_run: bool) -> EnrollRequest {
    let _ = fx;
    EnrollRequest {
        wiki: "mimir".to_owned(),
        remote: GitRemote::parse("origin").unwrap(),
        no_device_key: false,
        login: LoginPolicy::Never,
        dry_run,
    }
}

fn run(
    fx: &Fixture,
    config: &DeviceConfig,
    probe: &ScriptedProbe<'_>,
    authority: &FakeAuthority,
    request: &EnrollRequest,
) -> EnrollReport {
    let sleeps = Cell::new(0_u32);
    let probe_fn = |_: &str, _: Option<&Path>| probe.probe();
    let sleep_fn = |_: Duration| sleeps.set(sleeps.get() + 1);
    let env = EnrollEnvironment {
        device_config: config,
        identity: &fx.identity,
        state: &fx.state,
        executable: &fx.exe,
        probe: &probe_fn,
        authority,
        sleep: &sleep_fn,
        remote_url_override: Some(URL),
    };
    enroll_vault(&fx.paths, &env, request).unwrap()
}

fn status(report: &EnrollReport, name: &str) -> StepStatus {
    report
        .steps
        .iter()
        .rev()
        .find(|step| step.name == name)
        .unwrap_or_else(|| panic!("no `{name}` step in {:?}", report.steps))
        .status
}

fn has_step(report: &EnrollReport, name: &str) -> bool {
    report.steps.iter().any(|step| step.name == name)
}

#[test]
fn an_accepted_device_key_is_bound_registered_and_configured_for_plain_git() {
    let fx = Fixture::new(true);
    let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
    let authority = FakeAuthority::default();
    let report = run(
        &fx,
        &DeviceConfig::default(),
        &probe,
        &authority,
        &request(&fx, false),
    );

    assert_eq!(report.state, EnrollState::Bound);
    assert_eq!(status(&report, "probe"), StepStatus::Already);
    assert_eq!(status(&report, "bind"), StepStatus::Done);
    assert_eq!(status(&report, "registration"), StepStatus::Done);
    assert!(
        authority.calls.borrow().is_empty(),
        "no authority was needed"
    );
    assert!(fx.bound());
    assert!(fx.ssh_command().contains("device ssh-command"));
    assert_eq!(fx.registrations_on_remote().len(), 1);
    assert_eq!(probe.calls.get(), 1);
}

#[test]
fn enrolling_again_changes_nothing() {
    let fx = Fixture::new(true);
    let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
    let authority = FakeAuthority::default();
    run(
        &fx,
        &DeviceConfig::default(),
        &probe,
        &authority,
        &request(&fx, false),
    );
    let command = fx.ssh_command();

    let second = run(
        &fx,
        &DeviceConfig::default(),
        &probe,
        &authority,
        &request(&fx, false),
    );
    assert_eq!(second.state, EnrollState::Bound);
    assert_eq!(status(&second, "identity"), StepStatus::Already);
    assert_eq!(status(&second, "bind"), StepStatus::Already);
    assert_eq!(status(&second, "registration"), StepStatus::Already);
    assert_eq!(fx.ssh_command(), command);
    assert_eq!(
        fx.registrations_on_remote().len(),
        1,
        "no duplicate registration"
    );
}

#[test]
fn an_ambient_policy_or_a_non_ssh_remote_does_nothing() {
    for (label, config, no_device_key, url) in [
        (
            "default ambient",
            DeviceConfig {
                transport: crate::device_config::TransportSection {
                    default: TransportPolicy::Ambient,
                },
                ..DeviceConfig::default()
            },
            false,
            URL,
        ),
        (
            "per-host override",
            DeviceConfig {
                forges: vec![ForgeEntry {
                    transport: Some(TransportPolicy::Ambient),
                    ..forge_entry("forge.example.com")
                }],
                ..DeviceConfig::default()
            },
            false,
            URL,
        ),
        ("per-command opt out", DeviceConfig::default(), true, URL),
    ] {
        let fx = Fixture::new(true);
        let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
        let authority = FakeAuthority::default();
        let mut req = request(&fx, false);
        req.no_device_key = no_device_key;
        let report = run_with_url(&fx, &config, &probe, &authority, &req, url);
        assert_eq!(report.state, EnrollState::Skipped, "{label}");
        assert_eq!(report.policy, TransportPolicy::Ambient, "{label}");
        assert_eq!(probe.calls.get(), 0, "{label}: no network was touched");
        assert!(!fx.bound() && fx.ssh_command().is_empty(), "{label}");
        assert!(fx.registrations_on_remote().is_empty(), "{label}");
    }

    let fx = Fixture::new(true);
    let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
    let report = run_with_url(
        &fx,
        &DeviceConfig::default(),
        &probe,
        &FakeAuthority::default(),
        &request(&fx, false),
        "https://forge.example.com/eric/mimir.git",
    );
    assert_eq!(report.state, EnrollState::Skipped);
    assert!(report.steps.iter().any(|step| step
        .detail
        .as_deref()
        .is_some_and(|d| d.contains("not an SSH URL"))));
    assert_eq!(probe.calls.get(), 0);
}

fn run_with_url(
    fx: &Fixture,
    config: &DeviceConfig,
    probe: &ScriptedProbe<'_>,
    authority: &FakeAuthority,
    request: &EnrollRequest,
    url: &str,
) -> EnrollReport {
    let probe_fn = |_: &str, _: Option<&Path>| probe.probe();
    let sleep_fn = |_: Duration| {};
    let env = EnrollEnvironment {
        device_config: config,
        identity: &fx.identity,
        state: &fx.state,
        executable: &fx.exe,
        probe: &probe_fn,
        authority,
        sleep: &sleep_fn,
        remote_url_override: Some(url),
    };
    enroll_vault(&fx.paths, &env, request).unwrap()
}

#[test]
fn a_refused_key_without_a_forge_stays_pending_and_is_never_bound() {
    let fx = Fixture::new(true);
    let probe = ScriptedProbe::new(
        &fx,
        vec![ProbeOutcome::Denied(
            "Permission denied (publickey).".to_owned(),
        )],
    );
    let authority = FakeAuthority::default();
    let report = run(
        &fx,
        &DeviceConfig::default(),
        &probe,
        &authority,
        &request(&fx, false),
    );

    assert_eq!(report.state, EnrollState::Pending);
    assert_eq!(status(&report, "authority"), StepStatus::Pending);
    assert!(!has_step(&report, "bind"), "bind never ran");
    assert!(
        !fx.bound() && fx.ssh_command().is_empty(),
        "plain git is untouched"
    );
    assert!(authority.calls.borrow().is_empty());
    // The device still becomes visible to an administrator, via ambient access.
    assert_eq!(fx.registrations_on_remote().len(), 1);
    let steps = report.next_steps.join("\n");
    assert!(
        steps.contains("vulcan device public-key") && steps.contains("forge sync"),
        "{steps}"
    );
    assert!(steps.contains("vulcan vault enroll mimir"), "{steps}");
}

#[test]
fn a_forge_authorizes_the_key_and_only_then_is_the_vault_bound() {
    let fx = Fixture::new(true);
    let denied = ProbeOutcome::Denied("not authorized".to_owned());
    let probe = ScriptedProbe::new(&fx, vec![denied, ProbeOutcome::Accepted]);
    let authority = FakeAuthority::returning(Ok(AuthorizeAction::Added));
    let report = run(
        &fx,
        &config_with_forge(),
        &probe,
        &authority,
        &request(&fx, false),
    );

    assert_eq!(report.state, EnrollState::Bound);
    assert_eq!(status(&report, "authority"), StepStatus::Done);
    assert_eq!(status(&report, "bind"), StepStatus::Done);
    let calls = authority.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].0, "eric/mimir",
        "the repository is derived from the remote"
    );
    assert_eq!(Some(&calls[0].1), report.device_id.as_ref());
    // Every probe happened before anything was bound.
    assert!(
        probe
            .bound_at_call
            .borrow()
            .iter()
            .all(|(bound, command)| !bound && command.is_empty()),
        "nothing was bound at any probe: {:?}",
        probe.bound_at_call.borrow()
    );
    // The derived forge settings were saved for later `forge sync`.
    let saved = fx.forge_config().expect("forge settings saved");
    assert_eq!(
        (saved.url.as_str(), saved.repo.as_str()),
        ("https://forge.example.com", "eric/mimir")
    );
    assert_eq!(saved.oauth_client_id.as_deref(), Some("client-abc"));
}

#[test]
fn a_key_that_is_added_but_not_yet_accepted_is_retried_then_left_pending() {
    let fx = Fixture::new(true);
    let denied = ProbeOutcome::Denied("not authorized".to_owned());
    let probe = ScriptedProbe::new(&fx, vec![denied]);
    let authority = FakeAuthority::returning(Ok(AuthorizeAction::Added));
    let sleeps = Cell::new(0_u32);
    let probe_fn = |_: &str, _: Option<&Path>| probe.probe();
    let sleep_fn = |_: Duration| sleeps.set(sleeps.get() + 1);
    let env = EnrollEnvironment {
        device_config: &config_with_forge(),
        identity: &fx.identity,
        state: &fx.state,
        executable: &fx.exe,
        probe: &probe_fn,
        authority: &authority,
        sleep: &sleep_fn,
        remote_url_override: Some(URL),
    };
    let report = enroll_vault(&fx.paths, &env, &request(&fx, false)).unwrap();

    assert_eq!(report.state, EnrollState::Pending);
    assert_eq!(
        probe.calls.get(),
        1 + REPROBE_ATTEMPTS,
        "the first probe plus the retries"
    );
    assert_eq!(sleeps.get(), REPROBE_ATTEMPTS - 1);
    assert!(!fx.bound() && fx.ssh_command().is_empty());
}

#[test]
fn a_missing_login_is_pending_with_the_exact_command_and_login_is_requested_only_when_allowed() {
    for (login, entry_login, expected_allowed) in [
        (LoginPolicy::Never, LoginMode::Auto, false),
        (LoginPolicy::Interactive, LoginMode::Auto, true),
        (LoginPolicy::Interactive, LoginMode::Never, false),
        (LoginPolicy::Forced, LoginMode::Never, true),
    ] {
        let fx = Fixture::new(true);
        let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Denied("no".to_owned())]);
        let authority = FakeAuthority::returning(Err(AuthorityError::NeedsLogin));
        let mut config = config_with_forge();
        config.forges[0].login = entry_login;
        let mut req = request(&fx, false);
        req.login = login;
        let report = run(&fx, &config, &probe, &authority, &req);

        assert_eq!(report.state, EnrollState::Pending);
        assert_eq!(
            authority.calls.borrow()[0].2,
            expected_allowed,
            "{login:?}/{entry_login:?}"
        );
        assert!(report
            .next_steps
            .iter()
            .any(|step| step.contains("vulcan sync forge login --wiki mimir")));
        assert!(!fx.bound());
    }
}

#[test]
fn forge_failures_and_missing_credentials_are_reported_not_fatal() {
    for (result, expected) in [
        (
            Err(AuthorityError::NoCredential("set $FORGE_TOKEN".to_owned())),
            StepStatus::Pending,
        ),
        (
            Err(AuthorityError::Failed("HTTP 403".to_owned())),
            StepStatus::Failed,
        ),
    ] {
        let fx = Fixture::new(true);
        let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Denied("no".to_owned())]);
        let authority = FakeAuthority::returning(result);
        let report = run(
            &fx,
            &config_with_forge(),
            &probe,
            &authority,
            &request(&fx, false),
        );
        assert_eq!(report.state, EnrollState::Pending);
        assert_eq!(status(&report, "authority"), expected);
        assert!(!fx.bound());
    }
}

#[test]
fn an_unreachable_remote_is_pending_and_never_asks_the_forge() {
    let fx = Fixture::new(true);
    let probe = ScriptedProbe::new(
        &fx,
        vec![ProbeOutcome::Unreachable(
            "Could not resolve host".to_owned(),
        )],
    );
    let authority = FakeAuthority::returning(Ok(AuthorizeAction::Added));
    let report = run(
        &fx,
        &config_with_forge(),
        &probe,
        &authority,
        &request(&fx, false),
    );
    assert_eq!(report.state, EnrollState::Pending);
    assert!(authority.calls.borrow().is_empty());
    assert!(report
        .next_steps
        .iter()
        .any(|step| step.contains("check the network")));
    assert!(!fx.bound());
}

#[test]
fn a_binding_whose_key_the_remote_no_longer_accepts_is_reported_not_removed() {
    let fx = Fixture::new(true);
    let accepted = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
    run(
        &fx,
        &DeviceConfig::default(),
        &accepted,
        &FakeAuthority::default(),
        &request(&fx, false),
    );
    assert!(fx.bound());
    let command = fx.ssh_command();

    // The device was later revoked on the forge.
    let denied = ScriptedProbe::new(&fx, vec![ProbeOutcome::Denied("revoked".to_owned())]);
    let report = run(
        &fx,
        &DeviceConfig::default(),
        &denied,
        &FakeAuthority::default(),
        &request(&fx, false),
    );
    assert_eq!(report.state, EnrollState::Pending);
    assert!(
        fx.bound(),
        "the binding is left for the user to decide about"
    );
    assert_eq!(fx.ssh_command(), command);
}

#[test]
fn a_dry_run_probes_but_writes_and_binds_nothing() {
    let fx = Fixture::new(false);
    let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
    let report = run(
        &fx,
        &DeviceConfig::default(),
        &probe,
        &FakeAuthority::default(),
        &request(&fx, true),
    );
    assert_eq!(status(&report, "identity"), StepStatus::Planned);
    assert_eq!(report.state, EnrollState::Pending);
    assert_eq!(
        probe.calls.get(),
        0,
        "no identity yet, so nothing to probe with"
    );
    assert!(
        fx.identity.inspect().device_id.is_none(),
        "a dry run never initializes the identity"
    );

    // With an identity, a dry run still probes (read-only) and plans the rest.
    let fx = Fixture::new(true);
    let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
    let report = run(
        &fx,
        &DeviceConfig::default(),
        &probe,
        &FakeAuthority::default(),
        &request(&fx, true),
    );
    assert_eq!(probe.calls.get(), 1);
    assert_eq!(status(&report, "bind"), StepStatus::Planned);
    assert_eq!(status(&report, "registration"), StepStatus::Planned);
    assert!(!fx.bound() && fx.ssh_command().is_empty());
    assert!(fx.registrations_on_remote().is_empty());

    // A dry run does not authorize, and does not save derived forge settings.
    let fx = Fixture::new(true);
    let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Denied("no".to_owned())]);
    let authority = FakeAuthority::returning(Ok(AuthorizeAction::Added));
    let report = run(
        &fx,
        &config_with_forge(),
        &probe,
        &authority,
        &request(&fx, true),
    );
    assert_eq!(status(&report, "authority"), StepStatus::Planned);
    assert!(authority.calls.borrow().is_empty());
    assert!(fx.forge_config().is_none());
}

#[test]
fn a_missing_identity_is_created_by_a_real_run() {
    let fx = Fixture::new(false);
    let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
    let report = run(
        &fx,
        &DeviceConfig::default(),
        &probe,
        &FakeAuthority::default(),
        &request(&fx, false),
    );
    assert_eq!(status(&report, "identity"), StepStatus::Done);
    assert_eq!(report.state, EnrollState::Bound);
    assert!(fx.identity.inspect().device_id.is_some());
}

#[test]
fn a_stale_binding_is_repaired_by_enrolling_again() {
    let fx = Fixture::new(true);
    let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
    run(
        &fx,
        &DeviceConfig::default(),
        &probe,
        &FakeAuthority::default(),
        &request(&fx, false),
    );
    fs::remove_file(&fx.exe).unwrap();
    fs::write(&fx.exe, "#!/bin/sh\n").unwrap();
    let moved = fx.dir.path().join("moved").join("vulcan");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::write(&moved, "#!/bin/sh\n").unwrap();

    let probe_fn = |_: &str, _: Option<&Path>| Ok(ProbeOutcome::Accepted);
    let sleep_fn = |_: Duration| {};
    let authority = FakeAuthority::default();
    let env = EnrollEnvironment {
        device_config: &DeviceConfig::default(),
        identity: &fx.identity,
        state: &fx.state,
        executable: &moved,
        probe: &probe_fn,
        authority: &authority,
        sleep: &sleep_fn,
        remote_url_override: Some(URL),
    };
    let report = enroll_vault(&fx.paths, &env, &request(&fx, false)).unwrap();
    assert_eq!(
        status(&report, "bind"),
        StepStatus::Done,
        "the value now names the new executable"
    );
    assert!(fx
        .ssh_command()
        .contains(&moved.to_string_lossy().into_owned()));
}

#[test]
fn a_vault_without_the_remote_is_skipped() {
    let fx = Fixture::new(true);
    let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
    let probe_fn = |_: &str, _: Option<&Path>| probe.probe();
    let sleep_fn = |_: Duration| {};
    let authority = FakeAuthority::default();
    let env = EnrollEnvironment {
        device_config: &DeviceConfig::default(),
        identity: &fx.identity,
        state: &fx.state,
        executable: &fx.exe,
        probe: &probe_fn,
        authority: &authority,
        sleep: &sleep_fn,
        remote_url_override: None,
    };
    let mut req = request(&fx, false);
    req.remote = GitRemote::parse("upstream").unwrap();
    let report = enroll_vault(&fx.paths, &env, &req).unwrap();
    assert_eq!(report.state, EnrollState::Skipped);
    assert_eq!(probe.calls.get(), 0);
}

#[test]
fn a_foreign_core_ssh_command_never_blocks_enrollment() {
    let fx = Fixture::new(true);
    git(fx.root(), &["config", "core.sshCommand", "ssh -i /mine"]);
    let probe = ScriptedProbe::new(&fx, vec![ProbeOutcome::Accepted]);
    let report = run(
        &fx,
        &DeviceConfig::default(),
        &probe,
        &FakeAuthority::default(),
        &request(&fx, false),
    );
    assert_eq!(report.state, EnrollState::Bound);
    assert_eq!(fx.ssh_command(), "ssh -i /mine");
    let bind = report
        .steps
        .iter()
        .find(|step| step.name == "bind")
        .unwrap();
    assert!(bind
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("left alone")));
}
