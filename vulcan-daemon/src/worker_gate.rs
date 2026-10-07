//! Background workers' vault and repository mutations wait for the host's
//! shared per-vault mutation permit, as hosted requests do, so a worker's
//! apply never runs alongside an MCP or other hosted write to the same vault
//! or repository. Planning and agent calls run before the gate; the
//! workflows still take their own cross-process locks inside it.

use crate::mutation_scheduler::{MutationScheduleError, MutationScheduler, ScheduledOperation};
use crate::registry::WikiRegistration;
use std::sync::Arc;
use std::time::Duration;
use vulcan_app::execution::{
    ExecutionAuthority, ExecutionCancellationToken, ExecutionContext, ExecutionDeadline,
    ExecutionIdentity, ExecutionRepositoryIdentity, ExecutionRetryClass, ExecutionVaultIdentity,
    MutationGate,
};
use vulcan_app::AppError;
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::{GitCliEngine, GitEngine};

/// Longest a worker waits for its turn before abandoning the attempt; the
/// work stays pending for a later pass.
const WORKER_PERMIT_WAIT: Duration = Duration::from_secs(300);

/// The host scheduler as seen by one background service.
pub struct HostedWorkerGate {
    scheduler: Arc<MutationScheduler>,
    runtime: tokio::runtime::Handle,
    service_id: &'static str,
}

impl HostedWorkerGate {
    #[must_use]
    pub fn new(
        scheduler: Arc<MutationScheduler>,
        runtime: tokio::runtime::Handle,
        service_id: &'static str,
    ) -> Self {
        Self {
            scheduler,
            runtime,
            service_id,
        }
    }

    /// The gate for one registered vault, authorized by its configured
    /// permission profile.
    #[must_use]
    pub fn for_registration<'a>(&'a self, registration: &'a WikiRegistration) -> VaultGate<'a> {
        VaultGate {
            host: self,
            registration,
        }
    }
}

/// A [`MutationGate`] for one registered vault. Entering builds a
/// background-service execution context with the profile's grant as its
/// ceiling, waits for the vault's (and repository's) mutation permit, and
/// re-checks after the wait that the profile still grants Git access.
pub struct VaultGate<'a> {
    host: &'a HostedWorkerGate,
    registration: &'a WikiRegistration,
}

impl VaultGate<'_> {
    fn profile(&self) -> &str {
        self.registration
            .permissions_profile
            .as_deref()
            .unwrap_or("unrestricted")
    }

    fn context(&self, paths: &VaultPaths) -> Result<ExecutionContext, String> {
        let grant = resolve_permission_profile(paths, Some(self.profile()))
            .map_err(|error| error.to_string())?
            .grant;
        let repository = GitCliEngine::default()
            .discover_repository(&self.registration.path)
            .ok()
            .map(|repository| {
                ExecutionRepositoryIdentity::resolve(
                    repository.common_dir.display().to_string(),
                    &repository.common_dir,
                )
            })
            .transpose()
            .map_err(|error| error.to_string())?;
        ExecutionContext::new(
            ExecutionVaultIdentity::resolve(
                &self.registration.path,
                Some(self.registration.id.to_string()),
                repository,
            )
            .map_err(|error| error.to_string())?,
            ExecutionAuthority::BackgroundService {
                service_id: self.host.service_id.to_string(),
                authority_id: format!("{}:{}", self.registration.id, self.profile()),
                permission_ceiling: grant.clone(),
            },
            grant,
            ExecutionIdentity::new(self.host.service_id),
            None,
            ExecutionRetryClass::DurableRecovery,
            ExecutionCancellationToken::default(),
            Some(ExecutionDeadline::after(WORKER_PERMIT_WAIT)),
        )
        .map_err(|error| error.to_string())
    }
}

