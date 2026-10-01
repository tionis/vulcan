//! Optional device-key Git-over-SSH transport binding (Roadmap 12.21.1).
//!
//! A binding is device-local operational state that tells Vulcan's own Git
//! engine to authenticate with the installation's device key. It never changes
//! global Git/SSH configuration and, unless requested, not even the
//! repository's. See `docs/specs/device-transport-auth.md`.

use crate::device_identity::{DeviceIdentityStatus, DeviceIdentityStore};
use crate::sync_state::SyncStateStore;
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use vulcan_core::VaultPaths;
use vulcan_sync::GitCliEngine;

const BINDING_FILE: &str = "git-transport.json";
const BINDING_VERSION: u32 = 1;
const MAX_BINDING_BYTES: u64 = 4 * 1024;
const ELIGIBLE_PROVIDER: &str = "file_v1";
/// The shell script behind a Vulcan-owned `core.sshCommand`. Git runs the value
/// through `sh` with the SSH arguments appended, so `$0` is the `vulcan`
/// executable and `"$@"` are those arguments. If the executable has gone
/// missing (an upgrade moved it, a build directory was removed), plain `ssh`
/// runs instead, so a stale value can never leave `git` worse off than before.
const WRAPPER_SCRIPT: &str = r#"[ -x "$0" ] && exec "$0" device ssh-command "$@" || exec ssh "$@""#;
const SSH_OPTIONS: [&str; 6] = [
    "-o",
    "IdentitiesOnly=yes",
    "-o",
    "IdentityAgent=none",
    "-o",
    "BatchMode=yes",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingRecord {
    version: u32,
    /// Device ID the user bound. A later key replacement must be re-bound.
    device_id: String,
    /// Whether `bind` wrote the repository-local `core.sshCommand`.
    git_config: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitTransportState {
    NotBound,
    Usable,
    /// Bound, but the device key cannot currently authenticate. Sync fails
    /// closed; it never falls back to another credential.
    DeviceKeyUnavailable,
}

/// Whether `bind` also sets the repository-local `core.sshCommand`, so plain
/// `git` in the repository uses the device key too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitConfigMode {
    /// Set it unless another tool already owns the setting; then leave that
    /// value alone and say so. The default.
    #[default]
    Auto,
    /// Set it, failing if a value Vulcan does not own is present.
    Require,
    /// Do not set it, and remove a Vulcan-owned value.
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitConfigState {
    /// The binding does not manage `core.sshCommand`.
    NotManaged,
    /// Managed and present with a working Vulcan-owned value.
    Managed,
    /// Managed but the value is absent.
    Missing,
    /// Managed but another tool replaced the value.
    Foreign,
    /// Managed, but the executable it names no longer exists, so plain `git`
    /// falls back to ordinary `ssh`. Re-run `sync transport bind` to repair.
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitTransportStatus {
    pub state: GitTransportState,
    pub bound_device_id: Option<String>,
    pub git_config: GitConfigState,
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)] // A report: one flag per distinct outcome.
pub struct GitTransportBindReport {
    pub dry_run: bool,
    pub device_id: String,
    pub git_config_written: bool,
    pub git_config_removed: bool,
    /// Why plain `git` was not configured, when it was not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_config_skipped: Option<String>,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitTransportUnbindReport {
    pub dry_run: bool,
    pub was_bound: bool,
    pub git_config_removed: bool,
}

fn load_binding(
    paths: &VaultPaths,
    state: &SyncStateStore,
) -> Result<Option<BindingRecord>, AppError> {
    let Some(bytes) = state.read_vault_local(paths, BINDING_FILE, MAX_BINDING_BYTES)? else {
        return Ok(None);
    };
    let record: BindingRecord = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::operation(format!("invalid git transport binding: {error}")))?;
    if record.version != BINDING_VERSION {
        return Err(AppError::operation(format!(
            "unsupported git transport binding version {}",
            record.version
        )));
    }
    Ok(Some(record))
}

/// Resolve the eligible device key path, or explain why it is unusable.
fn eligible_key(store: &DeviceIdentityStore) -> Result<(String, PathBuf), String> {
    let report = store.inspect();
    match report.status {
        DeviceIdentityStatus::Uninitialized => {
            return Err("device identity is uninitialized; run `vulcan device init`".to_owned())
        }
        DeviceIdentityStatus::Ready => {}
        DeviceIdentityStatus::Degraded | DeviceIdentityStatus::Invalid => {
            return Err(report
                .diagnostic
                .unwrap_or_else(|| "device identity is not usable".to_owned()))
        }
    }
    if report.key_provider.as_deref() != Some(ELIGIBLE_PROVIDER) || !report.private_key_available {
        return Err(format!(
            "device key provider must be `{ELIGIBLE_PROVIDER}` with an available private key"
        ));
    }
    let device_id = report
        .device_id
        .ok_or_else(|| "device identity has no device ID".to_owned())?;
    let key = store
        .private_key_path()
        .map_err(|error| error.to_string())?;
    Ok((device_id, key))
}

/// POSIX-quote one word for the shell command line Git runs.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn path_word(path: &Path) -> String {
    shell_quote(&path.to_string_lossy())
}

