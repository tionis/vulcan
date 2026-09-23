"use strict";

const DEFAULT_SETTINGS = Object.freeze({
  baseUrl: "http://127.0.0.1:3210",
  wikiId: "",
  syncOnSave: false,
  saveDebounceMs: 1500,
  eventStream: true,
  notifyOnFailure: true,
  networkFailureNotifications: "immediate",
  networkFailureCount: 3,
  networkFailureMinutes: 15,
});

const MAX_FAILURE_DETAIL_CHARS = 240;

const BUSY_STATES = new Set([
  "capture_pending",
  "capturing",
  "captured_unpushed",
  "fetching",
  "fetched",
  "merging",
  "pushing",
  "applying",
]);

function sanitizeSettings(value) {
  const input = value && typeof value === "object" ? value : {};
  const debounce = Number(input.saveDebounceMs);
  return {
    baseUrl: typeof input.baseUrl === "string" ? input.baseUrl : DEFAULT_SETTINGS.baseUrl,
    wikiId: typeof input.wikiId === "string" ? input.wikiId : "",
    syncOnSave: input.syncOnSave === true,
    saveDebounceMs: Number.isFinite(debounce)
      ? Math.min(60_000, Math.max(250, Math.round(debounce)))
      : DEFAULT_SETTINGS.saveDebounceMs,
    eventStream: input.eventStream !== false,
    notifyOnFailure: input.notifyOnFailure !== false,
    networkFailureNotifications: ["immediate", "ignore", "count", "duration"].includes(input.networkFailureNotifications)
      ? input.networkFailureNotifications : DEFAULT_SETTINGS.networkFailureNotifications,
    networkFailureCount: boundedInteger(input.networkFailureCount, 2, 100, DEFAULT_SETTINGS.networkFailureCount),
    networkFailureMinutes: boundedInteger(input.networkFailureMinutes, 1, 1440, DEFAULT_SETTINGS.networkFailureMinutes),
  };
}

function boundedInteger(value, minimum, maximum, fallback) {
  if (value === "" || value === null || value === undefined) return fallback;
  const number = Number(value);
  return Number.isFinite(number) ? Math.min(maximum, Math.max(minimum, Math.round(number))) : fallback;
}

function boundedFailureDetail(value) {
  const detail = typeof value === "string" ? value.replace(/\s+/g, " ").trim() : "";
  if (!detail) return "Open synchronization status for details.";
  if (detail.length <= MAX_FAILURE_DETAIL_CHARS) return detail;
  return `${detail.slice(0, MAX_FAILURE_DETAIL_CHARS - 1)}…`;
}

function syncFailureAlert(status) {
  if (!status || typeof status !== "object") return null;
  const job = status.job;
  if (job && typeof job === "object" && job.state === "failed" && typeof job.id === "string" && job.id) {
    const error = job.error && typeof job.error === "object" ? job.error : {};
    const category = typeof error.category === "string" && error.category
      ? ` (${error.category.replaceAll("_", " ")})`
      : "";
    return {
      key: `job:${job.id}`,
      network: error.category === "network",
      message: `Vulcan sync failed${category}${error.retryable ? " (retryable)" : ""}: ${boundedFailureDetail(error.message || status.detail)}`,
    };
  }
  if (job && typeof job === "object") return null;
  if (status.state !== "error") return null;
  const transactionId = typeof status.transaction_id === "string" && status.transaction_id
    ? status.transaction_id
    : `${status.wiki_id || "unknown"}:${boundedFailureDetail(status.detail)}`;
  return {
    key: `transaction:${transactionId}`,
    network: false,
    message: `Vulcan synchronization failed: ${boundedFailureDetail(status.detail)}`,
  };
}

class SyncFailureAlertTracker {
  constructor(limit = 64, now = Date.now) {
    this.limit = Math.max(1, limit);
    this.now = now;
    this.seen = new Set();
    this.order = [];
    this.networkKeys = new Set();
    this.networkSince = null;
    this.networkNotified = false;
    this.policy = null;
  }

