//! A long-lived host's retained mdbase query state.
//!
//! The session keeps what is expensive to rebuild and safe to reuse between
//! requests: authorized control registries (keyed by read scope), compiled
//! query plans, and a read-only cache connection with its statement cache. It
//! never retains answers. Every request acquires the cooperating read guard
//! and verifies the retained registries against the current controls. By
//! default it also proves the cache current with a full stat walk.
//!
//! With an attached [`MdbaseChangeMonitor`] the session uses an explicit
//! *watched* freshness policy: a scope's last walk proof is reused only while
//! the monitor stays healthy and reports no change since before that walk
//! began, no Vulcan writer has completed an exclusive write section since (the
//! cooperating write epoch), and the proof is younger than `max_age`. External
//! edits are therefore observed once their notifications are delivered;
//! cooperating writes are observed immediately. Any miss falls back to the
//! ordinary disk-reconciled service and drops retained registries and proofs.

use super::query_profile::{time, MdbaseQueryMetrics};
use super::{
    allowed, build_mdbase_query_report_profiled, control_permission_denied, indexed_query_allowed,
    load_control_registries, open_query_cache, registry_diagnostics, LoadedCollection,
};
use crate::AppError;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use std::time::{Instant, SystemTime};
use vulcan_core::mdbase::{
    compile_mdbase_prepared_query, execute_retained_mdbase_query, load_mdbase_collection,
    MdbaseChangeMonitor, MdbasePreparedQuery, MdbaseQueryResult, MdbaseRetainedProof,
    MdbaseRetainedRows,
};
use vulcan_core::{PermissionFilter, VaultPaths};

/// Compiled plans retained per session; plans hold no records or authority.
const RETAINED_PLANS: usize = 64;
/// Distinct read scopes with retained registries.
const RETAINED_SCOPES: usize = 8;

pub struct MdbaseQuerySession {
    paths: VaultPaths,
    connection: Option<rusqlite::Connection>,
    collections: HashMap<String, LoadedCollection>,
    scope_order: VecDeque<String>,
    plans: HashMap<String, Arc<MdbasePreparedQuery>>,
    plan_order: VecDeque<String>,
    rows: MdbaseRetainedRows,
    watched: Option<Watched>,
    proofs: HashMap<String, ScopeProof>,
}

struct Watched {
    monitor: MdbaseChangeMonitor,
    max_age: Duration,
}

struct ScopeProof {
    proof: MdbaseRetainedProof,
    generation: u64,
    epoch: u64,
    proven_at: Instant,
}

impl MdbaseQuerySession {
    #[must_use]
    pub fn new(paths: VaultPaths) -> Self {
        Self {
            paths,
            connection: None,
            collections: HashMap::new(),
            scope_order: VecDeque::new(),
            plans: HashMap::new(),
            plan_order: VecDeque::new(),
            rows: MdbaseRetainedRows::default(),
            watched: None,
            proofs: HashMap::new(),
        }
    }

    /// Adopt the watched freshness policy with `monitor` watching this
    /// collection's root. Proofs older than `max_age` are always re-walked.
    #[must_use]
    pub fn with_change_monitor(mut self, monitor: MdbaseChangeMonitor, max_age: Duration) -> Self {
        self.watched = Some(Watched { monitor, max_age });
        self
    }

    /// Execute a canonical mdbase query with exactly the results, errors, and
    /// authorization of [`super::build_mdbase_query_report`].
    pub fn query(
        &mut self,
        query: &serde_json::Value,
        filter: Option<&PermissionFilter>,
    ) -> Result<MdbaseQueryResult, AppError> {
        self.query_profiled(query, filter, &mut MdbaseQueryMetrics::default())
    }

    /// [`Self::query`] with diagnostic metrics, reset before authorization.
    pub fn query_profiled(
        &mut self,
        query: &serde_json::Value,
        filter: Option<&PermissionFilter>,
        metrics: &mut MdbaseQueryMetrics,
    ) -> Result<MdbaseQueryResult, AppError> {
        *metrics = MdbaseQueryMetrics::default();
        let start = Instant::now();
        let result = self.query_inner(query, filter, metrics);
        metrics.total_seconds = start.elapsed().as_secs_f64();
        result
    }

