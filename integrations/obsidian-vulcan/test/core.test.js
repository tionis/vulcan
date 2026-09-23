"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const {
  SaveSyncCoordinator,
  SyncFailureAlertTracker,
  sanitizeSettings,
  syncFailureAlert,
  statusPresentation,
} = require("../core");

test("settings persistence is allowlisted and cannot retain bearer material", () => {
  assert.deepEqual(sanitizeSettings({
    baseUrl: "http://127.0.0.1:4000",
    wikiId: "personal",
    syncOnSave: true,
    saveDebounceMs: 1,
    eventStream: false,
    notifyOnFailure: false,
    networkFailureNotifications: "count",
    networkFailureCount: 4,
    networkFailureMinutes: 20,
    token: "must-not-survive",
  }), {
    baseUrl: "http://127.0.0.1:4000",
    wikiId: "personal",
    syncOnSave: true,
    saveDebounceMs: 250,
    eventStream: false,
    notifyOnFailure: false,
    networkFailureNotifications: "count",
    networkFailureCount: 4,
    networkFailureMinutes: 20,
  });
  assert.equal(sanitizeSettings({}).networkFailureNotifications, "immediate");
  assert.equal(sanitizeSettings({}).networkFailureCount, 3);
  assert.equal(sanitizeSettings({}).networkFailureMinutes, 15);
  assert.equal(sanitizeSettings({ networkFailureNotifications: "invalid" }).networkFailureNotifications, "immediate");
  assert.equal(sanitizeSettings({ networkFailureCount: 0 }).networkFailureCount, 2);
  assert.equal(sanitizeSettings({ networkFailureMinutes: 2000 }).networkFailureMinutes, 1440);
});

test("failed synchronization alerts are authoritative, bounded, and deduplicated", () => {
  const tracker = new SyncFailureAlertTracker();
  const status = {
    state: "offline",
    detail: "fallback",
    job: {
      id: "job-1",
      state: "failed",
      error: {
        category: "network_unavailable",
        message: ` remote failed\n${"x".repeat(300)} `,
      },
    },
  };
  const first = tracker.observe(status);
  assert.equal(first.key, "job:job-1");
  assert.match(first.message, /^Vulcan sync failed \(network unavailable\): remote failed x+…$/);
  assert.ok(first.message.length < 300);
  assert.equal(tracker.observe(status), null);

  status.job.id = "job-2";
  assert.equal(tracker.observe(status).key, "job:job-2");
  assert.equal(tracker.observe({ state: "offline", detail: "daemon unavailable" }), null);
});

test("retained journal errors alert once per transaction", () => {
  const tracker = new SyncFailureAlertTracker();
  const status = {
    state: "error",
    wiki_id: "personal",
    transaction_id: "tx-1",
    detail: "worktree changed during apply",
  };
  assert.equal(tracker.observe(status).key, "transaction:tx-1");
  assert.equal(tracker.observe(status), null);
  assert.equal(syncFailureAlert({ state: "dirty" }), null);
  assert.equal(syncFailureAlert({ state: "error", job: { id: "active", state: "running" } }), null);
});

test("status presentation distinguishes active, conflict, and terminal states", () => {
  assert.deepEqual(statusPresentation({ state: "applying" }), {
    state: "applying",
    busy: true,
    label: "Vulcan: applying",
  });
  assert.equal(statusPresentation({ state: "conflicted", unresolved_conflicts: 2 }).label, "Vulcan: 2 conflicts");
  assert.equal(statusPresentation({ state: "clean" }).label, "Vulcan: synced");
});

test("save coordination debounces writes into one ordinary sync request", async () => {
  let scheduled;
  let clearCount = 0;
  let syncCount = 0;
  const coordinator = new SaveSyncCoordinator({
    delayMs: 500,
    status: async () => ({ state: "dirty" }),
    sync: async () => { syncCount += 1; },
    setTimer: (callback) => { scheduled = callback; return Symbol("timer"); },
    clearTimer: () => { clearCount += 1; },
  });
  coordinator.notifySave();
  coordinator.notifySave();
  assert.equal(clearCount, 1);
  scheduled();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(syncCount, 1);
  coordinator.dispose();
});

