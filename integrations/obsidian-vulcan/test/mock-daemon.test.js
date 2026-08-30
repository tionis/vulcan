"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const http = require("node:http");
const { VulcanCompanionClient } = require("../protocol");

const TOKEN = "b".repeat(43);

test("client completes status, sync, conflict review, preview, and apply against a mock daemon", async (t) => {
  const requests = [];
  const server = http.createServer(async (request, response) => {
    const body = await readBody(request);
    requests.push({ method: request.method, url: request.url, headers: request.headers, body });
    response.setHeader("content-type", "application/json");
    response.setHeader("vulcan-protocol-version", "1");
    if (request.headers.authorization !== `Bearer ${TOKEN}`) {
      response.statusCode = 401;
      response.end(JSON.stringify({ version: 1, kind: "permission_denied", detail: "bad token" }));
      return;
    }
    if (request.url === "/wiki/sync/status") {
      response.end(JSON.stringify({ version: 1, wiki_id: "wiki", state: "conflicted", unresolved_conflicts: 1 }));
    } else if (request.url === "/wiki/sync" && request.method === "POST") {
      response.statusCode = 202;
      response.end(JSON.stringify({ replayed: false, job: { id: "job-1" } }));
    } else if (request.url === "/wiki/sync/conflicts") {
      response.end(JSON.stringify({ count: 1, conflicts: [{ id: "conflict-1", paths: ["Note.md"] }] }));
    } else if (request.url === "/wiki/sync/conflicts/conflict-1") {
      response.end(JSON.stringify({ record: { id: "conflict-1", paths: [{ path: "Note.md" }] }, resolution: "unresolved" }));
    } else if (request.url === "/wiki/sync/conflicts/conflict-1/resolve") {
      const input = JSON.parse(body);
      response.end(JSON.stringify({ conflict_id: "conflict-1", side: input.side, dry_run: input.dry_run }));
    } else {
      response.statusCode = 404;
      response.end(JSON.stringify({ version: 1, kind: "not_found", detail: "missing" }));
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => server.close());
  const address = server.address();
  const transport = async (input) => {
    const response = await fetch(input.url, {
      method: input.method,
      headers: input.headers,
      body: input.body,
    });
    const text = await response.text();
    return { status: response.status, text };
  };
  const client = new VulcanCompanionClient({
    baseUrl: `http://127.0.0.1:${address.port}`,
    token: TOKEN,
    request: transport,
    idempotencyKey: () => "obsidian-mock-job",
  });

  assert.equal((await client.status("wiki")).state, "conflicted");
  assert.equal((await client.sync("wiki")).job.id, "job-1");
  assert.equal((await client.conflicts("wiki")).count, 1);
  assert.equal((await client.conflict("wiki", "conflict-1")).record.id, "conflict-1");
  assert.equal((await client.resolveConflict("wiki", "conflict-1", "local", true)).dry_run, true);
  assert.equal((await client.resolveConflict("wiki", "conflict-1", "local", false)).dry_run, false);

  assert.equal(requests[0].headers["vulcan-protocol-version"], "1");
  assert.equal(requests[1].headers["idempotency-key"], "obsidian-mock-job");
  assert.deepEqual(
    requests.slice(-2).map((request) => JSON.parse(request.body)),
    [
      { side: "local", dry_run: true },
      { side: "local", dry_run: false },
    ],
  );
});

function readBody(request) {
  return new Promise((resolve, reject) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => resolve(body));
    request.on("error", reject);
  });
}