    fn query_inner(
        &mut self,
        query: &serde_json::Value,
        filter: Option<&PermissionFilter>,
        metrics: &mut MdbaseQueryMetrics,
    ) -> Result<MdbaseQueryResult, AppError> {
        if !allowed(filter, "mdbase.yaml") {
            return Err(control_permission_denied());
        }
        let now = DateTime::<Utc>::from(SystemTime::now());
        if indexed_query_allowed(filter) {
            let read_guard = time(&mut metrics.collection_seconds, || {
                vulcan_core::mdbase::acquire_mdbase_consistent_read(&self.paths)
                    .map_err(AppError::operation)
            })?;
            let scope = scope_key(filter);
            if !self.collections.contains_key(&scope) {
                let loaded = time(&mut metrics.collection_seconds, || {
                    load_retained(&self.paths, filter)
                })?;
                self.retain_scope(scope.clone(), loaded);
            }
            let plan = time(&mut metrics.query_preparation_seconds, || self.plan(query))?;
            if self.connection.is_none() {
                self.connection = open_query_cache(&self.paths);
            }
            // Read both counters under the shared lock, before any walk, so a
            // change racing this request invalidates the proof it produces.
            let epoch = vulcan_core::write_lock::read_write_epoch(&self.paths).ok();
            let generation = self
                .watched
                .as_ref()
                .and_then(|watched| watched.monitor.generation());
            let trusted = match (&self.watched, self.proofs.get(&scope), generation, epoch) {
                (Some(watched), Some(proof), Some(generation), Some(epoch))
                    if proof.generation == generation
                        && proof.epoch == epoch
                        && proof.proven_at.elapsed() < watched.max_age =>
                {
                    Some(proof.proof.clone())
                }
                _ => None,
            };
            let loaded = &self.collections[&scope];
            let rows = &mut self.rows;
            let result = self.connection.as_ref().and_then(|connection| {
                metrics.indexed_attempts += 1;
                let result = execute_retained_mdbase_query(
                    connection,
                    &loaded.collection,
                    &loaded.types,
                    &loaded.contracts,
                    &plan,
                    filter,
                    now,
                    rows,
                    trusted.as_ref(),
                    &mut metrics.indexed,
                )
                .ok()
                .flatten();
                if result.is_some() {
                    metrics.indexed_hits += 1;
                    metrics.prepared_visible_records = metrics.indexed.visible_records;
                }
                result
            });
            let result = result.map(|(report, proof)| {
                if trusted.is_none() {
                    match (generation, epoch) {
                        (Some(generation), Some(epoch)) => {
                            self.proofs.insert(
                                scope.clone(),
                                ScopeProof {
                                    proof,
                                    generation,
                                    epoch,
                                    proven_at: Instant::now(),
                                },
                            );
                        }
                        _ => {
                            self.proofs.remove(&scope);
                        }
                    }
                }
                report
            });
            let loaded = &self.collections[&scope];
            drop(read_guard);
            if let Some(mut report) = result {
                report
                    .diagnostics
                    .splice(0..0, registry_diagnostics(loaded, filter));
                return Ok(report);
            }
            // Controls or the cache may have changed; reload next time.
            self.collections.remove(&scope);
            self.scope_order.retain(|retained| retained != &scope);
            self.proofs.clear();
        }
        // The ordinary service resets the metrics and records its own work.
        build_mdbase_query_report_profiled(&self.paths, query, filter, metrics)
    }

    fn plan(&mut self, query: &serde_json::Value) -> Result<Arc<MdbasePreparedQuery>, AppError> {
        let key = serde_json::to_string(query).map_err(AppError::operation)?;
        if let Some(plan) = self.plans.get(&key) {
            return Ok(Arc::clone(plan));
        }
        let plan = Arc::new(compile_mdbase_prepared_query(query).map_err(AppError::operation)?);
        if self.plan_order.len() >= RETAINED_PLANS {
            if let Some(oldest) = self.plan_order.pop_front() {
                self.plans.remove(&oldest);
            }
        }
        self.plan_order.push_back(key.clone());
        self.plans.insert(key, Arc::clone(&plan));
        Ok(plan)
    }

    fn retain_scope(&mut self, scope: String, loaded: LoadedCollection) {
        if self.scope_order.len() >= RETAINED_SCOPES {
            if let Some(oldest) = self.scope_order.pop_front() {
                self.collections.remove(&oldest);
                self.proofs.remove(&oldest);
            }
        }
        self.scope_order.push_back(scope.clone());
        self.collections.insert(scope, loaded);
    }
}

/// Load registries with the caller's authority, retained without a read guard.
fn load_retained(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<LoadedCollection, AppError> {
    let collection = load_mdbase_collection(paths.vault_root())
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("not an mdbase collection: missing mdbase.yaml"))?;
    let (types, contracts) = load_control_registries(&collection, filter)?;
    Ok(LoadedCollection {
        read_guard: None,
        control_filter: filter.cloned(),
        collection,
        types,
        contracts,
    })
}

