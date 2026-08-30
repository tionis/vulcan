"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const {
  VulcanCompanionClient,
  VulcanProtocolError,
  normalizeBaseUrl,
} = require("../protocol");

const TOKEN = "a".repeat(43);

test("loopback validation rejects remote, credential-bearing, and path endpoints", () => {
  assert.equal(normalizeBaseUrl("http://127.0.0.1:3210"), "http://127.0.0.1:3210");
  assert.equal(normalizeBaseUrl("http://[::1]:3210"), "http://[::1]:3210");
  assert.throws(() => normalizeBaseUrl("https://127.0.0.1:3210"), /loopback/);
  assert.throws(() => normalizeBaseUrl("http://example.com:3210"), /loopback/);
  assert.throws(() => normalizeBaseUrl("http://token@127.0.0.1:3210"), /only a loopback origin/);
  assert.throws(() => normalizeBaseUrl("http://127.0.0.1:3210/api"), /only a loopback origin/);
});

test("manual sync carries authentication, protocol, and idempotency contracts", async () => {
  const calls = [];
  const client = new VulcanCompanionClient({
    baseUrl: "http://127.0.0.1:3210",
    token: TOKEN,
    idempotencyKey: () => "obsidian-fixed",
    request: async (request) => {
      calls.push(request);
      return { status: 202, json: { replayed: false, job: { id: "job-1" } } };
    },
  });

  const report = await client.sync("personal notes");
  assert.equal(report.job.id, "job-1");
  assert.deepEqual(calls, [{
    url: "http://127.0.0.1:3210/personal%20notes/sync",
    method: "POST",
    headers: {
      Authorization: `Bearer ${TOKEN}`,
      "Vulcan-Protocol-Version": "1",
      "Idempotency-Key": "obsidian-fixed",
    },
    body: undefined,
  }]);
});

test("conflict resolution always uses an explicit side and dry-run value", async () => {
  const calls = [];
  const client = new VulcanCompanionClient({
    baseUrl: "http://localhost:3210",
    token: TOKEN,
    request: async (request) => {
      calls.push(request);
      return { status: 200, json: { dry_run: true } };
    },
  });
  await client.resolveConflict("wiki", "conflict/id", "local", true);
  assert.equal(calls[0].url, "http://localhost:3210/wiki/sync/conflicts/conflict%2Fid/resolve");
  assert.equal(calls[0].body, JSON.stringify({ side: "local", dry_run: true }));
  assert.throws(() => client.resolveConflict("wiki", "id", "winner", false), /base, local, or remote/);
});

test("versioned daemon errors remain typed and do not expose response bodies", async () => {
  const client = new VulcanCompanionClient({
    baseUrl: "http://127.0.0.1:3210",
    token: TOKEN,
    request: async () => ({
      status: 409,
      json: { version: 1, kind: "conflict", detail: "stale accepted ref" },
      text: "secret unrelated body",
    }),
  });
  await assert.rejects(
    client.pause("wiki"),
    (error) => error instanceof VulcanProtocolError
      && error.kind === "conflict"
      && error.status === 409
      && error.message === "stale accepted ref",
  );
});

test("event stream uses bearer subprotocol and accepts only v1 state snapshots", () => {
  class FakeWebSocket {
    constructor(url, protocols) {
      this.url = url;
      this.protocols = protocols;
      FakeWebSocket.instance = this;
    }
  }
  const snapshots = [];
  const errors = [];
  const client = new VulcanCompanionClient({
    baseUrl: "http://127.0.0.1:3210",
    token: TOKEN,
    request: async () => ({ status: 200, json: {} }),
    WebSocketCtor: FakeWebSocket,
  });
  client.connectEvents({ onSnapshot: (value) => snapshots.push(value), onError: (error) => errors.push(error) });
  assert.equal(FakeWebSocket.instance.url, "ws://127.0.0.1:3210/events");
  assert.deepEqual(FakeWebSocket.instance.protocols, ["vulcan.v1", `vulcan.bearer.${TOKEN}`]);

  FakeWebSocket.instance.onmessage({ data: JSON.stringify({ version: 1, event: "state_snapshot", statuses: [] }) });
  FakeWebSocket.instance.onmessage({ data: JSON.stringify({ version: 2, event: "state_snapshot" }) });
  assert.equal(snapshots.length, 1);
  assert.equal(errors.length, 1);
});