/// Exact `GIT_SSH_COMMAND` for Vulcan-spawned Git processes.
fn ssh_command_for(key: &Path) -> String {
    let mut parts = vec!["ssh".to_owned(), "-i".to_owned(), path_word(key)];
    parts.extend(SSH_OPTIONS.iter().map(|option| (*option).to_owned()));
    parts.join(" ")
}

/// Command that fails every SSH connection with a clear message.
fn failing_ssh_command(reason: &str) -> String {
    let script = format!(
        "echo {} >&2; exit 255",
        shell_quote(&format!(
            "vulcan: device-key Git transport unavailable: {reason}"
        ))
    );
    format!("sh -c {} --", shell_quote(&script))
}

/// Argv for the `vulcan device ssh-command` wrapper: `ssh` with the device
/// key and the pinned options, followed by Git's own arguments.
pub fn device_ssh_argv(store: &DeviceIdentityStore) -> Result<Vec<String>, AppError> {
    let (_, key) = eligible_key(store).map_err(AppError::operation)?;
    let mut argv = vec!["-i".to_owned(), key.to_string_lossy().into_owned()];
    argv.extend(SSH_OPTIONS.iter().map(|option| (*option).to_owned()));
    Ok(argv)
}

/// Builds the sync engine for a vault, honoring its transport binding.
///
/// Never fails: a bound vault whose key is unavailable gets an engine whose
/// SSH transport fails closed, so local capture and recovery still run.
#[must_use]
pub fn git_engine(paths: &VaultPaths) -> GitCliEngine {
    apply_transport(GitCliEngine::default(), paths)
}

/// Layers the vault's transport binding from an explicit state store, so a sync
/// that was given a store derives its transport from that same store.
#[must_use]
pub fn apply_transport_with_state(
    engine: GitCliEngine,
    paths: &VaultPaths,
    state: &SyncStateStore,
) -> GitCliEngine {
    git_engine_with_store(engine, paths, state, Some(state.identity()))
}

/// Layers the vault's transport binding, if any, onto an existing engine.
#[must_use]
pub fn apply_transport(engine: GitCliEngine, paths: &VaultPaths) -> GitCliEngine {
    match SyncStateStore::user_default() {
        Ok(state) => git_engine_with_store(engine, paths, &state, Some(state.identity())),
        // Without a state directory no binding can exist.
        Err(_) => engine,
    }
}

fn git_engine_with_store(
    engine: GitCliEngine,
    paths: &VaultPaths,
    state: &SyncStateStore,
    store: Option<&DeviceIdentityStore>,
) -> GitCliEngine {
    match load_binding(paths, state) {
        Ok(None) => engine,
        Ok(Some(record)) => match store.map(eligible_key) {
            Some(Ok((device_id, key))) if device_id == record.device_id => {
                engine.with_ssh_command(ssh_command_for(&key))
            }
            Some(Ok(_)) => engine.with_ssh_command(failing_ssh_command(
                "the device key changed since binding; re-bind",
            )),
            Some(Err(reason)) => engine.with_ssh_command(failing_ssh_command(&reason)),
            None => engine.with_ssh_command(failing_ssh_command("no device identity directory")),
        },
        Err(error) => engine.with_ssh_command(failing_ssh_command(&error.to_string())),
    }
}

/// Read-only transport state; never initializes identity or touches a remote.
pub fn transport_status(paths: &VaultPaths) -> Result<GitTransportStatus, AppError> {
    let Ok(state) = SyncStateStore::user_default() else {
        return Ok(not_bound());
    };
    transport_status_with_store(paths, &state, state.identity())
}

