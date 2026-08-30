"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const {
  SaveSyncCoordinator,
  sanitizeSettings,
  statusPresentation,
} = require("../core");

test("settings persistence is allowlisted and cannot retain bearer material", () => {
  assert.deepEqual(sanitizeSettings({
    baseUrl: "http://127.0.0.1:4000",
    wikiId: "personal",
    syncOnSave: true,
    saveDebounceMs: 1,
    eventStream: false,
    token: "must-not-survive",
  }), {
    baseUrl: "http://127.0.0.1:4000",
    wikiId: "personal",
    syncOnSave: true,
    saveDebounceMs: 250,
    eventStream: false,
  });
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
  assert.doesNotMatch(source, /child_process|execFile|spawn\(|\bgit\s/);
});
