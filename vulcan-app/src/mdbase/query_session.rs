//! A long-lived host's retained mdbase query state.
//!
//! The session keeps what is expensive to rebuild and safe to reuse between
//! requests: authorized control registries (keyed by read scope), compiled
//! query plans, and a read-only cache connection with its statement cache. It
//! never retains answers. Every request verifies the retained registries
//! against the current controls and, by default, proves the cache current
//! with a full stat walk.
//!
//! Retained execution does not take the vault read lock, so readers never
//! queue behind a write section. A stat-proven result cannot observe a
//! half-applied write: files and cache rows agree only before a transaction's
//! first replacement or after its cache publication, and anything between
//! fails the proof. Such misses, and interrupted writes, take the ordinary
//! service with its cooperating read guard.
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
    allowed, build_mdbase_metadata_read_report, build_mdbase_query_report_profiled,
    control_permission_denied, indexed_query_allowed, load_control_registries, open_query_cache,
    registry_diagnostics, LoadedCollection, MdbaseMetadataReadReport, MdbaseRecordMetadata,
};
use crate::AppError;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use std::time::{Instant, SystemTime};
use vulcan_core::mdbase::{
    compile_mdbase_prepared_query, execute_retained_mdbase_query, load_mdbase_collection,
    mdbase_query_is_retainable, read_retained_mdbase_record, walk_mdbase_retained_scope,
    MdbaseChangeMonitor, MdbasePreparedQuery, MdbaseQueryResult, MdbaseRetainedProof,
    MdbaseRetainedRows,
};
use vulcan_core::{PermissionFilter, VaultPaths};

/// Compiled plans retained per session; plans hold no records or authority.
const RETAINED_PLANS: usize = 64;
/// Distinct read scopes with retained registries.
const RETAINED_SCOPES: usize = 8;
/// Idle read-only cache connections kept for concurrent requests.
const IDLE_CONNECTIONS: usize = 8;

/// Retained query state shared by concurrent requests (`&self`; the session
/// is `Sync`). Readers whose freshness is already proven, or whose walk finds
/// every visible row retained, execute together under a shared lock on the
/// retained rows; only a request that must decode changed rows takes it
/// exclusively. Each request uses its own pooled read-only connection.
pub struct MdbaseQuerySession {
    paths: VaultPaths,
    connections: Mutex<Vec<rusqlite::Connection>>,
    scopes: Mutex<Lru<Arc<LoadedCollection>>>,
    plans: Mutex<Lru<Arc<MdbasePreparedQuery>>>,
    rows: RwLock<MdbaseRetainedRows>,
    watched: Option<Watched>,
    proofs: Mutex<HashMap<String, ScopeProof>>,
    provers: Mutex<HashMap<String, Arc<Mutex<()>>>>,
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

/// Insertion-ordered bounded map.
struct Lru<T> {
    entries: HashMap<String, T>,
    order: VecDeque<String>,
    capacity: usize,
}

impl<T: Clone> Lru<T> {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    fn get(&self, key: &str) -> Option<T> {
        self.entries.get(key).cloned()
    }

    /// Insert unless present; returns the retained value and any evicted key.
    fn insert(&mut self, key: String, value: T) -> (T, Option<String>) {
        if let Some(existing) = self.entries.get(&key) {
            return (existing.clone(), None);
        }
        let mut evicted = None;
        if self.order.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
                evicted = Some(oldest);
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, value.clone());
        (value, evicted)
    }

    fn remove(&mut self, key: &str) {
        self.entries.remove(key);
        self.order.retain(|retained| retained != key);
    }
}

