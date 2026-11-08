//! `vulcan daemon plan` and the device-side steps of `vulcan daemon apply`
//! (`docs/specs/declarative-device-config.md`).
//!
//! A plan compares what `daemon.toml` declares with this device: stored
//! identities, vault directories, the daemon service, a running daemon, and
//! Vulcan state that no declaration owns any more (drift). Planning never
//! writes anything. Cloning and service management need the CLI's transport
//! and process helpers, so the CLI drives `apply` from the plan; this module
//! provides the plan and the pruning of drift.

use crate::api_tokens::ApiTokenStore;
use crate::mcp_remote::McpRemoteAuthentication;
use crate::mcp_state::McpAuthorizationStore;
use crate::process::{DaemonProcessContext, DaemonRuntimeRecord};
use crate::registry::{DaemonConfig, DeclaredConfig, WikiRegistration};
use crate::service::DaemonServiceDiagnostic;
use crate::vault_remotes::VaultRemoteState;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;
use vulcan_secrets::{ProtectedFileSecretStore, SecretName, SecretStore};

/// What `apply` would do with one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    Ok,
    /// Skipped on request (`--no-service`) or unsupported here.
    Skip,
    /// Undeclared Vulcan state; removed only with `--prune`.
    Drift,
    Create,
    Update,
    Clone,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    /// The file itself, or one top-level section of it.
    Config,
    Vault,
    McpRemote,
    Identity,
    Service,
    Daemon,
    ScanState,
    ApiToken,
    OauthState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanItem {
    pub kind: PlanKind,
    pub id: String,
    pub action: PlanAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PlanItem {
    fn new(kind: PlanKind, id: impl Into<String>, action: PlanAction) -> Self {
        Self {
            kind,
            id: id.into(),
            action,
            reason: None,
        }
    }

    fn because(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DevicePlan {
    /// The file the plan was made from.
    pub file: PathBuf,
    /// Whether that file is the live `daemon.toml` (not a `--file` review).
    pub live: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_by: Option<String>,
    pub items: Vec<PlanItem>,
}

impl DevicePlan {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.action == PlanAction::Error)
    }

    /// An error that must stop `apply` before any change. A vault whose
    /// directory cannot be cloned or checked is reported but does not stop
    /// the other vaults.
    #[must_use]
    pub fn blocking_errors(&self) -> Vec<&PlanItem> {
        self.items
            .iter()
            .filter(|item| item.action == PlanAction::Error && item.kind != PlanKind::Vault)
            .collect()
    }

    /// Whether `apply` would change anything. Drift counts only when
    /// `prune` is requested.
    #[must_use]
    pub fn has_changes(&self, prune: bool) -> bool {
        self.items.iter().any(|item| match item.action {
            PlanAction::Create | PlanAction::Update | PlanAction::Clone => true,
            PlanAction::Drift => prune,
            PlanAction::Ok | PlanAction::Skip | PlanAction::Error => false,
        })
    }

    #[must_use]
    pub fn items_of(&self, kind: PlanKind, action: PlanAction) -> Vec<&PlanItem> {
        self.items
            .iter()
            .filter(|item| item.kind == kind && item.action == action)
            .collect()
    }
}

/// How the daemon service looks on this device.
#[derive(Debug, Clone)]
pub enum ServiceCheck {
    /// `--no-service`: neither the service nor the daemon process is managed.
    Skipped,
    /// No native per-user service on this platform.
    Unsupported,
    Inspected(Option<DaemonServiceDiagnostic>),
}

pub struct PlanRequest<'a> {
    /// The file under review (`--file`); `None` plans the live `daemon.toml`.
    pub file: Option<&'a Path>,
    pub service: ServiceCheck,
    /// The executable `apply` runs as, which the service should start.
    pub executable: &'a Path,
}

/// Digest of the configuration a running daemon reads only at start.
/// Vault declarations and permission profiles are excluded: the daemon and
/// `vulcan-core` reread them while running.
#[must_use]
pub fn restart_digest(config: &DaemonConfig) -> String {
    let mut fixed = config.clone();
    fixed.vaults.clear();
    fixed.permissions = None;
    fixed.managed_by = None;
    let serialized = serde_json::to_vec(&fixed).unwrap_or_default();
    blake3::hash(&serialized).to_hex().to_string()
}