fn scope_key(filter: Option<&PermissionFilter>) -> String {
    filter.map_or_else(
        || "unrestricted".to_string(),
        |filter| format!("{:?}", filter.path_permission()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::build_mdbase_query_report;
    use crate::mdbase::tests::{fixture, read_control_grant};
    use serde_json::json;
    use std::fs;
    use std::time::Duration;
    use vulcan_core::permissions::{PathPermission, ResourceSpecifier};

    fn initialized() -> (tempfile::TempDir, VaultPaths) {
        let (directory, paths) = fixture();
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        // Populate the cache through the ordinary unrestricted service.
        build_mdbase_query_report(&paths, &json!({"types": ["task"]}), None).unwrap();
        (directory, paths)
    }

    fn restricted() -> PermissionFilter {
        let mut allow = read_control_grant();
        allow.push(ResourceSpecifier::Note("mdbase.lock.yaml".into()));
        PermissionFilter::new(PathPermission {
            allow,
            deny: vec![ResourceSpecifier::Folder("tasks/private/**".into())],
        })
    }

    fn queries() -> Vec<serde_json::Value> {
        vec![
            json!({"types": ["task"], "where": "title == 'Public'", "select": ["title"]}),
            json!({"types": ["task"], "where": "status == 'open'", "order_by": [{"field": "title"}],
                "frontmatter_mode": "both"}),
            json!({"types": ["task"], "order_by": [{"field": "title", "direction": "desc"}], "limit": 1}),
            json!({"types": ["task"], "where": "undeclared == 'x'"}),
            json!({"select": ["title", {"name": "copy", "expr": "title"}]}),
        ]
    }

    #[test]
    fn session_results_equal_the_ordinary_service_and_reuse_rows() {
        let (_directory, paths) = initialized();
        let mut session = MdbaseQuerySession::new(paths.clone());
        let filter = restricted();
        for _ in 0..2 {
            for query in queries() {
                for scope in [None, Some(&filter)] {
                    assert_eq!(
                        session.query(&query, scope).unwrap(),
                        build_mdbase_query_report(&paths, &query, scope).unwrap(),
                        "{query}"
                    );
                }
            }
        }
        let mut metrics = MdbaseQueryMetrics::default();
        session
            .query_profiled(&queries()[0], None, &mut metrics)
            .unwrap();
        assert_eq!(metrics.indexed_hits, 1);
        assert_eq!(metrics.indexed.reloaded_rows, 0);
        assert!(!metrics.indexed.trusted_proof);
        // Denied control reads stay denied for a retained session.
        let denied = PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::Folder("tasks/**".into())],
            deny: Vec::new(),
        });
        assert!(session.query(&queries()[0], Some(&denied)).is_err());
    }

    #[test]
    fn watched_sessions_trust_proofs_only_until_a_change_is_observed() {
        let (directory, paths) = initialized();
        let monitor =
            MdbaseChangeMonitor::watch(&directory.path().canonicalize().unwrap()).unwrap();
        let mut session = MdbaseQuerySession::new(paths.clone())
            .with_change_monitor(monitor, Duration::from_secs(60));
        let query = json!({"types": ["task"], "where": "title == 'Public'", "select": ["title"]});
        let mut metrics = MdbaseQueryMetrics::default();
        session.query_profiled(&query, None, &mut metrics).unwrap();
        assert!(!metrics.indexed.trusted_proof);
        let report = session.query_profiled(&query, None, &mut metrics).unwrap();
        assert!(metrics.indexed.trusted_proof);
        assert_eq!(report.meta.total_count, 1);

        // A cooperating writer is visible immediately, without notifications.
        {
            let _lock = vulcan_core::write_lock::acquire_write_lock(&paths).unwrap();
            fs::write(
                directory.path().join("tasks/public.md"),
                "---\ntype: task\ntitle: Renamed\n---\nBody\n",
            )
            .unwrap();
        }
        let report = session.query_profiled(&query, None, &mut metrics).unwrap();
        assert!(!metrics.indexed.trusted_proof);
        assert_eq!(report.meta.total_count, 0);
        assert_eq!(
            report,
            build_mdbase_query_report(&paths, &query, None).unwrap()
        );

        // An external edit is visible once its notification is delivered.
        session.query_profiled(&query, None, &mut metrics).unwrap();
        fs::write(
            directory.path().join("tasks/public.md"),
            "---\ntype: task\ntitle: Public\n---\nBody\n",
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let report = session.query_profiled(&query, None, &mut metrics).unwrap();
            if report.meta.total_count == 1 {
                assert!(!metrics.indexed.trusted_proof);
                break;
            }
            assert!(Instant::now() < deadline, "edit was never observed");
            std::thread::sleep(Duration::from_millis(20));
        }

        // Proofs expire, and each scope proves its own visibility.
        let mut aged = MdbaseQuerySession::new(paths.clone()).with_change_monitor(
            MdbaseChangeMonitor::watch(&directory.path().canonicalize().unwrap()).unwrap(),
            Duration::ZERO,
        );
        aged.query_profiled(&query, None, &mut metrics).unwrap();
        aged.query_profiled(&query, None, &mut metrics).unwrap();
        assert!(!metrics.indexed.trusted_proof);
        let filter = restricted();
        let scoped = json!({"types": ["task"]});
        let report = session
            .query_profiled(&scoped, Some(&filter), &mut metrics)
            .unwrap();
        assert!(!metrics.indexed.trusted_proof);
        assert_eq!(report.meta.total_count, 1);
        let report = session
            .query_profiled(&scoped, Some(&filter), &mut metrics)
            .unwrap();
        assert!(metrics.indexed.trusted_proof);
        assert_eq!(
            report,
            build_mdbase_query_report(&paths, &scoped, Some(&filter)).unwrap()
        );
    }
}