impl MutationGate for VaultGate<'_> {
    fn enter(&self) -> Result<Box<dyn std::any::Any>, AppError> {
        let paths = VaultPaths::new(&self.registration.path);
        let context = self.context(&paths).map_err(AppError::operation)?;
        let revalidate = |_: &ExecutionContext| {
            resolve_permission_profile(&paths, Some(self.profile()))
                .map(|selection| ProfilePermissionGuard::new(&paths, selection))
                .map_err(|error| error.to_string())
                .and_then(|guard| guard.check_git().map_err(|error| error.to_string()))
                .map_err(MutationScheduleError::Revalidation)
        };
        let permit = self
            .host
            .runtime
            .block_on(self.host.scheduler.acquire(
                &context,
                ScheduledOperation::Mutation,
                revalidate,
            ))
            .map_err(|error| AppError::operation(error.to_string()))?;
        Ok(Box::new(permit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation_scheduler::MutationSchedulerConfig;
    use crate::registry::{ManagedDirectoryProfile, MaterializationProfile, WikiId};
    use std::path::Path;
    use std::sync::mpsc;
    use ulid::Ulid;

    fn registration(path: &Path, profile: Option<&str>) -> WikiRegistration {
        WikiRegistration {
            profile: ManagedDirectoryProfile::Knowledge,
            profile_version: None,
            materialization: MaterializationProfile::Full,
            id: WikiId::parse("notes").unwrap(),
            registration_id: Ulid::new(),
            path: path.to_path_buf(),
            work_tree: None,
            groups: vec![],
            git_dir: None,
            permissions_profile: profile.map(str::to_string),
            sync_backend: Some("git".to_string()),
            platform_profile: None,
            sync_paused: false,
        }
    }

    fn hosted_writer(path: &Path) -> ExecutionContext {
        let grant = resolve_permission_profile(&VaultPaths::new(path), None)
            .unwrap()
            .grant;
        ExecutionContext::new(
            ExecutionVaultIdentity::resolve(path, None, None).unwrap(),
            ExecutionAuthority::Caller {
                principal_id: "mcp-client".to_string(),
                credential_id: None,
                permission_ceiling: grant.clone(),
            },
            grant,
            ExecutionIdentity::new("mcp:test"),
            None,
            ExecutionRetryClass::IndeterminateAfterDispatch,
            ExecutionCancellationToken::default(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn worker_mutations_wait_for_hosted_writers_and_recheck_git_authority() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let scheduler =
            Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).unwrap());
        let vault = tempfile::tempdir().unwrap();
        let gate = HostedWorkerGate::new(
            Arc::clone(&scheduler),
            runtime.handle().clone(),
            "worker.test",
        );
        let notes = registration(vault.path(), None);
        let writer = hosted_writer(vault.path());
        let acquire_writer =
            || scheduler.acquire(&writer, ScheduledOperation::Mutation, |_| Ok(()));

        // A worker waits for a hosted write to the same vault to finish.
        let held = runtime.block_on(acquire_writer()).unwrap();
        let (entered, waiting) = mpsc::channel();
        std::thread::scope(|threads| {
            threads.spawn(|| {
                let guard = gate.for_registration(&notes).enter().unwrap();
                entered.send(()).unwrap();
                drop(guard);
            });
            assert!(waiting
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err());
            drop(held);
            waiting
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("worker entered after the writer finished");
        });

        // A hosted write waits for a worker that holds the vault.
        let guard = gate.for_registration(&notes).enter().unwrap();
        let blocked = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(200), acquire_writer()).await
        });
        assert!(blocked.is_err(), "hosted writer ran during a worker apply");
        drop(guard);
        drop(runtime.block_on(acquire_writer()).unwrap());

        // Authority is checked at entry: a profile without Git access fails.
        std::fs::create_dir_all(vault.path().join(".vulcan")).unwrap();
        std::fs::write(
            vault.path().join(".vulcan/config.toml"),
            "[permissions.profiles.nogit]\nread = \"all\"\ngit = \"deny\"\n",
        )
        .unwrap();
        let revoked = registration(vault.path(), Some("nogit"));
        let error = gate.for_registration(&revoked).enter().unwrap_err();
        assert!(error.to_string().contains("revalidation"), "{error}");
    }
}
