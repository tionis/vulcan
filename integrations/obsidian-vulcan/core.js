"use strict";

const DEFAULT_SETTINGS = Object.freeze({
  baseUrl: "http://127.0.0.1:3210",
  wikiId: "",
  syncOnSave: false,
  saveDebounceMs: 1500,
  eventStream: true,
});

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
  };
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
  sanitizeSettings,
  statusPresentation,
};
