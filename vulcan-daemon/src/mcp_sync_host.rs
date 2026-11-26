//! The [`McpSyncHost`] behind the MCP `sync_refresh` and `sync_resume` tools.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use vulcan_app::mcp_sync::{McpSyncHooks, McpSyncHost};
use vulcan_app::sync::GitSyncOptions;
use vulcan_sync::SyncJobTrigger;

use crate::companion::SyncSelectionRequest;
use crate::process::{request_queued_sync, DaemonProcessContext, QueuedSyncError};
use crate::registry::{UpdateWikiRequest, WikiRegistration, WikiRegistry};
use crate::supervisor::SyncSupervisor;
use crate::sync::{sync_registered_wiki_observed, sync_registered_wikis, RegisteredSyncSelection};

/// Idempotency scope of jobs MCP tools queue.
const MCP_SCOPE: &str = "mcp";

/// Starts registered vaults' synchronization for MCP tools: on the daemon's
/// own supervisor inside the daemon, through the running daemon from a
/// separate `vulcan mcp`, and directly when no daemon runs.
pub struct RegistrySyncHost {
    registry: WikiRegistry,
    target: Target,
}

enum Target {
    Supervisor(Arc<SyncSupervisor>),
    Process(DaemonProcessContext),
}

impl RegistrySyncHost {
    /// The host for MCP sessions served inside the daemon.
    #[must_use]
    pub fn in_daemon(registry: WikiRegistry, supervisor: Arc<SyncSupervisor>) -> Self {
        Self {
            registry,
            target: Target::Supervisor(supervisor),
        }
    }

    /// The host for a separate `vulcan mcp` process.
    #[must_use]
    pub fn in_process(context: DaemonProcessContext) -> Self {
        Self {
            registry: context.registry.clone(),
            target: Target::Process(context),
        }
    }

