//! Live health of each vault's observer and observation consumers
//! (Roadmap 10.4), recorded by the trigger runtime and reported by
//! `GET /health` and `GET /{id}/health`.

use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// The longest error text kept per component.
const MAX_ERROR_CHARS: usize = 2_000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ComponentHealth {
    pub running: bool,
    /// How often the component exited and was restarted.
    pub restarts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_unix_ms: Option<u64>,
}

/// One vault's observer and consumers, by name (`observer`, `index`,
/// `sync`, `auto-commit`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct VaultWatcherHealth {
    pub components: BTreeMap<String, ComponentHealth>,
}

impl VaultWatcherHealth {
    /// Whether every component that should run is running.
    #[must_use]
    pub fn healthy(&self) -> bool {
        self.components.values().all(|component| component.running)
    }
}

#[derive(Debug, Clone, Default)]
pub struct WatcherHealthRegistry {
    vaults: Arc<Mutex<BTreeMap<String, VaultWatcherHealth>>>,
}

impl WatcherHealthRegistry {
    pub fn started(&self, wiki_id: &str, component: &str) {
        self.update(wiki_id, component, |health| health.running = true);
    }

    /// Records that `component` exited with `detail`; it counts as a
    /// restart once it starts again.
    pub fn exited(&self, wiki_id: &str, component: &str, detail: &str) {
        let detail = detail.chars().take(MAX_ERROR_CHARS).collect::<String>();
        self.update(wiki_id, component, |health| {
            health.running = false;
            health.restarts = health.restarts.saturating_add(1);
            health.last_error = Some(detail);
            health.last_error_unix_ms = Some(unix_time_ms());
        });
    }

    /// Records whether `component` runs now, without counting a restart;
    /// `None` means the registration does not want it.
    pub fn observe(&self, wiki_id: &str, component: &str, running: Option<bool>) {
        match running {
            Some(running) => self.update(wiki_id, component, |health| health.running = running),
            None => self.stopped(wiki_id, component),
        }
    }

    /// Forgets every vault not in `wiki_ids`.
    pub fn retain(&self, wiki_ids: &std::collections::BTreeSet<String>) {
        lock(&self.vaults).retain(|id, _| wiki_ids.contains(id));
    }

    /// Forgets a component the registration no longer wants.
    pub fn stopped(&self, wiki_id: &str, component: &str) {
        let mut vaults = lock(&self.vaults);
        if let Some(vault) = vaults.get_mut(wiki_id) {
            vault.components.remove(component);
        }
    }

    /// Forgets a vault whose watcher stopped for good (unregistered).
    pub fn remove(&self, wiki_id: &str) {
        lock(&self.vaults).remove(wiki_id);
    }

    #[must_use]
    pub fn get(&self, wiki_id: &str) -> Option<VaultWatcherHealth> {
        lock(&self.vaults).get(wiki_id).cloned()
    }

    fn update(&self, wiki_id: &str, component: &str, change: impl FnOnce(&mut ComponentHealth)) {
        let mut vaults = lock(&self.vaults);
        change(
            vaults
                .entry(wiki_id.to_string())
                .or_default()
                .components
                .entry(component.to_string())
                .or_default(),
        );
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exits_are_counted_and_kept_until_the_component_is_back() {
        let registry = WatcherHealthRegistry::default();
        registry.started("notes", "observer");
        registry.started("notes", "index");
        assert!(registry.get("notes").unwrap().healthy());

        registry.exited(
            "notes",
            "index",
            "index observation consumer failed: disk full",
        );
        let health = registry.get("notes").unwrap();
        assert!(!health.healthy());
        let index = &health.components["index"];
        assert_eq!(index.restarts, 1);
        assert!(index.last_error.as_deref().unwrap().contains("disk full"));

        registry.started("notes", "index");
        let index = registry.get("notes").unwrap().components["index"].clone();
        assert!(index.running && index.restarts == 1 && index.last_error.is_some());

        registry.stopped("notes", "index");
        assert!(!registry
            .get("notes")
            .unwrap()
            .components
            .contains_key("index"));
        registry.remove("notes");
        assert!(registry.get("notes").is_none());
    }

    #[test]
    fn long_errors_are_bounded() {
        let registry = WatcherHealthRegistry::default();
        registry.exited("notes", "sync", &"x".repeat(10_000));
        let error = registry.get("notes").unwrap().components["sync"]
            .last_error
            .clone()
            .unwrap();
        assert_eq!(error.chars().count(), MAX_ERROR_CHARS);
    }
}