fn not_bound() -> GitTransportStatus {
    GitTransportStatus {
        state: GitTransportState::NotBound,
        bound_device_id: None,
        git_config: GitConfigState::NotManaged,
        diagnostic: None,
    }
}

fn transport_status_with_store(
    paths: &VaultPaths,
    state: &SyncStateStore,
    store: &DeviceIdentityStore,
) -> Result<GitTransportStatus, AppError> {
    let Some(record) = load_binding(paths, state)? else {
        return Ok(not_bound());
    };
    let (transport, diagnostic) = match eligible_key(store) {
        Ok((device_id, _)) if device_id == record.device_id => (GitTransportState::Usable, None),
        Ok(_) => (
            GitTransportState::DeviceKeyUnavailable,
            Some("the device key changed since binding; re-bind".to_owned()),
        ),
        Err(reason) => (GitTransportState::DeviceKeyUnavailable, Some(reason)),
    };
    let git_config = if record.git_config {
        match read_core_ssh_command(paths.vault_root())? {
            None => GitConfigState::Missing,
            Some(value) if is_owned(&value) => {
                if owned_executable(&value).is_some_and(|path| path.is_file()) {
                    GitConfigState::Managed
                } else {
                    GitConfigState::Stale
                }
            }
            Some(_) => GitConfigState::Foreign,
        }
    } else {
        GitConfigState::NotManaged
    };
    Ok(GitTransportStatus {
        state: transport,
        bound_device_id: Some(record.device_id),
        git_config,
        diagnostic,
    })
}

/// Binds the vault's Git transport to the device key.
pub fn bind_transport(
    paths: &VaultPaths,
    remote: &str,
    mode: GitConfigMode,
    dry_run: bool,
) -> Result<GitTransportBindReport, AppError> {
    let state = SyncStateStore::user_default()?;
    bind_transport_with_store(
        paths,
        &state,
        state.identity(),
        remote,
        mode,
        dry_run,
        &std::env::current_exe().map_err(AppError::operation)?,
    )
}

/// The `core.sshCommand` value naming `executable`.
fn wrapper_value(executable: &Path) -> String {
    format!("{}{}", wrapper_prefix(), path_word(executable))
}

fn wrapper_prefix() -> String {
    format!("sh -c {} ", shell_quote(WRAPPER_SCRIPT))
}

/// The executable a Vulcan-owned value names.
fn owned_executable(value: &str) -> Option<PathBuf> {
    let quoted = value.strip_prefix(wrapper_prefix().as_str())?;
    let inner = quoted.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(PathBuf::from(inner.replace("'\\''", "'")))
}

pub(crate) fn bind_transport_with_store(
    paths: &VaultPaths,
    state: &SyncStateStore,
    store: &DeviceIdentityStore,
    remote: &str,
    mode: GitConfigMode,
    dry_run: bool,
    executable: &Path,
) -> Result<GitTransportBindReport, AppError> {
    let (device_id, _) = eligible_key(store).map_err(AppError::operation)?;
    let url = remote_url(paths.vault_root(), remote)?;
    if !is_ssh_url(&url) {
        return Err(AppError::operation(format!(
            "remote `{remote}` is not an SSH remote; device-key transport needs ssh:// or scp-like URLs"
        )));
    }
    let existing = load_binding(paths, state)?;
    let current = read_core_ssh_command(paths.vault_root())?;
    let owned_current = current.as_deref().is_some_and(is_owned);
    let foreign_current = current.is_some() && !owned_current;
    let mut skipped = None;
    let manage = match mode {
        GitConfigMode::Skip => false,
        GitConfigMode::Require if foreign_current => {
            return Err(AppError::operation(
                "core.sshCommand already holds a value Vulcan does not own; refusing to overwrite it",
            ));
        }
        GitConfigMode::Auto if foreign_current => {
            skipped = Some(
                "core.sshCommand is already set by something else and was left alone, so plain `git` still uses its own SSH setup"
                    .to_owned(),
            );
            false
        }
        GitConfigMode::Require | GitConfigMode::Auto => true,
    };
    let wrapper = wrapper_value(executable);
    let desired = BindingRecord {
        version: BINDING_VERSION,
        device_id: device_id.clone(),
        git_config: manage,
    };
    let config_written = manage && current.as_deref() != Some(wrapper.as_str());
    let config_removed = !manage && owned_current;
    let changed = existing.as_ref() != Some(&desired) || config_written || config_removed;
    if !dry_run && changed {
        if config_written {
            run_git_config(
                paths.vault_root(),
                &["config", "--local", "core.sshCommand", &wrapper],
            )?;
        } else if config_removed {
            run_git_config(
                paths.vault_root(),
                &["config", "--local", "--unset", "core.sshCommand"],
            )?;
        }
        let bytes = serde_json::to_vec_pretty(&desired).map_err(AppError::operation)?;
        state.write_vault_local(paths, BINDING_FILE, &bytes)?;
    }
    Ok(GitTransportBindReport {
        dry_run,
        device_id,
        git_config_written: config_written,
        git_config_removed: config_removed,
        git_config_skipped: skipped,
        changed,
    })
}