test("save coordination never races applying, paused, or conflicted daemon state", async () => {
  for (const state of ["applying", "paused", "conflicted"]) {
    let syncCount = 0;
    const coordinator = new SaveSyncCoordinator({
      delayMs: 250,
      status: async () => ({ state }),
      sync: async () => { syncCount += 1; },
    });
    coordinator.pending = true;
    assert.equal(await coordinator.flush(), false);
    assert.equal(syncCount, 0);
    coordinator.dispose();
  }
});

test("the Obsidian adapter delegates to the daemon and uses SecretStorage", () => {
  const source = fs.readFileSync(path.join(__dirname, "..", "plugin.js"), "utf8");
  assert.match(source, /secretStorage\.getSecret\(TOKEN_SECRET_ID\)/);
  assert.match(source, /secretStorage\.setSecret\(TOKEN_SECRET_ID/);
  assert.match(source, /client\.sync\(this\.settings\.wikiId\)/);
  assert.match(source, /failureAlerts\.observe\(status, this\.settings\)/);
  assert.match(source, /detail\(details, "Error category", job\.error\.category\)/);
  assert.match(source, /Network failure notices/);
  assert.doesNotMatch(source, /child_process|execFile|spawn\(|\bgit\s/);
});

function failedJob(id, category = "network") {
  return { state: "offline", wiki_id: "personal", job: {
    id, state: "failed", error: { category, retryable: category === "network", message: `${category} failure` },
  } };
}

test("network failure count notices once per outage and resets after recovery", () => {
  const tracker = new SyncFailureAlertTracker();
  const settings = sanitizeSettings({ networkFailureNotifications: "count", networkFailureCount: 3 });
  assert.equal(tracker.observe(failedJob("one"), settings), null);
  assert.equal(tracker.observe(failedJob("one"), settings), null);
  assert.equal(tracker.observe({ state: "fetching", job: { id: "two", state: "running" } }, settings), null);
  assert.equal(tracker.observe(failedJob("two"), settings), null);
  assert.match(tracker.observe(failedJob("three"), settings).message, /after 3 consecutive network failures$/);
  assert.equal(tracker.observe(failedJob("four"), settings), null);
  tracker.observe({ state: "clean", job: { id: "healthy", state: "succeeded" } }, settings);
  assert.equal(tracker.observe(failedJob("five"), settings), null);
  assert.equal(tracker.observe(failedJob("six", "authentication"), settings).network, false);
  assert.equal(tracker.observe(failedJob("seven"), settings), null);
});

test("network duration can mature while the same failed job remains current", () => {
  let now = 0;
  const tracker = new SyncFailureAlertTracker(64, () => now);
  const settings = sanitizeSettings({ networkFailureNotifications: "duration", networkFailureMinutes: 2 });
  const status = failedJob("one");
  assert.equal(tracker.observe(status, settings), null);
  now = 119_999;
  assert.equal(tracker.observe(status, settings), null);
  now = 120_000;
  assert.match(tracker.observe(status, settings).message, /for at least 2 minutes$/);
  assert.equal(tracker.observe(status, settings), null);
  assert.equal(tracker.observe(failedJob("two"), settings), null);
});

test("ignoring network failures leaves other failures visible", () => {
  const tracker = new SyncFailureAlertTracker();
  const settings = sanitizeSettings({ networkFailureNotifications: "ignore" });
  assert.equal(tracker.observe(failedJob("one"), settings), null);
  assert.equal(tracker.observe(failedJob("two"), settings), null);
  assert.match(tracker.observe(failedJob("three", "repository"), settings).message, /repository failure/);
});