impl MdbaseQuerySession {
    #[must_use]
    pub fn new(paths: VaultPaths) -> Self {
        Self {
            paths,
            connections: Mutex::new(Vec::new()),
            scopes: Mutex::new(Lru::new(RETAINED_SCOPES)),
            plans: Mutex::new(Lru::new(RETAINED_PLANS)),
            rows: RwLock::new(MdbaseRetainedRows::default()),
            watched: None,
            proofs: Mutex::new(HashMap::new()),
            provers: Mutex::new(HashMap::new()),
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
        &self,
        query: &serde_json::Value,
        filter: Option<&PermissionFilter>,
    ) -> Result<MdbaseQueryResult, AppError> {
        self.query_profiled(query, filter, &mut MdbaseQueryMetrics::default())
    }

    /// [`Self::query`] with diagnostic metrics, reset before authorization.
    pub fn query_profiled(
        &self,
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
        &self,
        query: &serde_json::Value,
        filter: Option<&PermissionFilter>,
        metrics: &mut MdbaseQueryMetrics,
    ) -> Result<MdbaseQueryResult, AppError> {
        if !allowed(filter, "mdbase.yaml") {
            return Err(control_permission_denied());
        }
        let now = DateTime::<Utc>::from(SystemTime::now());
        let admitted = indexed_query_allowed(filter)
            && time(&mut metrics.collection_seconds, || {
                vulcan_core::mdbase::check_mdbase_lock_free_read(&self.paths).is_ok()
            });
        if admitted {
            let scope = scope_key(filter);
            let loaded = time(&mut metrics.collection_seconds, || {
                self.scope(&scope, filter)
            })?;
            let plan = time(&mut metrics.query_preparation_seconds, || self.plan(query))?;
            let result = if mdbase_query_is_retainable(&plan, &loaded.types) {
                self.with_connection(|connection| {
                    self.query_retained(connection, &scope, &loaded, &plan, filter, now, metrics)
                })
            } else {
                None
            };
            if let Some(mut report) = result {
                report
                    .diagnostics
                    .splice(0..0, registry_diagnostics(&loaded, filter));
                return Ok(report);
            }
            // Controls or the cache may have changed; reload next time.
            lock(&self.scopes).remove(&scope);
            lock(&self.proofs).clear();
        }
        // The ordinary service resets the metrics and records its own work.
        build_mdbase_query_report_profiled(&self.paths, query, filter, metrics)
    }

    /// Read one record's metadata with exactly the result, errors, and
    /// authorization of [`super::build_mdbase_metadata_read_report`]. Once
    /// the retained proof holds, unrestricted readers are answered from one
    /// cached row; restricted readers also overlay that record's links and
    /// uniqueness against only the records they can see.
    pub fn read_metadata(
        &self,
        path: &str,
        filter: Option<&PermissionFilter>,
    ) -> Result<MdbaseMetadataReadReport, AppError> {
        self.read_metadata_profiled(path, filter, &mut MdbaseQueryMetrics::default())
    }

    /// [`Self::read_metadata`] with diagnostic metrics, reset first.
    pub fn read_metadata_profiled(
        &self,
        path: &str,
        filter: Option<&PermissionFilter>,
        metrics: &mut MdbaseQueryMetrics,
    ) -> Result<MdbaseMetadataReadReport, AppError> {
        *metrics = MdbaseQueryMetrics::default();
        let start = Instant::now();
        let result = self.read_metadata_inner(path, filter, metrics);
        metrics.total_seconds = start.elapsed().as_secs_f64();
        result
    }

    fn read_metadata_inner(
        &self,
        path: &str,
        filter: Option<&PermissionFilter>,
        metrics: &mut MdbaseQueryMetrics,
    ) -> Result<MdbaseMetadataReadReport, AppError> {
        let admitted = allowed(filter, path)
            && allowed(filter, "mdbase.yaml")
            && indexed_query_allowed(filter)
            && time(&mut metrics.collection_seconds, || {
                vulcan_core::mdbase::check_mdbase_lock_free_read(&self.paths).is_ok()
            });
        if admitted {
            let scope = scope_key(filter);
            let loaded = time(&mut metrics.collection_seconds, || {
                self.scope(&scope, filter)
            })?;
            let read = |rows: &MdbaseRetainedRows,
                        proof: &MdbaseRetainedProof,
                        trusted: bool,
                        metrics: &mut MdbaseQueryMetrics| {
                metrics.indexed_attempts += 1;
                // `Some(None)`: not a visible record.
                let record = self.with_connection(|connection| {
                    read_retained_mdbase_record(
                        connection,
                        &loaded.collection,
                        &loaded.types,
                        &loaded.contracts,
                        filter,
                        rows,
                        proof,
                        trusted,
                        path,
                    )
                    .ok()
                    .flatten()
                })?;
                metrics.indexed_hits += 1;
                metrics.indexed.trusted_proof = trusted;
                Some(record)
            };
            let record = self.with_connection(|connection| {
                self.proven(connection, &scope, &loaded, filter, metrics, read)
            });
            if let Some(Some(record)) = record {
                let metadata = record.metadata.unwrap_or_else(|| unreachable!());
                let mut diagnostics = registry_diagnostics(&loaded, filter);
                diagnostics.extend(
                    record
                        .diagnostics
                        .iter()
                        .map(vulcan_core::mdbase::MdbaseDiagnostic::from_record),
                );
                let valid = diagnostics.iter().all(|diagnostic| {
                    diagnostic.severity != vulcan_core::mdbase::MdbaseDiagnosticLevel::Error
                });
                return Ok(MdbaseMetadataReadReport {
                    valid,
                    record: MdbaseRecordMetadata {
                        path: record.path,
                        revision: record.revision,
                        types: record.types,
                        frontmatter: metadata.frontmatter,
                        effective_frontmatter: record.effective_frontmatter,
                        file: metadata.file,
                        display: record.display,
                        contract_views: record.contract_views,
                        diagnostics: record.diagnostics,
                    },
                    diagnostics,
                });
            }
            // Unknown paths take the ordinary read for its exact error;
            // the retained state stays proven.
            if record.is_none() {
                lock(&self.scopes).remove(&scope);
                lock(&self.proofs).clear();
            }
        }
        *metrics = MdbaseQueryMetrics::default();
        build_mdbase_metadata_read_report(&self.paths, path, filter)
    }

    #[allow(clippy::too_many_arguments)]
    fn query_retained(
        &self,
        connection: &rusqlite::Connection,
        scope: &str,
        loaded: &LoadedCollection,
        plan: &MdbasePreparedQuery,
        filter: Option<&PermissionFilter>,
        now: DateTime<Utc>,
        metrics: &mut MdbaseQueryMetrics,
    ) -> Option<MdbaseQueryResult> {
        let execute = |rows: &MdbaseRetainedRows,
                       proof: &MdbaseRetainedProof,
                       trusted: bool,
                       metrics: &mut MdbaseQueryMetrics| {
            metrics.indexed_attempts += 1;
            let result = execute_retained_mdbase_query(
                connection,
                &loaded.collection,
                &loaded.types,
                &loaded.contracts,
                plan,
                filter,
                now,
                rows,
                proof,
                trusted,
                &mut metrics.indexed,
            )
            .ok()
            .flatten();
            if result.is_some() {
                metrics.indexed_hits += 1;
                metrics.prepared_visible_records = metrics.indexed.visible_records;
            }
            result
        };
        self.proven(connection, scope, loaded, filter, metrics, execute)
    }

    /// Run `answer` over the retained rows with a proof that they describe
    /// the scope's current visible records: a trusted watched proof when one
    /// holds, else a fresh walk, reconciling changed rows when needed.
    /// `answer` receives whether the proof was trusted without a walk.
    /// `None` when no proof can be established or `answer` declines.
    fn proven<T>(
        &self,
        connection: &rusqlite::Connection,
        scope: &str,
        loaded: &LoadedCollection,
        filter: Option<&PermissionFilter>,
        metrics: &mut MdbaseQueryMetrics,
        answer: impl Fn(
            &MdbaseRetainedRows,
            &MdbaseRetainedProof,
            bool,
            &mut MdbaseQueryMetrics,
        ) -> Option<T>,
    ) -> Option<T> {
        // Read both counters before any walk, so a change racing this request
        // invalidates the proof it produces. The epoch advances only when a
        // write section ends, after its cache publication.
        let trusted = |epoch, generation, metrics: &mut MdbaseQueryMetrics| {
            let proof = self.trusted_proof(scope, generation, epoch)?;
            let rows = self
                .rows
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let result = answer(&rows, &proof, true, metrics);
            if result.is_none() {
                // Another request reconciled past this proof; walk instead.
                metrics.indexed = vulcan_core::mdbase::MdbaseIndexedQueryMetrics::default();
            }
            result
        };
        let (mut epoch, mut generation) = self.counters();
        if let Some(result) = trusted(epoch, generation, metrics) {
            return Some(result);
        }
        // Watched sessions prove each scope single-flight: requests that
        // arrive during a walk wait for its proof instead of walking too.
        let prover = self.watched.as_ref().map(|_| self.prover(scope));
        let _proving = prover.as_deref().map(lock);
        if prover.is_some() {
            (epoch, generation) = self.counters();
            if let Some(result) = trusted(epoch, generation, metrics) {
                return Some(result);
            }
        }
        let start = Instant::now();
        let walk = walk_mdbase_retained_scope(
            &loaded.collection,
            &loaded.types,
            &loaded.contracts,
            filter,
        )
        .ok()??;
        let walked = start.elapsed().as_secs_f64();
        let result = {
            let rows = self
                .rows
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            walk.proof_if_retained(&rows).map(|proof| {
                metrics.indexed.freshness_seconds = walked;
                let result = answer(&rows, &proof, false, metrics);
                (result, proof)
            })
        };
        let (result, proof) = if let Some(proven) = result {
            proven
        } else {
            let mut rows = self
                .rows
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut reconciled = vulcan_core::mdbase::MdbaseIndexedQueryMetrics::default();
            let proof = walk
                .reconcile(connection, &loaded.collection, &mut rows, &mut reconciled)
                .ok()
                .flatten();
            metrics.indexed = reconciled;
            let proof = if let Some(proof) = proof {
                proof
            } else {
                // A write holding the vault lock may have replaced files it
                // has not yet published; serve the state from before it
                // rather than queue behind it. The proof stays valid until
                // the write's epoch advance, a notification, or a reconcile.
                let write = self.in_flight_write()?;
                let proof = walk.proof_before_write(&rows, &write, filter)?;
                metrics.indexed.before_write = true;
                proof
            };
            metrics.indexed.freshness_seconds = start.elapsed().as_secs_f64();
            (answer(&rows, &proof, false, metrics), proof)
        };
        let result = result?;
        self.remember(scope, proof, generation, epoch);
        Some(result)
    }

    /// The write epoch and change generation; read before any walk.
    fn counters(&self) -> (Option<u64>, Option<u64>) {
        (
            vulcan_core::write_lock::read_write_epoch(&self.paths).ok(),
            self.watched
                .as_ref()
                .and_then(|watched| watched.monitor.generation()),
        )
    }

    fn remember(
        &self,
        scope: &str,
        proof: MdbaseRetainedProof,
        generation: Option<u64>,
        epoch: Option<u64>,
    ) {
        let mut proofs = lock(&self.proofs);
        if let (Some(generation), Some(epoch)) = (generation, epoch) {
            proofs.insert(
                scope.to_string(),
                ScopeProof {
                    proof,
                    generation,
                    epoch,
                    proven_at: Instant::now(),
                },
            );
        } else {
            proofs.remove(scope);
        }
    }

    /// The single-flight proving lock for `scope`.
    fn prover(&self, scope: &str) -> Arc<Mutex<()>> {
        Arc::clone(lock(&self.provers).entry(scope.to_string()).or_default())
    }

    /// The journal of a write currently holding the vault lock.
    fn in_flight_write(&self) -> Option<vulcan_core::mdbase::MdbaseInFlightWrite> {
        let active = vulcan_core::write_lock::try_acquire_read_lock(&self.paths)
            .ok()?
            .is_none();
        active
            .then(|| vulcan_core::mdbase::load_mdbase_in_flight_write(&self.paths))
            .flatten()
    }

    fn trusted_proof(
        &self,
        scope: &str,
        generation: Option<u64>,
        epoch: Option<u64>,
    ) -> Option<MdbaseRetainedProof> {
        let watched = self.watched.as_ref()?;
        let (generation, epoch) = (generation?, epoch?);
        let proofs = lock(&self.proofs);
        let proof = proofs.get(scope)?;
        (proof.generation == generation
            && proof.epoch == epoch
            && proof.proven_at.elapsed() < watched.max_age)
            .then(|| proof.proof.clone())
    }

    fn with_connection<T>(
        &self,
        run: impl FnOnce(&rusqlite::Connection) -> Option<T>,
    ) -> Option<T> {
        let pooled = lock(&self.connections).pop();
        let connection = pooled.or_else(|| open_query_cache(&self.paths))?;
        let result = run(&connection);
        let mut idle = lock(&self.connections);
        if idle.len() < IDLE_CONNECTIONS {
            idle.push(connection);
        }
        result
    }

    fn scope(
        &self,
        scope: &str,
        filter: Option<&PermissionFilter>,
    ) -> Result<Arc<LoadedCollection>, AppError> {
        if let Some(loaded) = lock(&self.scopes).get(scope) {
            return Ok(loaded);
        }
        let loaded = Arc::new(load_retained(&self.paths, filter)?);
        let (loaded, evicted) = lock(&self.scopes).insert(scope.to_string(), loaded);
        if let Some(evicted) = evicted {
            lock(&self.proofs).remove(&evicted);
            lock(&self.provers).remove(&evicted);
        }
        Ok(loaded)
    }

    fn plan(&self, query: &serde_json::Value) -> Result<Arc<MdbasePreparedQuery>, AppError> {
        let key = serde_json::to_string(query).map_err(AppError::operation)?;
        if let Some(plan) = lock(&self.plans).get(&key) {
            return Ok(plan);
        }
        let plan = Arc::new(compile_mdbase_prepared_query(query).map_err(AppError::operation)?);
        Ok(lock(&self.plans).insert(key, plan).0)
    }
}

/// Retained state stays usable after a panicking request: every guarded value
/// is a cache that later requests verify before use.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        known_revisions: None,
        cached_local: None,
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
    use crate::mdbase::tests::{fixture, read_control_grant};
    use crate::mdbase::{build_mdbase_metadata_read_report, build_mdbase_query_report};
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
        let session = MdbaseQuerySession::new(paths.clone());
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
        let session = MdbaseQuerySession::new(paths.clone())
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
        let aged = MdbaseQuerySession::new(paths.clone()).with_change_monitor(
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

    #[test]
    fn metadata_reads_equal_the_ordinary_read_and_use_one_cached_row() {
        let (directory, paths) = initialized();
        // An invalid record, so diagnostics and validity are compared too.
        fs::write(
            directory.path().join("tasks/untitled.md"),
            "---\ntype: task\n---\nBody\n",
        )
        .unwrap();
        build_mdbase_query_report(&paths, &json!({"types": ["task"]}), None).unwrap();
        let session = MdbaseQuerySession::new(paths.clone()).with_change_monitor(
            MdbaseChangeMonitor::watch(&directory.path().canonicalize().unwrap()).unwrap(),
            Duration::from_secs(60),
        );
        let filter = restricted();
        let ordinary = |path: &str, scope: Option<&PermissionFilter>| {
            build_mdbase_metadata_read_report(&paths, path, scope)
                .map_err(|error| error.to_string())
        };
        let read = |path: &str, scope: Option<&PermissionFilter>| {
            session
                .read_metadata(path, scope)
                .map_err(|error| error.to_string())
        };
        for path in [
            "tasks/public.md",
            "tasks/untitled.md",
            "tasks/private/secret.md",
        ] {
            for scope in [None, Some(&filter)] {
                assert_eq!(read(path, scope), ordinary(path, scope), "{path}");
            }
        }
        assert!(!read("tasks/untitled.md", None).unwrap().valid);
        for missing in ["tasks/missing.md", "_types/task.md", "../outside.md"] {
            assert!(read(missing, None).is_err(), "{missing}");
            assert_eq!(read(missing, None), ordinary(missing, None));
        }

        let mut metrics = MdbaseQueryMetrics::default();
        session
            .read_metadata_profiled("tasks/public.md", None, &mut metrics)
            .unwrap();
        assert_eq!(metrics.indexed_hits, 1);
        assert!(metrics.indexed.trusted_proof);
        assert_eq!(metrics.indexed.reloaded_rows, 0);
        session
            .read_metadata_profiled("tasks/public.md", Some(&filter), &mut metrics)
            .unwrap();
        assert_eq!(metrics.indexed_hits, 1);

        // A cooperating write is observed immediately.
        {
            let _lock = vulcan_core::write_lock::acquire_write_lock(&paths).unwrap();
            fs::write(
                directory.path().join("tasks/public.md"),
                "---\ntype: task\ntitle: Renamed\n---\nBody\n",
            )
            .unwrap();
        }
        let report = read("tasks/public.md", None).unwrap();
        assert_eq!(report.record.frontmatter["title"], "Renamed");
        assert_eq!(Ok(report), ordinary("tasks/public.md", None));
    }

    #[test]
    fn identity_stable_writes_keep_restricted_link_indexes_and_stay_exact() {
        let (directory, paths) = fixture();
        fs::write(
            directory.path().join("tasks/public.md"),
            "---\ntype: task\ntitle: Public\n---\nSee [[secret]] and [[other]].\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("tasks/other.md"),
            "---\ntype: task\ntitle: Other\n---\n",
        )
        .unwrap();
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        build_mdbase_query_report(&paths, &json!({"types": ["task"]}), None).unwrap();
        let session = MdbaseQuerySession::new(paths.clone());
        let filter = restricted();
        let read = |path: &str| {
            let expected = build_mdbase_metadata_read_report(&paths, path, Some(&filter)).unwrap();
            let actual = session.read_metadata(path, Some(&filter)).unwrap();
            assert_eq!(actual, expected, "{path}");
            actual.record.revision
        };
        let built = || session.rows.read().unwrap().link_indexes_built();
        let patch = |path: &str, revision: &str, field: &str, value: serde_json::Value| {
            crate::mdbase::patch_mdbase_frontmatter(
                &paths,
                &crate::mdbase::MdbaseFrontmatterPatchRequest {
                    path: path.to_string(),
                    if_revision: revision.to_string(),
                    set: std::collections::BTreeMap::from([(field.to_string(), value)]),
                    unset: Vec::new(),
                    permission_profile: None,
                },
                &crate::mdbase::MdbaseFrontmatterPatchOptions {
                    no_commit: true,
                    quiet: true,
                    ..crate::mdbase::MdbaseFrontmatterPatchOptions::default()
                },
            )
            .unwrap()
            .revision
        };
        let revision = read("tasks/public.md");
        assert_eq!(built(), 1);
        // A field edit keeps every identity: the index is reused.
        let revision = patch("tasks/public.md", &revision, "title", json!("Edited"));
        assert_eq!(read("tasks/public.md"), revision);
        assert_eq!(built(), 1);
        // An authored ID changes how links find the record: rebuilt.
        let other = read("tasks/other.md");
        patch("tasks/other.md", &other, "id", json!("renamed"));
        read("tasks/public.md");
        assert_eq!(built(), 2);
    }

    #[test]
    fn restricted_metadata_reads_overlay_only_visible_records() {
        let (directory, paths) = fixture();
        let write = |path: &str, source: &str| {
            fs::write(directory.path().join(path), source).unwrap();
        };
        write(
            "_types/task.md",
            "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  value: {type: object}\ncollection:\n  unique:\n    - {field: code}\n  links:\n    related: {target_type: any, validate_exists: true}\n---\n",
        );
        // Duplicates of a hidden record, of a visible one, and of both; links
        // to a hidden record, a visible one, and nothing.
        write(
            "tasks/public.md",
            "---\ntype: task\ncode: A\nrelated: '[[tasks/private/secret]]'\n---\nSee [[secret]].\n",
        );
        write(
            "tasks/private/secret.md",
            "---\ntype: task\ncode: A\nrelated: '[[tasks/public]]'\n---\n",
        );
        write(
            "tasks/b1.md",
            "---\ntype: task\ncode: B\nrelated: '[[tasks/b2]]'\n---\n",
        );
        write("tasks/b2.md", "---\ntype: task\ncode: B\n---\n");
        write("tasks/private/b3.md", "---\ntype: task\ncode: B\n---\n");
        write(
            "tasks/c1.md",
            "---\ntype: task\ncode: C\nrelated: '[[missing]]'\n---\n",
        );
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        build_mdbase_query_report(&paths, &json!({"types": ["task"]}), None).unwrap();
        let session = MdbaseQuerySession::new(paths.clone());
        let filter = restricted();
        let mut metrics = MdbaseQueryMetrics::default();
        for path in [
            "tasks/public.md",
            "tasks/b1.md",
            "tasks/b2.md",
            "tasks/c1.md",
        ] {
            for scope in [None, Some(&filter)] {
                let expected = build_mdbase_metadata_read_report(&paths, path, scope).unwrap();
                let actual = session
                    .read_metadata_profiled(path, scope, &mut metrics)
                    .unwrap();
                assert_eq!(actual, expected, "{path} {}", scope.is_some());
                assert_eq!(metrics.indexed_hits, 1, "{path}");
            }
        }
        // The scopes do differ: the hidden duplicate and link target vanish.
        let codes = |scope| {
            session
                .read_metadata("tasks/public.md", scope)
                .unwrap()
                .record
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.clone())
                .collect::<Vec<_>>()
        };
        assert_ne!(codes(None), codes(Some(&filter)));
        // A renamed link target invalidates the retained visible index.
        fs::rename(
            directory.path().join("tasks/b2.md"),
            directory.path().join("tasks/b2-renamed.md"),
        )
        .unwrap();
        build_mdbase_query_report(&paths, &json!({"types": ["task"]}), None).unwrap();
        let expected =
            build_mdbase_metadata_read_report(&paths, "tasks/b1.md", Some(&filter)).unwrap();
        assert_eq!(
            session
                .read_metadata_profiled("tasks/b1.md", Some(&filter), &mut metrics)
                .unwrap(),
            expected
        );
        assert_eq!(metrics.indexed_hits, 1);
        assert!(expected
            .record
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.field == "related"));
        assert!(!serde_json::to_string(
            &session
                .read_metadata("tasks/b2-renamed.md", Some(&filter))
                .unwrap()
        )
        .unwrap()
        .contains("private"));
    }

    /// Retained state never outlives what it was proven for: narrowed grants,
    /// edited controls, interrupted writes, and a lost cache all give the
    /// ordinary service's answers, for queries and metadata reads alike.
    #[test]
    fn sessions_follow_revocation_control_edits_interrupted_writes_and_cache_loss() {
        fn text<T>(result: Result<T, AppError>) -> Result<T, String> {
            result.map_err(|error| error.to_string())
        }
        let (directory, paths) = initialized();
        let session = MdbaseQuerySession::new(paths.clone()).with_change_monitor(
            MdbaseChangeMonitor::watch(&directory.path().canonicalize().unwrap()).unwrap(),
            Duration::from_secs(600),
        );
        let query = json!({"types": ["task"], "order_by": [{"field": "title"}],
            "frontmatter_mode": "both"});
        let check = |scope: Option<&PermissionFilter>| {
            for _ in 0..2 {
                assert_eq!(
                    text(session.query(&query, scope)),
                    text(build_mdbase_query_report(&paths, &query, scope))
                );
                for path in ["tasks/public.md", "tasks/private/secret.md"] {
                    assert_eq!(
                        text(session.read_metadata(path, scope)),
                        text(build_mdbase_metadata_read_report(&paths, path, scope)),
                        "{path}"
                    );
                }
            }
        };
        let filter = restricted();
        check(None);
        check(Some(&filter));

        // Revocation: a narrower grant sees only what it still allows.
        let mut allow = read_control_grant();
        allow.push(ResourceSpecifier::Note("mdbase.lock.yaml".into()));
        let revoked = PermissionFilter::new(PathPermission {
            allow,
            deny: vec![ResourceSpecifier::Folder("tasks/**".into())],
        });
        check(Some(&revoked));
        assert_eq!(
            session
                .query(&query, Some(&revoked))
                .unwrap()
                .meta
                .total_count,
            0
        );
        assert!(session
            .read_metadata("tasks/public.md", Some(&revoked))
            .is_err());

        // A control edit changes defaults for every retained scope.
        let task_type = directory.path().join("_types/task.md");
        let controls = fs::read_to_string(&task_type).unwrap();
        fs::write(
            &task_type,
            controls.replace(
                "read_defaults: {status: open}",
                "read_defaults: {status: later}",
            ),
        )
        .unwrap();
        check(None);
        check(Some(&filter));
        assert_eq!(
            session
                .read_metadata("tasks/public.md", None)
                .unwrap()
                .record
                .effective_frontmatter["status"],
            "later"
        );

        // An interrupted write's journal sends every request to the ordinary
        // guarded path and its recovery error, until it is resolved.
        let journal = paths
            .operational_state_dir()
            .unwrap_or_else(|_| paths.vulcan_dir().to_path_buf())
            .join("mdbase-write/journal.json");
        fs::create_dir_all(journal.parent().unwrap()).unwrap();
        fs::write(&journal, "{}").unwrap();
        assert!(session.query(&query, None).is_err());
        assert!(session.read_metadata("tasks/public.md", None).is_err());
        check(None);
        fs::remove_file(&journal).unwrap();
        check(None);

        // A lost cache, then a cooperating edit (external edits are observed
        // only once their notification arrives): answers follow the sources.
        for name in ["cache.db", "cache.db-wal", "cache.db-shm"] {
            let _ = fs::remove_file(paths.vulcan_dir().join(name));
        }
        check(None);
        check(Some(&filter));
        {
            let _lock = vulcan_core::write_lock::acquire_write_lock(&paths).unwrap();
            fs::write(
                directory.path().join("tasks/public.md"),
                "---\ntype: task\ntitle: After loss\n---\nBody\n",
            )
            .unwrap();
        }
        check(None);
        check(Some(&filter));
        assert_eq!(
            session
                .read_metadata("tasks/public.md", None)
                .unwrap()
                .record
                .frontmatter["title"],
            "After loss"
        );
    }

    /// MDB.10 work bounds: a warm request rederives nothing, reads no record
    /// source, and decodes no retained row again; one edited record costs one
    /// decoded row, and only the returned page is hydrated.
    #[test]
    fn warm_requests_do_bounded_work_independent_of_collection_size() {
        let (directory, paths) = initialized();
        for index in 0..40 {
            fs::write(
                directory.path().join(format!("tasks/t{index:02}.md")),
                format!("---\ntype: task\ntitle: T{index:02}\nstatus: open\n---\nBody\n"),
            )
            .unwrap();
        }
        let query = json!({"types": ["task"], "where": "status == 'open'",
            "order_by": [{"field": "title"}], "select": ["title"], "limit": 5});
        let expected = build_mdbase_query_report(&paths, &query, None).unwrap();
        let session = MdbaseQuerySession::new(paths.clone());
        let mut metrics = MdbaseQueryMetrics::default();
        session.query_profiled(&query, None, &mut metrics).unwrap();
        let visible = metrics.indexed.visible_records;
        assert_eq!(visible, 42);
        assert_eq!(metrics.indexed.reloaded_rows, visible);

        let warm = |metrics: &MdbaseQueryMetrics| {
            assert_eq!(metrics.indexed_hits, 1);
            assert_eq!(metrics.source_loads, 0);
            assert_eq!(metrics.completed_manifests, 0);
            assert_eq!(metrics.cache_attempts, 0);
            assert_eq!(metrics.cache_refresh_attempts, 0);
        };
        assert_eq!(
            session.query_profiled(&query, None, &mut metrics).unwrap(),
            expected
        );
        warm(&metrics);
        assert_eq!(metrics.indexed.reloaded_rows, 0);
        assert_eq!(metrics.indexed.type_candidates, visible);
        assert_eq!(metrics.indexed.sql_decided, visible);
        assert_eq!(metrics.indexed.residual_evaluations, 0);
        assert_eq!(metrics.indexed.hydrated, 5);
        session
            .read_metadata_profiled("tasks/t07.md", None, &mut metrics)
            .unwrap();
        warm(&metrics);
        assert_eq!(metrics.indexed.reloaded_rows, 0);

        // One edit, published by an authorized refresh, decodes one row.
        fs::write(
            directory.path().join("tasks/t03.md"),
            "---\ntype: task\ntitle: T03\nstatus: done\n---\nBody\n",
        )
        .unwrap();
        build_mdbase_query_report(&paths, &query, None).unwrap();
        let report = session.query_profiled(&query, None, &mut metrics).unwrap();
        warm(&metrics);
        assert_eq!(metrics.indexed.reloaded_rows, 1);
        assert_eq!(
            report,
            build_mdbase_query_report(&paths, &query, None).unwrap()
        );
    }

    #[test]
    fn concurrent_readers_share_a_session_and_observe_concurrent_writes() {
        fn shareable<T: Send + Sync>() {}
        shareable::<MdbaseQuerySession>();
        // Strict sessions: these assertions do not need watched trust, and
        // inotify instances are a scarce per-user resource on test hosts.
        let (directory, paths) = initialized();
        let session = MdbaseQuerySession::new(paths.clone());
        let filter = restricted();
        let expected = queries()
            .iter()
            .map(|query| {
                [None, Some(&filter)]
                    .map(|scope| build_mdbase_query_report(&paths, query, scope).unwrap())
            })
            .collect::<Vec<_>>();
        std::thread::scope(|threads| {
            for reader in 0..8 {
                let (session, filter, expected) = (&session, &filter, &expected);
                threads.spawn(move || {
                    for round in 0..10 {
                        let index = (reader + round) % expected.len();
                        let scoped = (reader % 2 == 1).then_some(filter);
                        assert_eq!(
                            session.query(&queries()[index], scoped).unwrap(),
                            expected[index][usize::from(scoped.is_some())]
                        );
                    }
                });
            }
        });

        // Readers racing cooperating writes see either state, then the last.
        let query = json!({"types": ["task"], "where": "title == 'Public'", "select": ["title"]});
        let record = directory.path().join("tasks/public.md");
        std::thread::scope(|threads| {
            for _ in 0..4 {
                let (session, query) = (&session, &query);
                threads.spawn(move || {
                    for _ in 0..20 {
                        let total = session.query(query, None).unwrap().meta.total_count;
                        assert!(total <= 1);
                    }
                });
            }
            for title in ["Edited", "Public", "Final"] {
                let _lock = vulcan_core::write_lock::acquire_write_lock(&paths).unwrap();
                fs::write(
                    &record,
                    format!("---\ntype: task\ntitle: {title}\n---\nBody\n"),
                )
                .unwrap();
            }
        });
        assert_eq!(
            session.query(&query, None).unwrap(),
            build_mdbase_query_report(&paths, &query, None).unwrap()
        );
        assert_eq!(session.query(&query, None).unwrap().meta.total_count, 0);

        // Retained readers do not queue behind a write section; they observe
        // the state from before it.
        let before = session.query(&query, None).unwrap();
        let writer = vulcan_core::write_lock::acquire_write_lock(&paths).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::scope(|threads| {
            let (session, query) = (&session, &query);
            threads.spawn(move || sender.send(session.query(query, None).unwrap()).unwrap());
            let during = receiver.recv_timeout(Duration::from_secs(10));
            drop(writer);
            assert_eq!(during.expect("reader blocked by the write section"), before);
        });
    }
}