/// Removes the binding and, only when Vulcan-owned, the repository config value.
pub fn unbind_transport(
    paths: &VaultPaths,
    dry_run: bool,
) -> Result<GitTransportUnbindReport, AppError> {
    unbind_transport_with_store(paths, &SyncStateStore::user_default()?, dry_run)
}

fn unbind_transport_with_store(
    paths: &VaultPaths,
    state: &SyncStateStore,
    dry_run: bool,
) -> Result<GitTransportUnbindReport, AppError> {
    let Some(record) = load_binding(paths, state)? else {
        return Ok(GitTransportUnbindReport {
            dry_run,
            was_bound: false,
            git_config_removed: false,
        });
    };
    let owned_value = record.git_config
        && read_core_ssh_command(paths.vault_root())?
            .as_deref()
            .is_some_and(is_owned);
    if !dry_run {
        if owned_value {
            run_git_config(
                paths.vault_root(),
                &["config", "--local", "--unset", "core.sshCommand"],
            )?;
        }
        state.remove_vault_local(paths, BINDING_FILE)?;
    }
    Ok(GitTransportUnbindReport {
        dry_run,
        was_bound: true,
        git_config_removed: owned_value,
    })
}

fn is_owned(value: &str) -> bool {
    value.starts_with(wrapper_prefix().as_str())
}

fn is_ssh_url(url: &str) -> bool {
    if let Some(rest) = url
        .strip_prefix("ssh://")
        .or_else(|| url.strip_prefix("git+ssh://"))
    {
        return !rest.is_empty();
    }
    if url.contains("://") {
        return false;
    }
    // scp-like `[user@]host:path`; a lone letter before `:` is a Windows drive.
    match url.split_once(':') {
        Some((host, path)) => {
            let drive_letter = host.len() == 1 && host.chars().all(|c| c.is_ascii_alphabetic());
            !host.is_empty() && !path.is_empty() && !host.contains('/') && !drive_letter
        }
        None => false,
    }
}

pub(crate) fn remote_url(root: &Path, remote: &str) -> Result<String, AppError> {
    let output = git_output(root, &["remote", "get-url", remote])?;
    Ok(output.trim().to_owned())
}

fn read_core_ssh_command(root: &Path) -> Result<Option<String>, AppError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--local", "--get", "core.sshCommand"])
        .output()
        .map_err(AppError::operation)?;
    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_owned(),
        )),
        Some(1) => Ok(None),
        _ => Err(AppError::operation("failed to read core.sshCommand")),
    }
}