#[must_use]
pub fn modified_unix_ms(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let elapsed = modified.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(elapsed.as_millis()).ok()
}

/// Compares the declared configuration with this device. Never writes.
pub fn plan_device(context: &DaemonProcessContext, request: &PlanRequest<'_>) -> DevicePlan {
    let live_path = context.registry.path().to_path_buf();
    let file = request
        .file
        .map_or_else(|| live_path.clone(), Path::to_path_buf);
    let mut plan = DevicePlan {
        file: file.clone(),
        live: request.file.is_none(),
        managed_by: None,
        items: Vec::new(),
    };
    let source = match request.file {
        Some(path) => match fs::read_to_string(path) {
            Ok(source) => Some(source),
            Err(error) => {
                plan.items.push(
                    PlanItem::new(
                        PlanKind::Config,
                        file.display().to_string(),
                        PlanAction::Error,
                    )
                    .because(format!("cannot read: {error}")),
                );
                return plan;
            }
        },
        None => None,
    };
    let declared = match context.registry.declared(source.as_deref()) {
        Ok(declared) => declared,
        Err(error) => {
            plan.items.push(
                PlanItem::new(
                    PlanKind::Config,
                    file.display().to_string(),
                    PlanAction::Error,
                )
                .because(error.to_string()),
            );
            return plan;
        }
    };
    plan.managed_by.clone_from(&declared.config.managed_by);
    plan.items.push(PlanItem::new(
        PlanKind::Config,
        file.display().to_string(),
        PlanAction::Ok,
    ));
    let live = if request.file.is_some() {
        context.registry.declared(None).ok().map(|live| live.config)
    } else {
        None
    };
    if let Some(live) = &live {
        plan.items.extend(section_changes(live, &declared.config));
    }
    plan.items.extend(duplicate_paths(&declared.config));
    plan.items.extend(identity_items(&declared));
    let vault_items = declared
        .config
        .vaults
        .iter()
        .map(|wiki| vault_item(wiki, live.as_ref()))
        .collect::<Vec<_>>();
    let cloning = vault_items
        .iter()
        .filter(|item| item.action == PlanAction::Clone)
        .map(|item| item.id.clone())
        .collect::<BTreeSet<_>>();
    plan.items.extend(vault_items);
    let check_remotes = || remote_items(&declared.config, live.as_ref(), &cloning);
    let remote_items = match request.file {
        Some(path) => vulcan_core::config::with_daemon_config_file(path, check_remotes),
        None => check_remotes(),
    };
    plan.items.extend(remote_items);
    plan.items
        .extend(service_items(context, request, &declared.config));
    plan.items.extend(drift_items(context, &declared));
    plan
}