  observe(status, settings = DEFAULT_SETTINGS) {
    const policy = `${settings.notifyOnFailure}:${settings.networkFailureNotifications}:${settings.networkFailureCount}:${settings.networkFailureMinutes}`;
    if (policy !== this.policy) {
      this.resetNetwork();
      this.policy = policy;
    }
    const alert = syncFailureAlert(status);
    if (!alert) {
      if (!status || !status.job || ["succeeded", "conflicted", "paused", "cancelled"].includes(status.job.state)) {
        this.resetNetwork();
      }
      return null;
    }
    if (!alert.network) this.resetNetwork();
    if (!settings.notifyOnFailure) return null;
    const alreadySeen = this.seen.has(alert.key);
    if (alreadySeen && (!alert.network || settings.networkFailureNotifications !== "duration")) return null;
    if (!alreadySeen) {
      this.seen.add(alert.key);
      this.order.push(alert.key);
      if (this.order.length > this.limit) this.seen.delete(this.order.shift());
    }
    if (alert.network) {
      if (this.networkSince === null) this.networkSince = this.now();
      if (settings.networkFailureNotifications === "count" && !this.networkNotified) {
        this.networkKeys.add(alert.key);
      }
      switch (settings.networkFailureNotifications) {
        case "ignore": return null;
        case "count":
          if (this.networkNotified || this.networkKeys.size < settings.networkFailureCount) return null;
          break;
        case "duration":
          if (this.networkNotified || this.now() - this.networkSince < settings.networkFailureMinutes * 60_000) return null;
          break;
        default: return alreadySeen ? null : alert;
      }
      this.networkNotified = true;
      const context = settings.networkFailureNotifications === "count"
        ? ` after ${this.networkKeys.size} consecutive network failures`
        : ` for at least ${settings.networkFailureMinutes} minute${settings.networkFailureMinutes === 1 ? "" : "s"}`;
      return { ...alert, message: `${alert.message}${context}` };
    }
    return alert;
  }

  resetNetwork() {
    this.networkKeys.clear();
    this.networkSince = null;
    this.networkNotified = false;
  }
}

function statusPresentation(status) {
  const state = status && typeof status.state === "string" ? status.state : "unknown";
  const conflicts = Number(status && status.unresolved_conflicts) || 0;
  const labels = {
    clean: "Vulcan: synced",
    dirty: "Vulcan: changes pending",
    paused: "Vulcan: paused",
    offline: "Vulcan: offline",
    conflicted: `Vulcan: ${conflicts || ""} conflict${conflicts === 1 ? "" : "s"}`.replace("  ", " "),
    error: "Vulcan: error",
    unknown: "Vulcan: not configured",
  };
  return {
    state,
    busy: BUSY_STATES.has(state),
    label: labels[state] || `Vulcan: ${state.replaceAll("_", " ")}`,
  };
}

class SaveSyncCoordinator {
  constructor({ delayMs, status, sync, onError, setTimer = setTimeout, clearTimer = clearTimeout }) {
    this.delayMs = delayMs;
    this.status = status;
    this.sync = sync;
    this.onError = onError || (() => {});
    this.setTimer = setTimer;
    this.clearTimer = clearTimer;
    this.timer = null;
    this.running = false;
    this.pending = false;
    this.disposed = false;
  }

  notifySave() {
    if (this.disposed) return;
    this.pending = true;
    if (this.timer !== null) this.clearTimer(this.timer);
    this.timer = this.setTimer(() => {
      this.timer = null;
      void this.flush();
    }, this.delayMs);
  }

  async flush() {
    if (this.disposed || this.running || !this.pending) return false;
    this.pending = false;
    this.running = true;
    try {
      const status = await this.status();
      const state = statusPresentation(status).state;
      if (BUSY_STATES.has(state) || state === "paused" || state === "conflicted") {
        return false;
      }
      await this.sync();
      return true;
    } catch (error) {
      this.onError(error);
      return false;
    } finally {
      this.running = false;
      if (this.pending && !this.disposed) this.notifySave();
    }
  }

  dispose() {
    this.disposed = true;
    this.pending = false;
    if (this.timer !== null) this.clearTimer(this.timer);
    this.timer = null;
  }
}

module.exports = {
  BUSY_STATES,
  DEFAULT_SETTINGS,
  SaveSyncCoordinator,
  SyncFailureAlertTracker,
  sanitizeSettings,
  syncFailureAlert,
  statusPresentation,
};