fn git_output(root: &Path, args: &[&str]) -> Result<String, AppError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(AppError::operation)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(AppError::operation(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn run_git_config(root: &Path, args: &[&str]) -> Result<(), AppError> {
    git_output(root, args).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn classifies_ssh_remotes() {
        for url in [
            "ssh://git@forge.example/o/r.git",
            "git+ssh://forge.example/o/r.git",
            "git@forge.example:o/r.git",
            "forge.example:o/r.git",
        ] {
            assert!(is_ssh_url(url), "{url}");
        }
        for url in [
            "https://forge.example/o/r.git",
            "file:///srv/r.git",
            "/srv/r.git",
            "C:\\repos\\r.git",
            "C:/repos/r.git",
            "ssh://",
        ] {
            assert!(!is_ssh_url(url), "{url}");
        }
    }

    #[test]
    fn quoting_survives_spaces_and_single_quotes() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        let command = ssh_command_for(Path::new("/home/me/it's here/id"));
        assert!(command.starts_with("ssh -i '/home/me/it'\\''s here/id' -o IdentitiesOnly=yes"));
        assert!(command.contains("IdentityAgent=none"));
        assert!(command.contains("BatchMode=yes"));
        assert!(!command.contains("StrictHostKeyChecking"));
    }

    #[test]
    fn failing_command_exits_nonzero_with_reason() {
        let command = failing_ssh_command("locked 'store'");
        let output = Command::new("sh")
            .arg("-c")
            .arg(format!("{command} host git-upload-pack repo"))
            .output()
            .expect("sh");
        assert_eq!(output.status.code(), Some(255));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("device-key Git transport unavailable: locked 'store'"));
    }

    #[test]
    fn ownership_marker_matches_only_the_wrapper() {
        assert!(is_owned(&wrapper_value(Path::new("/usr/bin/vulcan"))));
        assert!(!is_owned("ssh -i ~/.ssh/other"));
        assert!(
            !is_owned("'/usr/bin/vulcan' device ssh-command"),
            "the old suffix form is not ours"
        );
    }

    #[test]
    fn the_wrapper_value_round_trips_its_executable_even_with_awkward_paths() {
        for path in [
            "/usr/bin/vulcan",
            "/opt/my tools/vulcan",
            "/home/me/it's here/vulcan",
        ] {
            let value = wrapper_value(Path::new(path));
            assert_eq!(
                owned_executable(&value),
                Some(PathBuf::from(path)),
                "{value}"
            );
        }
        assert_eq!(owned_executable("ssh -i /other"), None);
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    struct Fixture {
        dir: tempfile::TempDir,
        paths: VaultPaths,
        store: DeviceIdentityStore,
        state: SyncStateStore,
        exe: PathBuf,
    }

    /// A fixture whose "vulcan" is a real executable script, so the written
    /// `core.sshCommand` can actually be run.
    fn fixture(remote: &str, init_identity: bool) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path().join("vault");
        fs::create_dir_all(&vault).expect("vault");
        git(&vault, &["init", "-q"]);
        git(&vault, &["remote", "add", "origin", remote]);
        let store = DeviceIdentityStore::at(dir.path().join("identity"));
        if init_identity {
            store.initialize(false).expect("identity");
        }
        let exe = dir.path().join("my tools").join("vulcan");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "#!/bin/sh\necho \"vulcan:$*\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Fixture {
            paths: VaultPaths::new(&vault),
            store,
            state: SyncStateStore::at(dir.path().join("state/sync/repositories")),
            exe,
            dir,
        }
    }

    fn bind(
        fx: &Fixture,
        mode: GitConfigMode,
        dry_run: bool,
    ) -> Result<GitTransportBindReport, AppError> {
        bind_transport_with_store(
            &fx.paths, &fx.state, &fx.store, "origin", mode, dry_run, &fx.exe,
        )
    }

    fn status(fx: &Fixture) -> GitTransportStatus {
        transport_status_with_store(&fx.paths, &fx.state, &fx.store).unwrap()
    }

    fn config_value(fx: &Fixture) -> Option<String> {
        read_core_ssh_command(fx.paths.vault_root()).unwrap()
    }

    #[test]
    fn bind_configures_plain_git_by_default() {
        let fx = fixture("git@forge.example:o/r.git", true);
        assert_eq!(status(&fx).state, GitTransportState::NotBound);

        let preview = bind(&fx, GitConfigMode::default(), true).unwrap();
        assert!(preview.dry_run && preview.changed && preview.git_config_written);
        assert_eq!(
            status(&fx).state,
            GitTransportState::NotBound,
            "a dry run binds nothing"
        );
        assert_eq!(config_value(&fx), None);

        let report = bind(&fx, GitConfigMode::default(), false).unwrap();
        assert!(report.git_config_written && report.git_config_skipped.is_none());
        let state = status(&fx);
        assert_eq!(
            (state.state, state.git_config),
            (GitTransportState::Usable, GitConfigState::Managed)
        );
        let value = config_value(&fx).unwrap();
        assert!(is_owned(&value));
        assert!(
            !value.contains("id_ed25519"),
            "the key path stays out of Git config"
        );
        assert!(
            !bind(&fx, GitConfigMode::default(), false).unwrap().changed,
            "idempotent"
        );

        let engine = git_engine_with_store(
            GitCliEngine::default(),
            &fx.paths,
            &fx.state,
            Some(&fx.store),
        );
        assert_ne!(engine, GitCliEngine::default());

        let removed = unbind_transport_with_store(&fx.paths, &fx.state, false).unwrap();
        assert!(removed.was_bound && removed.git_config_removed);
        assert_eq!(config_value(&fx), None);
        assert_eq!(status(&fx).state, GitTransportState::NotBound);
        assert_eq!(
            git_engine_with_store(
                GitCliEngine::default(),
                &fx.paths,
                &fx.state,
                Some(&fx.store)
            ),
            GitCliEngine::default()
        );
    }

    #[test]
    fn opting_out_binds_without_touching_git_config_and_removes_an_owned_value() {
        let fx = fixture("git@forge.example:o/r.git", true);
        let report = bind(&fx, GitConfigMode::Skip, false).unwrap();
        assert!(!report.git_config_written && !report.git_config_removed);
        let state = status(&fx);
        assert_eq!(
            (state.state, state.git_config),
            (GitTransportState::Usable, GitConfigState::NotManaged)
        );
        assert_eq!(config_value(&fx), None);

        // Opting out later removes only what Vulcan wrote.
        bind(&fx, GitConfigMode::Auto, false).unwrap();
        assert!(config_value(&fx).is_some());
        let report = bind(&fx, GitConfigMode::Skip, false).unwrap();
        assert!(report.git_config_removed && report.changed);
        assert_eq!(config_value(&fx), None);
        assert_eq!(
            status(&fx).state,
            GitTransportState::Usable,
            "the binding itself stays"
        );
    }

    #[test]
    fn a_foreign_core_ssh_command_is_left_alone_and_never_blocks_the_bind() {
        let fx = fixture("git@forge.example:o/r.git", true);
        git(
            fx.paths.vault_root(),
            &["config", "core.sshCommand", "ssh -i /mine"],
        );

        let report = bind(&fx, GitConfigMode::Auto, false).unwrap();
        assert!(!report.git_config_written);
        assert!(report
            .git_config_skipped
            .as_deref()
            .unwrap()
            .contains("left alone"));
        let state = status(&fx);
        assert_eq!(
            (state.state, state.git_config),
            (GitTransportState::Usable, GitConfigState::NotManaged)
        );
        assert_eq!(config_value(&fx).as_deref(), Some("ssh -i /mine"));

        // Skip never removes a value it does not own either.
        bind(&fx, GitConfigMode::Skip, false).unwrap();
        assert_eq!(config_value(&fx).as_deref(), Some("ssh -i /mine"));
        unbind_transport_with_store(&fx.paths, &fx.state, false).unwrap();
        assert_eq!(config_value(&fx).as_deref(), Some("ssh -i /mine"));
    }

    #[test]
    fn requiring_the_git_config_fails_on_a_foreign_value_and_changes_nothing() {
        let fx = fixture("git@forge.example:o/r.git", true);
        git(
            fx.paths.vault_root(),
            &["config", "core.sshCommand", "ssh -i /mine"],
        );
        let error = bind(&fx, GitConfigMode::Require, false).unwrap_err();
        assert!(error.to_string().contains("does not own"));
        assert_eq!(status(&fx).state, GitTransportState::NotBound);
        assert_eq!(config_value(&fx).as_deref(), Some("ssh -i /mine"));
    }

    #[test]
    fn a_value_naming_a_missing_executable_is_stale_and_rebinding_repairs_it() {
        let fx = fixture("git@forge.example:o/r.git", true);
        bind(&fx, GitConfigMode::Auto, false).unwrap();
        assert_eq!(status(&fx).git_config, GitConfigState::Managed);

        fs::remove_file(&fx.exe).unwrap();
        assert_eq!(status(&fx).git_config, GitConfigState::Stale);

        // A new executable (an upgrade, a different install path) is adopted by re-binding.
        let moved = fx.dir.path().join("elsewhere").join("vulcan");
        fs::create_dir_all(moved.parent().unwrap()).unwrap();
        fs::write(&moved, "#!/bin/sh\n").unwrap();
        let report = bind_transport_with_store(
            &fx.paths,
            &fx.state,
            &fx.store,
            "origin",
            GitConfigMode::Auto,
            false,
            &moved,
        )
        .unwrap();
        assert!(report.git_config_written);
        assert_eq!(owned_executable(&config_value(&fx).unwrap()), Some(moved));
        assert_eq!(status(&fx).git_config, GitConfigState::Managed);
    }

    /// Runs the stored value the way Git does: through `sh` with the SSH
    /// arguments appended.
    #[cfg(unix)]
    fn run_like_git(value: &str, path: &str) -> (String, String) {
        let output = Command::new("sh")
            .arg("-c")
            .arg(format!("{value} \"$@\""))
            .arg("sh")
            .args(["git@forge.example", "git-upload-pack 'o/r'"])
            .env("PATH", path)
            .output()
            .expect("sh");
        (
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn the_stored_value_runs_vulcan_and_falls_back_to_ssh_when_it_is_gone() {
        let fx = fixture("git@forge.example:o/r.git", true);
        bind(&fx, GitConfigMode::Auto, false).unwrap();
        let value = config_value(&fx).unwrap();

        // A fake `ssh` earlier on PATH proves which program actually ran.
        let bin = fx.dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("ssh"), "#!/bin/sh\necho \"plain-ssh:$*\"\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(bin.join("ssh"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!("{}:/usr/bin:/bin", bin.display());

        // The executable exists (and has a space in its path): vulcan runs with
        // `device ssh-command` followed by Git's SSH arguments.
        let (stdout, _) = run_like_git(&value, &path);
        assert_eq!(
            stdout,
            "vulcan:device ssh-command git@forge.example git-upload-pack 'o/r'"
        );

        // The executable is gone: plain `ssh` runs with the same arguments, so
        // a stale value never leaves `git` worse off than before.
        fs::remove_file(&fx.exe).unwrap();
        let (stdout, _) = run_like_git(&value, &path);
        assert_eq!(stdout, "plain-ssh:git@forge.example git-upload-pack 'o/r'");
    }

    #[test]
    fn bind_rejects_non_ssh_remotes_and_uninitialized_identity() {
        let https = fixture("https://forge.example/o/r.git", true);
        assert!(bind(&https, GitConfigMode::Auto, false)
            .unwrap_err()
            .to_string()
            .contains("not an SSH remote"));
        assert_eq!(config_value(&https), None);

        let bare = fixture("git@forge.example:o/r.git", false);
        let error = bind(&bare, GitConfigMode::Auto, false).unwrap_err();
        assert!(error.to_string().contains("vulcan device init"));
        assert!(
            bare.store.inspect().device_id.is_none(),
            "bind must not initialize identity"
        );
        assert_eq!(
            config_value(&bare),
            None,
            "a failed bind writes no Git config"
        );
    }

    #[test]
    fn bound_vault_fails_closed_when_the_device_key_is_unusable() {
        let fx = fixture("git@forge.example:o/r.git", true);
        bind(&fx, GitConfigMode::Auto, false).unwrap();
        fs::remove_file(fx.store.private_key_path().unwrap()).unwrap();

        let state = status(&fx);
        assert_eq!(state.state, GitTransportState::DeviceKeyUnavailable);
        assert!(state.diagnostic.is_some());
        // The engine still builds, with a transport that cannot authenticate
        // and cannot silently use another key.
        let engine = git_engine_with_store(
            GitCliEngine::default(),
            &fx.paths,
            &fx.state,
            Some(&fx.store),
        );
        assert_ne!(engine, GitCliEngine::default());
    }

    #[test]
    fn binding_state_never_lands_in_the_work_tree() {
        let fx = fixture("git@forge.example:o/r.git", true);
        bind(&fx, GitConfigMode::Auto, false).unwrap();
        // Plain Git vaults replicate everything in the work tree, so the
        // binding must live in device-local state, and `.vulcan/` must not
        // even be created for it. (core.sshCommand lives in `.git/config`,
        // which is never replicated.)
        assert!(!fx.paths.vault_root().join(".vulcan").exists());
        let entries = fs::read_dir(fx.paths.vault_root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [std::ffi::OsString::from(".git")]);
        assert!(fx
            .state
            .vault_local_dir(&fx.paths)
            .join(BINDING_FILE)
            .is_file());
    }
}