/// Top-level sections a reviewed file changes relative to the live one.
fn section_changes(live: &DaemonConfig, desired: &DaemonConfig) -> Vec<PlanItem> {
    let as_table = |config: &DaemonConfig| {
        let mut value =
            toml::Value::try_from(config).unwrap_or(toml::Value::Table(toml::map::Map::new()));
        if let Some(table) = value.as_table_mut() {
            table.remove("vault");
            table.remove("mcp_remote");
        }
        value.as_table().cloned().unwrap_or_default()
    };
    let live = as_table(live);
    let desired = as_table(desired);
    let keys = live
        .keys()
        .chain(desired.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter_map(|key| match (live.get(&key), desired.get(&key)) {
            (Some(old), Some(new)) if old == new => None,
            (None, Some(_)) => Some(
                PlanItem::new(PlanKind::Config, key, PlanAction::Create)
                    .because("declared in the reviewed file"),
            ),
            (Some(_), None) => Some(
                PlanItem::new(PlanKind::Config, key, PlanAction::Update)
                    .because("removed in the reviewed file"),
            ),
            _ => Some(
                PlanItem::new(PlanKind::Config, key, PlanAction::Update)
                    .because("changed in the reviewed file"),
            ),
        })
        .collect()
}

fn duplicate_paths(config: &DaemonConfig) -> Vec<PlanItem> {
    let mut seen = Vec::<(PathBuf, &str)>::new();
    let mut items = Vec::new();
    for wiki in &config.vaults {
        let path = fs::canonicalize(&wiki.path).unwrap_or_else(|_| wiki.path.clone());
        if let Some((_, other)) = seen.iter().find(|(existing, _)| existing == &path) {
            items.push(
                PlanItem::new(
                    PlanKind::Config,
                    format!("vault.{}", wiki.id),
                    PlanAction::Error,
                )
                .because(format!(
                    "path {} is also declared by vault `{other}`",
                    wiki.path.display()
                )),
            );
        } else {
            seen.push((path, wiki.id.as_str()));
        }
    }
    items
}

fn identity_items(declared: &DeclaredConfig) -> Vec<PlanItem> {
    let mut items = Vec::new();
    for id in &declared.new_vault_identities {
        items.push(
            PlanItem::new(
                PlanKind::Identity,
                format!("vault.{id}"),
                PlanAction::Create,
            )
            .because("new registration identity"),
        );
    }
    for id in &declared.new_remote_identities {
        items.push(
            PlanItem::new(
                PlanKind::Identity,
                format!("mcp_remote.{id}"),
                PlanAction::Create,
            )
            .because("new instance identity"),
        );
    }
    for id in declared.stored_identities.vaults.keys() {
        if !declared
            .config
            .vaults
            .iter()
            .any(|wiki| wiki.id.as_str() == id)
        {
            items.push(
                PlanItem::new(PlanKind::Identity, format!("vault.{id}"), PlanAction::Drift)
                    .because("no longer declared; declaring it again restores it"),
            );
        }
    }
    for id in declared.stored_identities.mcp_remotes.keys() {
        if !declared
            .config
            .mcp_remotes
            .iter()
            .any(|remote| remote.id.as_str() == id)
        {
            items.push(
                PlanItem::new(
                    PlanKind::Identity,
                    format!("mcp_remote.{id}"),
                    PlanAction::Drift,
                )
                .because("no longer declared; declaring it again restores it"),
            );
        }
    }
    items
}

fn vault_item(wiki: &WikiRegistration, live: Option<&DaemonConfig>) -> PlanItem {
    let id = wiki.id.to_string();
    let directory = wiki.work_tree();
    if !wiki.path.is_dir() {
        return match &wiki.source {
            Some(source) if !directory.exists() => {
                PlanItem::new(PlanKind::Vault, id, PlanAction::Clone)
                    .because(format!("clone {source} into {}", directory.display()))
            }
            Some(_) => PlanItem::new(PlanKind::Vault, id, PlanAction::Error).because(format!(
                "{} exists but is not the vault directory {}",
                directory.display(),
                wiki.path.display()
            )),
            None => PlanItem::new(PlanKind::Vault, id, PlanAction::Error).because(format!(
                "{} is missing; add `source` to clone it, or create it",
                wiki.path.display()
            )),
        };
    }
    if let Some(source) = &wiki.source {
        match git_remote_urls(directory, wiki.git_dir.as_deref()) {
            Ok(urls) if urls.iter().any(|url| same_git_url(url, source)) => {}
            Ok(urls) if urls.is_empty() => {
                return PlanItem::new(PlanKind::Vault, id, PlanAction::Error).because(format!(
                    "{} has no Git remote, but `source` is {source}",
                    directory.display()
                ));
            }
            Ok(urls) => {
                return PlanItem::new(PlanKind::Vault, id, PlanAction::Error).because(format!(
                    "{} has remote {}, but `source` is {source}",
                    directory.display(),
                    urls.join(", ")
                ));
            }
            Err(error) => {
                return PlanItem::new(PlanKind::Vault, id, PlanAction::Error).because(format!(
                    "cannot check the Git remote of {}: {error}",
                    directory.display()
                ));
            }
        }
    }
    match live.map(|live| live.vaults.iter().find(|entry| entry.id == wiki.id)) {
        Some(None) => PlanItem::new(PlanKind::Vault, id, PlanAction::Create)
            .because("declared in the reviewed file"),
        Some(Some(existing)) if !same_declaration(existing, wiki) => {
            PlanItem::new(PlanKind::Vault, id, PlanAction::Update)
                .because("changed in the reviewed file")
        }
        _ => PlanItem::new(PlanKind::Vault, id, PlanAction::Ok),
    }
}

fn same_declaration(left: &WikiRegistration, right: &WikiRegistration) -> bool {
    let mut left = left.clone();
    left.registration_id = right.registration_id;
    &left == right
}

fn remote_items(
    config: &DaemonConfig,
    live: Option<&DaemonConfig>,
    cloning: &BTreeSet<String>,
) -> Vec<PlanItem> {
    let mut items = Vec::new();
    for remote in &config.mcp_remotes {
        let id = remote.id.to_string();
        let mut problems = Vec::new();
        let mut notes = Vec::new();
        vault_remote_notes(config, &id, &mut problems, &mut notes);
        for exposed in &remote.vaults {
            let Some(wiki) = config.vaults.iter().find(|wiki| wiki.id == exposed.wiki_id) else {
                continue;
            };
            if cloning.contains(wiki.id.as_str()) || !wiki.path.is_dir() {
                notes.push(format!(
                    "ceiling `{}` for `{}` is checked once the vault exists",
                    exposed.ceiling_profile, wiki.id
                ));
                continue;
            }
            let paths = vulcan_core::VaultPaths::new(&wiki.path);
            if let Err(error) = vulcan_core::resolve_device_permission_profile(
                &paths,
                Some(&exposed.ceiling_profile),
            ) {
                problems.push(format!(
                    "ceiling `{}` for `{}`: {error}",
                    exposed.ceiling_profile, wiki.id
                ));
                continue;
            }
            if local_profile_defined(&paths, &exposed.ceiling_profile) {
                notes.push(format!(
                    "ceiling `{0}` is defined in {1}'s config.local.toml; `vulcan --vault {2} config permissions profile move {0}` moves it to daemon.toml",
                    exposed.ceiling_profile,
                    wiki.id,
                    wiki.path.display()
                ));
            }
        }
        if !problems.is_empty() {
            items.push(
                PlanItem::new(PlanKind::McpRemote, id, PlanAction::Error)
                    .because(problems.join("; ")),
            );
            continue;
        }
        let item = {
            let action = match live.map(|live| {
                live.mcp_remotes
                    .iter()
                    .find(|existing| existing.id == remote.id)
            }) {
                Some(None) => PlanAction::Create,
                Some(Some(existing)) => {
                    let mut existing = existing.clone();
                    existing.instance_id = remote.instance_id;
                    if &existing == remote {
                        PlanAction::Ok
                    } else {
                        PlanAction::Update
                    }
                }
                None => PlanAction::Ok,
            };
            let item = PlanItem::new(PlanKind::McpRemote, id, action);
            if notes.is_empty() {
                item
            } else {
                item.because(notes.join("; "))
            }
        };
        items.push(item);
    }
    items.extend(unserved_vault_remotes(config, cloning));
    items
}

/// Where a remote from a vault's `[mcp_remote]` comes from, and whether its
/// `IndieAuth` identity waits for acceptance on this device.
fn vault_remote_notes(
    config: &DaemonConfig,
    id: &str,
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let Some(status) = config.vault_remotes.iter().find(|status| status.id == id) else {
        return;
    };
    match status.state {
        VaultRemoteState::Served => {
            notes.push(format!("from vault `{id}`'s [mcp_remote]"));
            let identity = config
                .mcp_remotes
                .iter()
                .find(|remote| remote.id.as_str() == id)
                .map(|remote| match &remote.authentication {
                    McpRemoteAuthentication::IndieAuth { identity } => identity.clone(),
                })
                .unwrap_or_default();
            if let Some(asked) = &status.pending_identity {
                problems.push(format!(
                    "the vault now asks for IndieAuth identity {asked}, but this device accepted {identity}; the remote keeps {identity} until you run `vulcan daemon apply --accept-remote {id}`"
                ));
            } else if status.first_acceptance {
                notes.push(format!(
                    "accepts IndieAuth identity {identity} on this device"
                ));
            }
        }
        VaultRemoteState::Overridden => {
            if let Some(note) = &status.note {
                notes.push(note.clone());
            }
        }
        VaultRemoteState::VaultMissing | VaultRemoteState::Invalid => {}
    }
}

fn unserved_vault_remotes(config: &DaemonConfig, cloning: &BTreeSet<String>) -> Vec<PlanItem> {
    config
        .vault_remotes
        .iter()
        .filter_map(|status| {
            let note = status.note.clone().unwrap_or_default();
            let item = match status.state {
                VaultRemoteState::Served | VaultRemoteState::Overridden => return None,
                VaultRemoteState::VaultMissing if cloning.contains(&status.id) => {
                    PlanItem::new(PlanKind::McpRemote, &status.id, PlanAction::Create)
                        .because("from the vault's [mcp_remote], once it is cloned")
                }
                VaultRemoteState::VaultMissing => {
                    PlanItem::new(PlanKind::McpRemote, &status.id, PlanAction::Skip).because(note)
                }
                VaultRemoteState::Invalid => {
                    PlanItem::new(PlanKind::McpRemote, &status.id, PlanAction::Error).because(note)
                }
            };
            Some(item)
        })
        .collect()
}

fn local_profile_defined(paths: &vulcan_core::VaultPaths, name: &str) -> bool {
    fs::read_to_string(paths.local_config_file())
        .ok()
        .and_then(|source| toml::from_str::<toml::Value>(&source).ok())
        .and_then(|value| {
            value
                .get("permissions")?
                .get("profiles")?
                .get(name)
                .map(|_| ())
        })
        .is_some()
}

fn service_items(
    context: &DaemonProcessContext,
    request: &PlanRequest<'_>,
    config: &DaemonConfig,
) -> Vec<PlanItem> {
    let mut items = Vec::new();
    let mut service_restarts = false;
    match &request.service {
        ServiceCheck::Skipped => items.push(
            PlanItem::new(PlanKind::Service, "daemon", PlanAction::Skip).because("--no-service"),
        ),
        ServiceCheck::Unsupported | ServiceCheck::Inspected(None) => items.push(
            PlanItem::new(PlanKind::Service, "daemon", PlanAction::Skip)
                .because("no native per-user service on this platform"),
        ),
        ServiceCheck::Inspected(Some(diagnostic)) if !diagnostic.installed => {
            service_restarts = true;
            items.push(
                PlanItem::new(PlanKind::Service, "daemon", PlanAction::Create)
                    .because(format!("install {}", diagnostic.definition_path.display())),
            );
        }
        ServiceCheck::Inspected(Some(diagnostic)) if !diagnostic.definition_current => {
            service_restarts = true;
            let reason = match &diagnostic.configured_executable {
                Some(executable) if executable != request.executable => format!(
                    "the service runs {}; update it to {}",
                    executable.display(),
                    request.executable.display()
                ),
                _ => format!("update {}", diagnostic.definition_path.display()),
            };
            items.push(
                PlanItem::new(PlanKind::Service, "daemon", PlanAction::Update).because(reason),
            );
        }
        ServiceCheck::Inspected(Some(_)) => {
            items.push(PlanItem::new(PlanKind::Service, "daemon", PlanAction::Ok));
        }
    }
    let managed = !matches!(request.service, ServiceCheck::Skipped);
    let item = match crate::process::running_daemon(context).as_ref() {
        Some(record) => {
            let stale = stale_reasons(record, config);
            if stale.is_empty() {
                PlanItem::new(PlanKind::Daemon, "process", PlanAction::Ok)
            } else if managed {
                PlanItem::new(PlanKind::Daemon, "process", PlanAction::Update)
                    .because(format!("{}; apply restarts it", stale.join("; ")))
            } else {
                PlanItem::new(PlanKind::Daemon, "process", PlanAction::Skip).because(format!(
                    "{}; restart it yourself (--no-service)",
                    stale.join("; ")
                ))
            }
        }
        None if service_restarts => PlanItem::new(PlanKind::Daemon, "process", PlanAction::Ok)
            .because("started by the service"),
        None if managed
            && !matches!(
                request.service,
                ServiceCheck::Inspected(None) | ServiceCheck::Unsupported
            ) =>
        {
            PlanItem::new(PlanKind::Daemon, "process", PlanAction::Create)
                .because("not running; apply starts it")
        }
        None => PlanItem::new(PlanKind::Daemon, "process", PlanAction::Skip).because("not running"),
    };
    items.push(item);
    items
}

/// Why `record` is not yet the daemon `apply` waits for after a restart: a
/// process other than `previous` that runs the current executable and
/// configuration. Empty when it is.
#[must_use]
pub fn restart_pending_reasons(
    previous: Option<&DaemonRuntimeRecord>,
    record: &DaemonRuntimeRecord,
    config: &DaemonConfig,
) -> Vec<String> {
    let same_process = previous.is_some_and(|previous| {
        previous.pid == record.pid && previous.started_unix_ms == record.started_unix_ms
    });
    if same_process {
        return vec![format!(
            "the previous daemon (pid {}) is still running",
            record.pid
        )];
    }
    stale_reasons(record, config)
}

fn stale_reasons(record: &DaemonRuntimeRecord, config: &DaemonConfig) -> Vec<String> {
    let mut reasons = Vec::new();
    match &record.config_digest {
        Some(digest) if *digest == restart_digest(config) => {}
        Some(_) => reasons.push("running with an older configuration".to_string()),
        None => reasons.push("started by an older Vulcan".to_string()),
    }
    if let (Some(executable), Some(modified)) =
        (&record.executable, record.executable_modified_unix_ms)
    {
        if modified_unix_ms(executable) != Some(modified) {
            reasons.push(format!("{} was replaced", executable.display()));
        }
    }
    reasons
}

/// Vulcan state that belongs to no declared vault or remote.
fn drift_items(context: &DaemonProcessContext, declared: &DeclaredConfig) -> Vec<PlanItem> {
    let mut items = Vec::new();
    let registrations = declared
        .config
        .vaults
        .iter()
        .map(|wiki| wiki.registration_id.to_string().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let instances = declared
        .config
        .mcp_remotes
        .iter()
        .map(|remote| remote.instance_id.to_string().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for path in scan_state_files(&context.state_root) {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !registrations.contains(&stem) {
            items.push(
                PlanItem::new(
                    PlanKind::ScanState,
                    path.display().to_string(),
                    PlanAction::Drift,
                )
                .because("scan state of an undeclared vault"),
            );
        }
    }
    if let Ok(tokens) = ApiTokenStore::at(&context.state_root).list() {
        for token in tokens {
            if !registrations.contains(&token.registration_id.to_string().to_ascii_lowercase()) {
                items.push(
                    PlanItem::new(PlanKind::ApiToken, token.id.to_string(), PlanAction::Drift)
                        .because(format!(
                            "API token for undeclared vault `{}`",
                            token.wiki_id
                        )),
                );
            }
        }
    }
    let grants = McpAuthorizationStore::at(&context.state_root);
    if grants.path().exists() {
        if let Ok(reports) = grants.list_grants(None) {
            for grant in reports {
                if grant.revoked_at.is_none()
                    && !instances
                        .contains(&grant.remote_instance_id.to_string().to_ascii_lowercase())
                {
                    items.push(
                        PlanItem::new(
                            PlanKind::OauthState,
                            format!("grant {}", grant.id),
                            PlanAction::Drift,
                        )
                        .because(format!(
                            "connection grant of undeclared remote `{}`",
                            grant.remote_id
                        )),
                    );
                }
            }
        }
    }
    for path in oauth_secret_files(&context.state_root) {
        let instance = secret_instance(&path).unwrap_or_default();
        if !instances.contains(&instance) {
            items.push(
                PlanItem::new(
                    PlanKind::OauthState,
                    path.display().to_string(),
                    PlanAction::Drift,
                )
                .because("OAuth key of an undeclared remote"),
            );
        }
    }
    if let Ok(entries) = fs::read_dir(context.state_root.join("mcp-remotes")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !declared
                .config
                .mcp_remotes
                .iter()
                .any(|remote| remote.id.as_str() == name)
            {
                items.push(
                    PlanItem::new(
                        PlanKind::OauthState,
                        entry.path().display().to_string(),
                        PlanAction::Drift,
                    )
                    .because("runtime state of an undeclared remote"),
                );
            }
        }
    }
    items
}

fn scan_state_files(state_root: &Path) -> Vec<PathBuf> {
    let directory = state_root.join("sync/repositories/daemon/scans");
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// `mcp-<instance>.<kind>.secret` files of named remotes.
fn oauth_secret_files(state_root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for directory in ["secrets", "mcp-credential-bindings"] {
        let Ok(entries) = fs::read_dir(state_root.join(directory)) else {
            continue;
        };
        files.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| secret_instance(path).is_some()),
        );
    }
    files.sort();
    files
}

fn secret_instance(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix("mcp-")?;
    let (instance, _) = rest.split_once('.')?;
    (instance.len() == 26).then(|| instance.to_ascii_lowercase())
}

/// One removal `apply --prune` made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PruneOutcome {
    pub kind: PlanKind,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Removes the drift a plan listed. Only Vulcan state goes; working trees
/// and Git directories are never touched.
#[must_use]
pub fn prune_drift(context: &DaemonProcessContext, plan: &DevicePlan) -> Vec<PruneOutcome> {
    let mut outcomes = Vec::new();
    let mut vault_ids = Vec::new();
    let mut remote_ids = Vec::new();
    let mut undeclared_grants = BTreeSet::new();
    for item in plan
        .items
        .iter()
        .filter(|item| item.action == PlanAction::Drift)
    {
        let result: Result<(), String> = match item.kind {
            PlanKind::Identity => {
                if let Some(id) = item.id.strip_prefix("vault.") {
                    vault_ids.push(id.to_string());
                } else if let Some(id) = item.id.strip_prefix("mcp_remote.") {
                    remote_ids.push(id.to_string());
                }
                continue;
            }
            PlanKind::ScanState => fs::remove_file(&item.id).map_err(|error| error.to_string()),
            PlanKind::ApiToken => item
                .id
                .parse()
                .map_err(|error: ulid::DecodeError| error.to_string())
                .and_then(|id| {
                    ApiTokenStore::at(&context.state_root)
                        .revoke(id, false)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                }),
            PlanKind::OauthState => {
                if let Some(grant) = item.id.strip_prefix("grant ") {
                    undeclared_grants.insert(grant.to_string());
                    continue;
                }
                remove_oauth_path(Path::new(&item.id))
            }
            _ => continue,
        };
        outcomes.push(PruneOutcome {
            kind: item.kind,
            id: item.id.clone(),
            error: result.err(),
        });
    }
    if !undeclared_grants.is_empty() {
        outcomes.extend(revoke_undeclared_grants(context, &undeclared_grants));
    }
    if !vault_ids.is_empty() || !remote_ids.is_empty() {
        let result = context
            .registry
            .forget_undeclared_identities(&vault_ids, &remote_ids)
            .map_err(|error| error.to_string());
        for id in vault_ids
            .iter()
            .map(|id| format!("vault.{id}"))
            .chain(remote_ids.iter().map(|id| format!("mcp_remote.{id}")))
        {
            outcomes.push(PruneOutcome {
                kind: PlanKind::Identity,
                id,
                error: result.clone().err(),
            });
        }
    }
    outcomes
}

fn revoke_undeclared_grants(
    context: &DaemonProcessContext,
    grants: &BTreeSet<String>,
) -> Vec<PruneOutcome> {
    let store = McpAuthorizationStore::at(&context.state_root);
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    grants
        .iter()
        .map(|grant| {
            let error = grant
                .parse()
                .map_err(|error: ulid::DecodeError| error.to_string())
                .and_then(|id| {
                    store
                        .revoke_grant(id, now, false)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .err();
            PruneOutcome {
                kind: PlanKind::OauthState,
                id: format!("grant {grant}"),
                error,
            }
        })
        .collect()
}

fn remove_oauth_path(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        return fs::remove_dir_all(path).map_err(|error| error.to_string());
    }
    let (Some(directory), Some(name)) = (
        path.parent(),
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".secret")),
    ) else {
        return fs::remove_file(path).map_err(|error| error.to_string());
    };
    let name = SecretName::parse(name).map_err(|error| error.to_string())?;
    ProtectedFileSecretStore::at(directory)
        .delete(&name)
        .map_err(|error| error.to_string())
}

