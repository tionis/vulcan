# Event relay reference server and Vulcan client plan

**Status:** Planning baseline  
**Specifications:** [Event Relay Protocol](event-relay-protocol.md), [Git Realtime Events](git-realtime-events.md)

This document separates two products:

- a generic reference event relay that should become its own project once the protocol fixtures stabilize;
- the Vulcan client, which consumes Git events without making the generic relay depend on Vulcan.

## 1. Reference server

### 1.1 Project boundary

The server should be developed as a standalone project with an independent name, release cycle, container image, and conformance suite. This repository remains the initial specification home and may temporarily contain test fixtures or a prototype, but Vulcan must consume only the published protocol.

The server is a generic CloudEvents relay. Git support is an ingress/profile package, not a core assumption.

### 1.2 Recommended architecture

```text
management API ──────────────── channel/credential store
       |                                  |
webhook ingress → authenticated adapters  |
       |                                  |
       └──── normalized CloudEvents ─→ NATS + JetStream
                                               |
                                      authenticated clients
```

Initial components:

- an HTTPS management and descriptor API;
- opaque channel, publisher, and subscriber records;
- hashed bearer-capability verification and independent revocation;
- a generic authenticated CloudEvents HTTP ingress;
- a Forgejo webhook adapter;
- NATS structured CloudEvents publication;
- JetStream retention for bounded logs and latest-by-subject state;
- a NATS authentication callout or equivalently scoped generated credentials;
- a CLI for operator setup, channel creation, webhook provisioning, subscription export, rotation, revocation, and diagnostics;
- bounded audit records containing credential IDs but no tokens or event bodies.

Rust with `axum`, `tokio`, and `async-nats` is the natural initial implementation because it matches the specification prototype and Vulcan ecosystem, but the conformance suite—not shared Rust internals—defines compatibility.

### 1.3 Storage

The first single-node release can use SQLite for channel and credential metadata while JetStream owns retained event delivery. The database stores token hashes/verifiers, never raw exported tokens. Schema design must permit later PostgreSQL or another transactional store without changing descriptors.

Events need not be copied into the management database. Administrative state and broker retention have separate backup and repair procedures.

### 1.4 Initial authentication decision

The capability-authenticated MVP uses four disjoint authorities:

- a server operator identity bootstrapped through local CLI/configuration and required for server-wide administration;
- a channel-manager capability that can create, rotate, and revoke credentials for one channel but cannot publish or subscribe;
- a source-specific webhook secret or publish-only ingress capability;
- one read-only capability per subscriber by default.

The reference server may encode an opaque token as `er1.<credential-id>.<secret>`, where the public credential ID provides bounded lookup and the secret contains 256 random bits. The format is an implementation detail and clients treat the entire token as opaque. The server stores the credential ID and an HMAC-SHA-256 verifier under a separately protected server pepper, compares verifiers in constant time, and returns the raw token only once. Database and pepper backups are both required to preserve issued credentials.

For NATS, an authentication callout validates the capability and returns short-lived exact-subject permissions. Credential expiry bounds an already connected client's authority; explicit revocation should also disconnect matching active credential IDs where the broker administration API permits it. A revoked capability can never obtain a new broker session. The implementation must test both reconnect revocation and the documented upper bound for an existing connection.

This model avoids requiring user accounts or forge integration for the first release while preserving clean migration to OIDC, OAuth token exchange, mTLS, or signed public-key identities. Stronger schemes issue the same channel-scoped broker permissions and do not change subscription descriptors or Git events.

### 1.5 Initial HTTP surface

The exact resource names remain implementation details, but the first server needs operations equivalent to:

- create, inspect, and delete a channel;
- create and revoke a subscriber capability;
- create and rotate a publisher or webhook capability;
- return a public source descriptor;
- export a subscription bundle exactly once at credential creation;
- accept generic CloudEvents from an authenticated publisher;
- accept and validate Forgejo webhooks;
- report broker, adapter, and retention health.

Mutating operations require an administrative identity and idempotency key. Secret-bearing responses must set `Cache-Control: no-store` and must never be returned by later read operations.

### 1.6 Delivery and authentication milestones

1. **Protocol types and fixtures:** JSON Schema or equivalent validation for descriptors, bundles, common retention declarations, and Git events.
2. **Local conformance broker:** one-node NATS/JetStream deployment, generic CloudEvent ingress, public channels, and replay tests.
3. **Capability security:** per-subscriber 256-bit tokens, hash-only storage, exact-subject subscription permissions, rotation/revocation, TLS, redaction, and rate limits.
4. **Forgejo adapter:** HMAC verification, repository mapping, deterministic webhook deduplication, `refs.updated`, and `ref.state` emission.
5. **Operator packaging:** container image, example Compose/systemd deployment, health endpoints, backup guidance, and versioned migrations.
6. **Additional adapters:** Gitea, GitHub, GitLab, and native `post-receive` publisher.
7. **Stronger authorization:** OIDC/OAuth or public-key identities exchanged for short-lived broker credentials; do not delay the capability-authenticated MVP for this.
8. **Additional bindings:** MQTT and WebSocket/HTTP delivery based on demonstrated client needs.

### 1.7 Reference server test gates

- Run the protocol conformance matrix from the generic spec.
- Replay captured, redacted webhook fixtures for every forge adapter.
- Prove subscriber credentials cannot publish, enumerate other channels, or subscribe across channel boundaries.
- Prove publisher credentials cannot subscribe or administer.
- Test duplicate, reordered, delayed, oversized, malformed, and unauthorized events.
- Test restart with retained latest state, token revocation during reconnect, broker outage, and database/broker partial failure.
- Test that descriptors, logs, metrics, traces, and audit records contain no credential material.

