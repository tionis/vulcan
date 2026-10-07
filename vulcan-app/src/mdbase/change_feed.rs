//! Logical mdbase change notifications for long-lived hosts (MDB.9).
//!
//! A watching host reports each scan's changed paths. The feed keeps only
//! changes that matter to the collection (configuration, type and contract
//! files, schema documents, and records), brings the derived record cache up
//! to date, and only then publishes a numbered notification. A reader who
//! sees a notification therefore reads state at least as new as it.
//! Notifications are bounded in memory; a reader that falls behind the
//! retained window is told to reconcile instead of receiving a partial list.

use super::{load_collection_authorized, AppError, VaultPaths};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use vulcan_core::mdbase::{
    is_mdbase_record_path, load_mdbase_collection, rebuild_mdbase_record_cache,
    refresh_mdbase_record_cache, MDBASE_CONFIG_FILE_NAME, MDBASE_LOCK_FILE_NAME,
};
use vulcan_core::PermissionFilter;

/// Notifications retained for readers that poll.
pub const MDBASE_CHANGE_FEED_CAPACITY: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseChangeNotification {
    /// Strictly increasing within one feed.
    pub generation: u64,
    /// Configuration, type, contract, or schema files changed: types,
    /// effective schemas, and every query result may differ.
    pub controls_changed: bool,
    /// Changed record and control paths, collection-relative and sorted.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseChangesReport {
    /// Notifications after the requested generation that the caller may see.
    pub notifications: Vec<MdbaseChangeNotification>,
    /// The newest published generation; pass it as `after` next time.
    pub generation: u64,
    /// Older notifications were dropped: re-read everything of interest.
    pub reconcile: bool,
}

#[derive(Debug, Default)]
pub struct MdbaseChangeFeed {
    state: Mutex<FeedState>,
}

#[derive(Debug, Default)]
struct FeedState {
    generation: u64,
    retained: VecDeque<MdbaseChangeNotification>,
    /// Relevant changes whose refresh failed, announced with the next
    /// successful observation instead of being lost.
    pending: Option<(bool, Vec<String>)>,
}

impl MdbaseChangeFeed {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe one scan's changed vault paths. Returns the published
    /// generation, or `None` when nothing mdbase-relevant changed. A failed
    /// cache refresh publishes nothing; the next observation retries.
    pub fn observe(&self, paths: &VaultPaths, changed: &[String]) -> Result<Option<u64>, AppError> {
        let pending = self.lock()?.pending.take();
        let classified = classify(paths, changed);
        let (controls_changed, relevant) = match (pending, classified) {
            (None, None) => return Ok(None),
            (Some(change), None) | (None, Some(change)) => change,
            (Some((pending_controls, mut pending_paths)), Some((controls, fresh))) => {
                pending_paths.extend(fresh);
                pending_paths.sort();
                pending_paths.dedup();
                (pending_controls || controls, pending_paths)
            }
        };
        if let Err(error) = refresh_derived_state(paths) {
            self.lock()?.pending = Some((controls_changed, relevant));
            return Err(error);
        }
        if load_collection_authorized(paths, None).is_err() && !controls_changed {
            return Ok(None);
        }
        let mut state = self.lock()?;
        state.generation += 1;
        let generation = state.generation;
        if state.retained.len() == MDBASE_CHANGE_FEED_CAPACITY {
            state.retained.pop_front();
        }
        state.retained.push_back(MdbaseChangeNotification {
            generation,
            controls_changed,
            paths: relevant,
        });
        Ok(Some(generation))
    }

    /// Notifications after `after` that the caller may see. Reading requires
    /// the same control authority as any mdbase read; record paths outside
    /// the caller's read scope are removed, and notifications left with
    /// nothing visible are omitted.
    pub fn changes_since(
        &self,
        paths: &VaultPaths,
        after: u64,
        filter: Option<&PermissionFilter>,
    ) -> Result<MdbaseChangesReport, AppError> {
        load_collection_authorized(paths, filter)?;
        let state = self.lock()?;
        let oldest = state
            .retained
            .front()
            .map_or(state.generation + 1, |notification| notification.generation);
        Ok(MdbaseChangesReport {
            reconcile: after.saturating_add(1) < oldest && after < state.generation,
            generation: state.generation,
            notifications: state
                .retained
                .iter()
                .filter(|notification| notification.generation > after)
                .filter_map(|notification| {
                    let paths = notification
                        .paths
                        .iter()
                        .filter(|path| filter.is_none_or(|filter| filter.is_allowed(path)))
                        .cloned()
                        .collect::<Vec<_>>();
                    (notification.controls_changed || !paths.is_empty()).then_some(
                        MdbaseChangeNotification {
                            generation: notification.generation,
                            controls_changed: notification.controls_changed,
                            paths,
                        },
                    )
                })
                .collect(),
        })
    }
}

impl MdbaseChangeFeed {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, FeedState>, AppError> {
        self.state
            .lock()
            .map_err(|_| AppError::operation("mdbase change feed is unavailable"))
    }
}

/// Bring the derived record cache up to date before anything is announced.
/// An invalid collection has no record state to refresh: readers now get its
/// errors, which is consistent, so a control change is still announced.
fn refresh_derived_state(paths: &VaultPaths) -> Result<(), AppError> {
    let Ok(loaded) = load_collection_authorized(paths, None) else {
        return Ok(());
    };
    if !paths.cache_db().exists() {
        return Ok(());
    }
    let mut database = vulcan_core::CacheDatabase::open(paths).map_err(AppError::operation)?;
    // As the query path does: rebuild when an incremental refresh cannot
    // apply (no published rows yet, or damaged ones).
    refresh_mdbase_record_cache(
        &mut database,
        &loaded.collection,
        &loaded.types,
        &loaded.contracts,
    )
    .or_else(|_| {
        rebuild_mdbase_record_cache(
            &mut database,
            &loaded.collection,
            &loaded.types,
            &loaded.contracts,
        )
    })
    .map(|_| ())
    .map_err(AppError::operation)
}

/// Split changed paths into control changes and record changes. `None` when
/// nothing touches the collection. Schema documents may live anywhere a type
/// references, so any JSON or YAML file change counts as a control change.
fn classify(paths: &VaultPaths, changed: &[String]) -> Option<(bool, Vec<String>)> {
    let collection = load_mdbase_collection(paths.vault_root()).ok().flatten();
    let mut controls_changed = false;
    let mut relevant = Vec::new();
    for path in changed {
        let control = path == MDBASE_CONFIG_FILE_NAME
            || path == MDBASE_LOCK_FILE_NAME
            || collection.as_ref().is_some_and(|collection| {
                let settings = &collection.config.settings;
                [&settings.types_folder, &settings.contracts_folder]
                    .into_iter()
                    .any(|folder| path.starts_with(&format!("{folder}/")))
            })
            || (collection.is_some()
                && std::path::Path::new(path)
                    .extension()
                    .is_some_and(|extension| {
                        ["json", "yaml", "yml"]
                            .iter()
                            .any(|kind| extension.eq_ignore_ascii_case(kind))
                    }));
        let record = !control
            && collection
                .as_ref()
                .is_some_and(|collection| is_mdbase_record_path(collection, path).unwrap_or(false));
        controls_changed |= control;
        if control || record {
            relevant.push(path.clone());
        }
    }
    relevant.sort();
    relevant.dedup();
    (!relevant.is_empty()).then_some((controls_changed, relevant))
}

#[cfg(test)]
mod tests;