    fn sync(
        &self,
        vault_root: &Path,
        hooks: Option<&mut McpSyncHooks<'_>>,
    ) -> Result<Value, String> {
        let registration = self.registration(vault_root)?;
        let key = ulid::Ulid::new().to_string();
        match &self.target {
            Target::Supervisor(supervisor) => {
                let report = supervisor
                    .enqueue_idempotent(
                        MCP_SCOPE,
                        &key,
                        registration.id.as_str(),
                        registration.path.clone(),
                        SyncJobTrigger::Manual,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(json!({ "queued": true, "job": report }))
            }
            Target::Process(context) => {
                let selection = SyncSelectionRequest {
                    wiki: Some(registration.id.as_str().to_string()),
                    group: None,
                    all: false,
                };
                match request_queued_sync(context, &selection, &key) {
                    Ok(report) => Ok(json!({ "queued": true, "jobs": report })),
                    Err(QueuedSyncError::NotRunning) => {
                        let report = match hooks {
                            Some(hooks) => {
                                let cancellation = vulcan_sync::SyncCancellationToken::default();
                                let mut observer = PhaseObserver {
                                    hooks,
                                    cancellation: &cancellation,
                                };
                                sync_registered_wiki_observed(
                                    &self.registry,
                                    &registration.id,
                                    &GitSyncOptions::default(),
                                    &mut observer,
                                    &cancellation,
                                )
                            }
                            None => sync_registered_wikis(
                                &self.registry,
                                &RegisteredSyncSelection::Wiki(registration.id.clone()),
                                &GitSyncOptions::default(),
                                None,
                            ),
                        }
                        .map_err(|error| error.to_string())?;
                        Ok(json!({ "queued": false, "report": report }))
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
        }
    }

    fn registration(&self, vault_root: &Path) -> Result<WikiRegistration, String> {
        self.registry.find_by_path(vault_root).map_err(|error| {
            format!(
                "this vault is not a registered wiki, so Vulcan does not synchronize it: {error}"
            )
        })
    }
}

/// Reports a direct cycle's phases to a `sync_refresh` call, and stops the
/// cycle at the engine's next safe point once the call is cancelled.
struct PhaseObserver<'a, 'h> {
    hooks: &'a mut McpSyncHooks<'h>,
    cancellation: &'a vulcan_sync::SyncCancellationToken,
}

impl vulcan_sync::GitSyncObserver for PhaseObserver<'_, '_> {
    fn progress(
        &mut self,
        progress: &vulcan_sync::GitSyncProgress,
    ) -> Result<(), vulcan_sync::GitSyncObserverError> {
        let phase = serde_json::to_value(progress.phase)
            .ok()
            .and_then(|phase| phase.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{:?}", progress.phase));
        (self.hooks.on_phase)(&phase);
        if (self.hooks.cancelled)() {
            self.cancellation.cancel();
        }
        Ok(())
    }
}

impl McpSyncHost for RegistrySyncHost {
    fn sync_now(&self, vault_root: &Path) -> Result<Value, String> {
        self.sync(vault_root, None)
    }

    fn sync_now_with(
        &self,
        vault_root: &Path,
        hooks: &mut McpSyncHooks<'_>,
    ) -> Result<Value, String> {
        self.sync(vault_root, Some(hooks))
    }

    fn resume(&self, vault_root: &Path) -> Result<Value, String> {
        let registration = self.registration(vault_root)?;
        let resumed = registration.sync_paused;
        if resumed {
            self.registry
                .update(
                    &registration.id,
                    &UpdateWikiRequest {
                        sync_backend: None,
                        groups_to_add: Vec::new(),
                        groups_to_remove: Vec::new(),
                        permissions_profile: None,
                        sync_paused: Some(false),
                        profile: None,
                        execution_trust: None,
                        mcp_remote: None,
                    },
                    false,
                )
                .map_err(|error| error.to_string())?;
        }
        let sync = self.sync_now(vault_root)?;
        Ok(json!({ "resumed": resumed, "sync": sync }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::AddWikiRequest;

    #[test]
    fn a_direct_cycle_reports_its_phases_and_trips_the_engine_token_when_cancelled() {
        let temporary = tempfile::tempdir().unwrap();
        let mut seen = Vec::new();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let cancelled = || stop.load(std::sync::atomic::Ordering::Acquire);
        let mut on_phase = |phase: &str| seen.push(phase.to_string());
        let mut hooks = McpSyncHooks {
            on_phase: &mut on_phase,
            cancelled: &cancelled,
        };
        let cancellation = vulcan_sync::SyncCancellationToken::default();
        let mut observer = PhaseObserver {
            hooks: &mut hooks,
            cancellation: &cancellation,
        };
        let mut progress = vulcan_sync::GitSyncProgress {
            phase: vulcan_sync::GitSyncPhase::Fetching,
            attempt: 1,
            repository: vulcan_sync::GitRepository {
                git_dir: temporary.path().join(".git"),
                common_dir: temporary.path().join(".git"),
                work_tree: Some(temporary.path().to_path_buf()),
                layout: vulcan_sync::GitRepositoryLayout::Colocated,
                object_format: vulcan_sync::GitObjectFormat::Sha1,
            },
            local_snapshot: None,
            local_tree: None,
        };
        vulcan_sync::GitSyncObserver::progress(&mut observer, &progress).unwrap();
        assert!(!cancellation.is_cancelled());
        stop.store(true, std::sync::atomic::Ordering::Release);
        progress.phase = vulcan_sync::GitSyncPhase::Merging;
        vulcan_sync::GitSyncObserver::progress(&mut observer, &progress).unwrap();
        assert!(cancellation.is_cancelled());
        assert_eq!(seen, ["fetching", "merging"]);
    }

    #[test]
    fn resume_clears_the_pause_and_queues_a_manual_job() {
        let temporary = tempfile::tempdir().unwrap();
        let vault = temporary.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let supervisor = Arc::new(SyncSupervisor::at(temporary.path().join("jobs.json")).unwrap());
        let registration = registry
            .add(
                &AddWikiRequest {
                    source: None,
                    id: crate::registry::WikiId::parse("notes").unwrap(),
                    path: vault.clone(),
                    profile: None,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .unwrap();
        registry
            .update(
                &registration.id,
                &UpdateWikiRequest {
                    sync_backend: None,
                    groups_to_add: Vec::new(),
                    groups_to_remove: Vec::new(),
                    permissions_profile: None,
                    sync_paused: Some(true),
                    profile: None,
                    execution_trust: None,
                    mcp_remote: None,
                },
                false,
            )
            .unwrap();
        let host = RegistrySyncHost::in_daemon(registry.clone(), Arc::clone(&supervisor));

        let report = host.resume(&vault).unwrap();
        assert_eq!(report["resumed"], true, "{report}");
        assert_eq!(report["sync"]["queued"], true, "{report}");
        assert!(!registry.find_by_path(&vault).unwrap().sync_paused);
        assert!(host.sync_now(&temporary.path().join("elsewhere")).is_err());
    }
}