/// URLs of the Git remotes of a work tree.
fn git_remote_urls(work_tree: &Path, git_dir: Option<&Path>) -> Result<Vec<String>, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(work_tree);
    if let Some(git_dir) = git_dir {
        command.arg("--git-dir").arg(git_dir);
    }
    let output = command
        .args(["config", "--get-regexp", r"^remote\..*\.url$"])
        .output()
        .map_err(|error| error.to_string())?;
    // Exit code 1 means no remote is configured.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' ').map(|(_, url)| url.trim().to_string()))
        .collect())
}

/// Whether two Git URLs name the same repository, ignoring a trailing
/// `.git` or slash.
#[must_use]
pub fn same_git_url(left: &str, right: &str) -> bool {
    let normalize = |url: &str| {
        let url = url.trim().trim_end_matches('/');
        url.strip_suffix(".git").unwrap_or(url).to_string()
    };
    normalize(left) == normalize(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_urls_compare_without_suffixes() {
        assert!(same_git_url(
            "git@forge.example:eric/grim.git",
            "git@forge.example:eric/grim"
        ));
        assert!(same_git_url("https://h/x/", "https://h/x.git"));
        assert!(!same_git_url("git@h:a/b", "git@h:a/c"));
    }

    #[test]
    fn secret_files_name_their_instance() {
        assert_eq!(
            secret_instance(Path::new("/s/mcp-01m6sqppjpnj0tv6rmzxdckmxj.issuer.secret")),
            Some("01m6sqppjpnj0tv6rmzxdckmxj".to_string())
        );
        assert_eq!(secret_instance(Path::new("/s/other.secret")), None);
    }

    #[test]
    fn a_restart_is_done_once_a_new_process_runs_the_current_config() {
        let config = DaemonConfig::default();
        let record = |pid, started_unix_ms, digest: Option<&str>| DaemonRuntimeRecord {
            version: 1,
            pid,
            bind: "127.0.0.1:3210".parse().unwrap(),
            started_unix_ms,
            credential_id: "credential".to_string(),
            executable: None,
            executable_modified_unix_ms: None,
            config_digest: digest.map(ToString::to_string),
        };
        let current = restart_digest(&config);
        let old = record(10, 1, Some("old"));

        let still_old = restart_pending_reasons(Some(&old), &record(10, 1, Some("old")), &config);
        assert!(still_old[0].contains("still running"), "{still_old:?}");
        let stale = restart_pending_reasons(Some(&old), &record(11, 2, Some("old")), &config);
        assert!(stale[0].contains("older configuration"), "{stale:?}");
        assert!(
            restart_pending_reasons(Some(&old), &record(11, 2, Some(&current)), &config).is_empty()
        );
        assert!(restart_pending_reasons(None, &record(10, 1, Some(&current)), &config).is_empty());
    }

    #[test]
    fn the_restart_digest_ignores_what_the_daemon_rereads() {
        let mut config = DaemonConfig::default();
        let digest = restart_digest(&config);
        config.managed_by = Some("chezmoi".to_string());
        config.permissions = Some(crate::registry::OpaqueToml(toml::Value::Table(
            toml::map::Map::new(),
        )));
        assert_eq!(restart_digest(&config), digest);
        config.bind = "127.0.0.1:4000".to_string();
        assert_ne!(restart_digest(&config), digest);
    }

    #[test]
    fn changes_count_drift_only_when_pruning() {
        let plan = DevicePlan {
            file: PathBuf::from("daemon.toml"),
            live: true,
            managed_by: None,
            items: vec![PlanItem::new(PlanKind::ApiToken, "x", PlanAction::Drift)],
        };
        assert!(!plan.has_changes(false));
        assert!(plan.has_changes(true));
    }
}