## 2. Vulcan client

### 2.1 Ownership boundaries

- A small dependency-light protocol module or workspace crate owns descriptor, subscription-bundle, CloudEvents, and Git-profile validation plus redacted secret wrappers. It performs no network I/O.
- `vulcan-daemon` owns asynchronous NATS connections, reconnect/backoff, subscription multiplexing, retained-event consumption, and runtime health.
- `vulcan-sync` remains transport-independent. It receives only the existing `SyncJobTrigger::RemoteNotification` through the supervisor.
- `vulcan-cli` owns import/list/remove/status/test presentation and edits device-local configuration through reusable services.
- `vulcan-app` is used only if subscription management becomes a reusable synchronous workflow; it must not own resident connections.

No event client belongs in `vulcan-core`, and no event path may call the Git engine directly.

### 2.2 Device-local state

Persist outside the vault and rebuildable cache:

- non-secret source descriptors;
- subscription IDs and credential-store references;
- explicit repository-source-to-wiki/ref bindings;
- bounded delivery cursors or `(source, id)` deduplication state where the binding needs them;
- last connection and validation diagnostics.

Tokens live in the platform credential store or a permission-restricted secret file. JSON output reports only credential IDs and redacted authentication schemes. Subscription material is never synchronized through Git.

### 2.3 Proposed CLI

All mutating commands support `--dry-run`; all commands support `--output json`.

```text
vulcan sync notifications import <wiki> --bundle <file|-> [--ref <full-ref>...]
vulcan sync notifications list [<wiki> | --all]
vulcan sync notifications show <subscription>
vulcan sync notifications remove <subscription>
vulcan sync notifications test <subscription>
vulcan sync notifications status [<wiki> | --all]
```

Import validates the complete bundle before storing anything, refuses an ambiguous repository binding, defaults to the registered wiki's configured live ref when no `--ref` is supplied, and produces a redacted plan. Reading a bundle from a command argument is not supported because it would expose credentials in process listings and shell history.

Direct CLI operation remains available without a daemon. Subscription management and validation work directly; realtime listening truthfully reports that it requires a running daemon. Manual and polling sync remain unaffected.

### 2.4 Runtime connection manager

The daemon groups compatible subscriptions by transport endpoint, TLS policy, and credential identity. It opens the minimum safe number of connections without sharing authority between credentials. Each connection uses bounded exponential reconnect with jitter and exposes connected, reconnecting, unauthorized, invalid-descriptor, and stopped states.

For each event, the runtime:

1. applies size and CloudEvents validation;
2. validates the Git profile;
3. resolves the explicit channel/source/ref binding;
4. rejects or quarantines malformed and mismatched input;
5. records bounded deduplication/cursor state when applicable;
6. enqueues `RemoteNotification` for every matching active wiki;
7. acknowledges only after routing has succeeded or a permanent invalid-input decision is recorded.

Supervisor coalescing is the work deduplication boundary. The event client never waits for the full Git sync before acknowledging a notification. A retained `ref.state` event and periodic polling repair notifications missed while the device was offline.

### 2.5 Permissions and network policy

Automatic subscriptions apply the registered wiki's permission profile before opening an endpoint or enqueueing Git work. Endpoint changes require revalidation. TLS validation is mandatory, redirects do not silently change broker authority, and descriptor/bundle imports use the same network and secret-handling rules as other Vulcan integrations.

One subscription may trigger only explicitly bound wikis and refs. Event payload repository names or URLs cannot select arbitrary local paths.

### 2.6 Android behavior

A persistent broker socket is an optional latency optimization, not the Android correctness mechanism. Termux may listen while its daemon is alive, but JobScheduler periodic sync remains the energy-aware repair path. A later native WorkManager or push gateway can translate a validated Git event into the same finite `sync run`/`RemoteNotification` path without another merge implementation.

### 2.7 Vulcan implementation milestones

1. Add strict protocol models, validation fixtures, and redacted secret wrappers.
2. Add device-local subscription and repository-binding storage with atomic import/remove and CLI JSON contracts.
3. Add a mock transport and runtime routing tests proving only matching source/ref bindings enqueue `RemoteNotification`.
4. Add the NATS client, connection multiplexing, JetStream acknowledgment, reconnect/backoff, and daemon lifecycle integration.
5. Add CLI status/test diagnostics and companion status projection without exposing credentials.
6. Run end-to-end tests against the reference server conformance fixture: webhook to CloudEvent to NATS to one coalesced Vulcan sync job.
7. Document desktop/server service behavior and Android fallback behavior.
8. Evaluate MQTT, WebSocket, native mobile wake, and Git protocol v2 discovery only after the NATS path is operational.

### 2.8 Vulcan acceptance criteria

- A Forgejo push wakes every bound active wiki and no unbound wiki.
- The resulting job uses `RemoteNotification` and the ordinary finite sync/conflict pipeline.
- Duplicate events and bursts produce bounded supervisor work.
- Invalid, unauthorized, stale, and mismatched events never invoke Git.
- Relay or credential failure degrades to visible offline notification status while periodic polling continues.
- Restart consumes retained state without losing local changes or replaying unbounded work.
- Logs, CLI JSON, companion payloads, crash reports, and config files do not expose tokens.
- Ordinary local commands and one-shot sync remain independent of the event client and daemon.

## 3. Extraction decision

Start the generic schema fixtures and the smallest reference-server spike where iteration is convenient, but create the standalone project before publishing a server release or accepting non-Vulcan consumers. At extraction:

- move generic protocol schemas, fixtures, and server code;
- leave links and the Vulcan-specific client plan here;
- consume versioned released fixtures or a protocol-types crate rather than a Git submodule of server internals;
- preserve protocol conformance across the split in CI.
