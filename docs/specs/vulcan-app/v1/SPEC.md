# Vulcan App Platform Specification v1

Status: normative implementation target for Roadmap Phase 19. It does not claim that the current binary implements this specification.

This document freezes the version 1 package, lifecycle, capability, bridge, runtime, storage, and conformance contracts. Requirements use MUST, MUST NOT, SHOULD, and MAY in the RFC 2119 sense. Anything not granted or described here is unavailable to an app. Future behavior requires a separately versioned contract rather than permissive interpretation.

Machine-readable companions:

- `manifest.schema.json` — closed JSON Schema for `manifest.json`
- `vulcan-app.wit` — server-component guest/host boundary
- `examples/minimal/` — canonical static package source fixture and identity vector
- `EXAMPLE_APPS.md` — implementable product contracts for the first-party examples

## 1. Scope and non-goals

A Vulcan App is one immutable package that may declare static browser views, typed CLI commands, QuickJS functions, server WebAssembly components, jobs, and event handlers. Every surface calls the same transport-neutral App API under the same effective authority.

Version 1 does not provide:

- arbitrary host filesystem, environment, process, socket, database, or daemon access;
- native package executables or package-supplied SQLite extensions;
- executable dependencies on another `.vapp`;
- raw TTY file descriptors;
- ambient WASI, HTTP, clocks, randomness, or filesystem imports for server components;
- automatic multi-master SQLite replication; or
- execution directly from an unvalidated archive or synchronized package path.

Browser WASM is an ordinary browser asset and reaches Vulcan only through the JavaScript bridge. Server WASM uses the component contract in this directory and is independently feature-gated.

## 2. Required implementation boundaries

The implementation MUST preserve these ownership boundaries:

- `vulcan-core`: manifest/domain types, strict schema validation, identities, capability/store descriptors, App API request/report types, and package-path validation;
- `vulcan-app`: finite validation, install, update, instance, binding, migration, invocation, and uninstall workflows;
- `vulcan-daemon`: HTTP/iframe hosting, browser sessions, retained jobs, schedules, events, live sessions, and async supervision;
- `vulcan-cli`: argument parsing and human/JSON rendering over the shared services; and
- replaceable adapters: ZIP codec, QuickJS integration, Wasmtime integration, private-store implementation, and future replicated stores.

No runtime or transport may construct an executable package except from the single complete validation boundary.

## 3. Package representation

### 3.1 Logical layout

`manifest.json` MUST be the first logical member. Other paths have no reserved directory semantics except `META-INF/signatures/`; the manifest is authoritative. Recommended layout:

```text
manifest.json
assets/index.html
assets/app.js
backend/main.js
components/compute.wasm
schemas/input.json
schemas/output.json
META-INF/signatures/<key-id>.json
```

Every non-signature payload MUST occur exactly once in `files`. Every `files` member MUST occur exactly once in the archive. `manifest.json` and signature records MUST NOT occur in `files`. Unlisted entries are invalid.

### 3.2 Fixed validation limits

These are hard v1 format ceilings. Administrators MAY configure lower limits but MUST NOT accept larger v1 packages.

| Limit | Value |
| --- | ---: |
| exact ZIP bytes | 268,435,456 (256 MiB) |
| manifest bytes | 1,048,576 (1 MiB) |
| entries including manifest/signatures | 4,096 |
| compressed bytes per entry | 134,217,728 (128 MiB) |
| actual uncompressed bytes per entry | 134,217,728 (128 MiB) |
| total actual uncompressed payload | 536,870,912 (512 MiB) |
| decompression ratio per entry and total | 100:1 |
| canonical path bytes | 240 |
| path components | 16 |
| signature records | 32 |

Checked arithmetic and bounded streaming readers MUST enforce actual produced bytes. Declared ZIP sizes are never sufficient.

### 3.3 ZIP profile

Accept only single-disk ZIP with Stored (`0`) and Deflate (`8`) regular-file entries. Reject ZIP64, encryption, data descriptors with inconsistent headers, directories, links, special files, duplicate or ASCII-case-fold-colliding names, overlapping ranges, prepended/trailing bytes, archive/entry comments, extra fields, unsupported filename encodings, and inconsistent local/central headers. Byte-range-delivered entries MUST be Stored.

Package paths use ASCII and match:

```text
[A-Za-z0-9._@+-]+(/[A-Za-z0-9._@+-]+)*
```

Reject leading/trailing slash, empty/`.`/`..` components, backslash, NUL/control bytes, drive/device prefixes, and any path that is not already canonical. Package validation and execution MUST NOT extract entries.

### 3.4 Canonical manifest and identities

The manifest MUST:

- be UTF-8 without BOM;
- be I-JSON compatible;
- contain no duplicate keys, floats, or explicit `null` values;
- exactly equal its RFC 8785 canonical serialization; and
- validate against `manifest.schema.json` with no unknown fields.

Object maps rely on RFC 8785 member ordering. Semantically set-like arrays—capability-request IDs, selector values, examples, and required feature names—MUST be duplicate-free and sorted by Unicode code point. Validators reject noncanonical collection order rather than silently sorting it.

Identity algorithms are closed for v1:

```text
payload digest = BLAKE3 derive-key("dev.vulcan.app-payload.v1", uncompressed bytes)
AppContentId   = BLAKE3 derive-key("dev.vulcan.app-content.v1", exact canonical manifest bytes)
PackageBlobId  = BLAKE3 derive-key("dev.vulcan.app-package-blob.v1", exact ZIP bytes)
```

Serialize them as lowercase `blake3:` followed by 64 hexadecimal characters. Repacking may change `PackageBlobId` while preserving `AppContentId`.

The validation order is fixed: transport limit, central directory, ZIP structure/profile, names/types/ranges, bounded manifest read, duplicate-key parse, canonical comparison, schema/semantic validation, archive/manifest bijection, streamed payload size/digest verification, identities, and detached signatures. Only success yields `ValidatedAppPackage`.

## 4. Manifest contract

The schema is normative. The top-level fields are:

- `format_version`: integer `1`;
- `app`: stable reverse-DNS ID, semantic version, display metadata, SPDX license expression, optional homepage;
- `api_versions`: exact major strings for App API, browser bridge, CLI descriptor, QuickJS host, and server-WASM ABI;
- `entrypoints`: maps of views, functions, commands, jobs, and event handlers;
- `capability_requests`: sorted requests with stable IDs and maximum selectors;
- `stores`: map of package-local logical store IDs;
- `resources`: maximum runtime budgets requested by the package; and
- `files`: complete payload inventory.

IDs match `[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*`, are at most 96 bytes, and are compared byte-for-byte. App IDs MUST contain at least two dot-separated components. Entrypoint, capability-request, and store IDs are stable API identifiers and MUST NOT be reused for incompatible semantics within the same app major version.

At least one view, function, command, or job is required. Commands/jobs/event handlers reference a declared function. A function declares either QuickJS or server-WASM execution:

- QuickJS: `runtime: "quickjs"`, package `path`, and named ESM `export`;
- server WASM: `runtime: "wasm"`, component `path`, and logical exported `entry` passed to the component's `invoke` function.

Input/output schema paths MUST name files with media type `application/schema+json`. Validators compile every referenced schema at package validation time using Vulcan's bounded JSON Schema 2020-12 profile. Remote `$ref` is invalid.

Views name an HTML file and declare `full_page`, `embed`, or `both`; their CSP and effective features are derived from grants, never supplied as raw header text. `static_export` only declares compatibility and does not grant publication.

## 5. Lifecycle and durable identities

Package, installation, and instance state machines are separate:

```text
package candidate: discovered -> validated | rejected
installation:      absent -> installed-disabled -> enabled -> disabled -> removed
instance:          absent -> configured-disabled -> migration-pending -> enabled
                                      |                    |             |
                                      +-> invalid/blocked <-+-------------+
                                      -> archived -> removed
```

Rules:

1. Discovery is passive and performs no trust, migration, or execution.
2. Installation copies the exact validated blob to a no-clobber device-local content store keyed by `PackageBlobId` and records a validation receipt.
3. Trust is bound to exact `AppContentId`, publisher evidence, installation, and grant lineage—not display version.
4. Instances have stable ULIDs independent of display name and bind package requests/stores to concrete narrower resources.
5. Updates are candidates until validation, capability delta, entrypoint removal, publisher continuity, compatibility, and migration plans are reviewed.
6. Code activation and data migration are separate transactions. Failed migration leaves the old code active when compatible or blocks the instance without destroying data.
7. Disable stops new sessions/jobs, revokes bridge channels, requests cooperative cancellation, and retains data.
8. Uninstalling code never deletes authoritative data. Cache deletion, instance removal, data archive, and explicit data deletion are separate planned operations.
9. Rollback selects a retained compatible package; it never silently reverses a data schema.

All lifecycle mutations use preview/apply requests carrying exact package, installation, instance, grant, configuration, and store revisions. Repeated apply with the same operation ID is idempotent; changed inputs return `stale_state`.

## 6. Capability model

Effective authority is:

```text
caller/session ∩ installation ∩ instance ∩ manifest request
∩ runtime ceiling ∩ canonical policy ceilings
```

Nested function, component, command, tool, and job calls preserve or attenuate that set. A manifest request grants nothing. Required requests prevent enablement when denied; optional requests produce a declared reduced mode.

The v1 registry is closed to these names:

| Capability | Selector fields | Operations |
| --- | --- | --- |
| `vault.notes.read` | `paths`, `tags`, `types` | list/get/render visible notes |
| `vault.notes.write` | `paths`, `tags`, `types` | plan/apply create or content/property mutation |
| `vault.notes.delete` | `paths`, `tags`, `types` | separately plan/apply deletion |
| `vault.query` | `paths`, `tags`, `types` | canonical query AST and mdbase query |
| `vault.search` | `paths`, `tags`, `types` | filtered lexical/vector search |
| `vault.graph` | `paths`, `tags`, `types` | filtered links/backlinks/neighborhoods |
| `vault.tasks.read` / `vault.tasks.write` | `paths`, `tags`, `types` | task queries or planned mutations |
| `vault.artifacts.read` / `write` / `delete` | `paths`, `media_types` | bounded artifact access or planned mutation |
| `app.stores.read` / `write` / `admin` | `stores` | bound store operations; admin covers reset/export/delete |
| `app.sessions.join` / `manage` | `stores` | live-session participation or privileged transitions |
| `jobs.enqueue` / `schedule` | `functions` | immediate or retained/scheduled invocation |
| `events.subscribe` | `events`, `paths`, `media_types` | filtered post-consistency events |
| `network.fetch` | `domains` | host-mediated HTTPS/approved loopback requests |
| `secrets.use` | `secrets`, `domains` | use opaque handles only with named operations/origins |
| `tools.call` | `tools` | invoke visible typed tools |
| `publication.write` | `paths` | plan/apply an app publication |
| `host.execute` | `executables` | optional named host adapter; never a shell |

Selector arrays are sorted, duplicate-free, positive allowlists. Paths are vault-relative forward-slash globs interpreted by the existing permission layer. Domains are lowercase IDNA ASCII hostnames with optional exact port; wildcards match subdomains only and never broaden to sibling registrable domains. Empty/missing selectors mean no resources, not all resources. A request that needs all resources uses the explicit sentinel string `"*"`, which still remains subject to grants and policy ceilings.

Reads, counts, errors, timing-sensitive suggestions, search, graph, events, and autocomplete are filtered before app code receives them. Mutation authorization checks both old and resulting selectors.

## 7. App API v1

Every adapter exposes these logical method names. Methods unavailable under the compiled feature set or grant return `unsupported_feature` or `permission_denied`; they do not disappear silently from capability negotiation.

| Namespace | Methods |
| --- | --- |
| `app` | `context`, `features`, `instance.get` |
| `notes` | `list`, `get`, `render` |
| `query` | `run`, `search`, `graph` |
| `tasks` | `list`, `get` |
| `mutations` | `plan`, `apply`, `status` |
| `stores` | `get`, `kv.get`, `kv.scan`, `kv.transact`, `sql.query`, `sql.transact`, `mdbase.read`, `mdbase.query`, `mdbase.plan`, `blob.put`, `blob.get`, `blob.list`, `admin.plan`, `admin.apply` |
| `sessions` | `join`, `snapshot`, `command`, `leave` |
| `jobs` | `enqueue`, `get`, `cancel`, `schedule.plan`, `schedule.apply` |
| `events` | `subscribe`, `unsubscribe` |
| `network` | `fetch` |
| `secrets` | `invoke` |
| `tools` | `list`, `call` |
| `publication` | `plan`, `apply` |

All calls use typed domain request/report objects. Before any App API runtime is enabled, the implementation MUST check in versioned request, success, and error JSON fixtures for every method in this closed registry. Rust domain types plus those fixtures are the source, and one build step MUST generate or conformance-check browser TypeScript declarations, OpenAPI projections, and WIT-facing JSON schemas from them. Hand-maintained divergent wire structs are invalid. Adding or incompatibly changing a method requires a new App API major; adding an optional field or method fixture requires a reviewed compatible protocol revision.

Common request metadata contains `request_id`, optional `trace_id`, expected protocol major, and optional deadline. Mutation apply additionally requires plan ID, exact accepted input revisions, and idempotency key. List operations use opaque cursors and explicit limits; v1 default/max page sizes are 50/500. Structured logs and errors are capped at 64 KiB per invocation after redaction.

Stable error codes are: `invalid_request`, `unsupported_version`, `unsupported_feature`, `not_found`, `ambiguous`, `permission_denied`, `capability_revoked`, `stale_state`, `conflict`, `limit_exceeded`, `cancelled`, `deadline_exceeded`, `unavailable`, `invalid_data`, `migration_required`, `instance_blocked`, and `internal`. `internal` exposes a correlation ID, never host paths, secrets, SQL, headers, or panic text.

## 8. Browser host and bridge v1

Each instance is served through an isolated app origin. If deployment cannot provide subdomains, the daemon MUST use an equivalent origin-isolating host; path-only isolation on the WebUI origin is invalid. The iframe omits `allow-same-origin` unless a future reviewed profile requires it and receives only explicitly derived sandbox tokens. Default CSP denies network, navigation, popups, downloads, forms, workers, media capture, and framing outside the Vulcan host. Granted `network.fetch` still uses the bridge; it does not add arbitrary origins to `connect-src`.

Handshake:

1. The iframe loads immutable assets keyed by `AppContentId`.
2. It sends `{type:"vulcan.ready", bridge_version:"1", nonce:<128-bit base64url>}` to its parent.
3. The host verifies window, origin, instance, active session, nonce shape, and compatible version.
4. The host transfers exactly one `MessagePort` with `{type:"vulcan.init", bridge_version:"1", session_id, instance_id, view_id, features}`.
5. All later traffic uses that port; window-level messages are ignored.

Requests are `{type:"request", id, method, params}`, responses are `{type:"response", id, ok:true, result}` or `{type:"response", id, ok:false, error}`, and notifications are `{type:"event", subscription_id, sequence, event, data}`. IDs are unique within the session. Unknown fields, duplicate IDs, out-of-order event sequences, oversized messages, invalid schemas, wrong source ports, or calls after revocation close the channel. Default/max request size is 1/8 MiB; default/max in-flight calls is 16/64. Navigation or reload creates a new session and invalidates handles/subscriptions.

Note embeds receive the same isolated view with an embed context and tighter size/input/navigation limits. They never inherit editor or facilitator authority from the containing page.

## 9. Runtime contracts

### 9.1 QuickJS

QuickJS remains behind `js_runtime`. Modules are ESM loaded only from the validated package VFS. Imports are relative package paths or the exact virtual module `@vulcan/app`; host paths, dynamic network imports, Node/Bun APIs, CommonJS resolution, environment variables, and native modules are absent.

Each declared function export receives `(input, context)` and returns a JSON-compatible value or Promise. `context` contains non-secret invocation IDs, deadline, cancellation observation, and an App API client restricted to the effective grant. The host drives promises only while the invocation is alive. Functions cannot retain handles across invocations unless the API explicitly returns a durable opaque ID.

Defaults/maxima: 64/256 MiB heap, 5/30 seconds for interactive/CLI calls, 60/300 seconds for jobs, 1/8 MiB input, and 8/32 MiB output. The manifest may request values up to maxima; installation policy may lower them. Interrupt checks, cancellation, stack limits, and bounded host calls are mandatory.

### 9.2 Server WebAssembly

Version 1 selects Wasmtime `36.0.10`, whose upstream MSRV 1.86 fits Vulcan's MSRV 1.88, with default features disabled and only the reviewed component-model/runtime/compiler features needed by the adapter. The dependency is optional behind `wasm_runtime`; exact enabled features and binary-size impact are recorded when added. Upgrades require explicit compatibility/security review.

Only WebAssembly Components implementing `vulcan-app.wit` are accepted. The world has one generic JSON request/response invocation and one generic App API host call so the Rust domain schemas remain authoritative. Inputs and outputs are UTF-8 RFC 8785 JSON bytes. Components receive no WASI imports. Clocks, random bytes, network, files, stores, secrets, and tools are reachable only through explicit App API methods and effective capabilities.

Enable fuel consumption, epoch interruption, pooling/allocation limits where supported, maximum 128/512 MiB linear memory default/max, 10,000 table elements, 1,000 instances/tables/memories combined, 5/30 second interactive calls, and 60/300 second jobs. Compiled component caches are derived and keyed by component digest, Wasmtime version, target, engine configuration, and ABI version.

Traps map to typed bounded runtime errors. A trap, timeout, or cancellation drops the store and invalidates all invocation handles. Components never run during package validation, discovery, sync, conflict inspection, or migration planning.

## 10. Store profiles

Store fields are orthogonal, but these v1 profiles are the only accepted combinations:

| Profile | Engine | Authority | Scope | Replication | Visibility | Retention |
| --- | --- | --- | --- | --- | --- | --- |
| `temporary` | `kv` or `blob` | `temporary` | `invocation` | `none` | `private` | `disposable` |
| `derived-cache` | `kv`, `sqlite`, or `blob` | `derived` | `device` | `none` | `admin` | `disposable` |
| `device-local` | `kv` or `sqlite` | `authoritative` | `device` | `none` | `admin` | `preserve` |
| `vault-collection` | `mdbase` | `authoritative` | `vault` | `file_tree` | `vault` | `preserve` |
| `canonical-artifact` | `canonical_artifact` or `blob` | `authoritative` | `vault` | `file_tree` | `vault` | `preserve` |
| `live-session` | `live_session` | `temporary` | `group` | `external` | `admin` | `disposable` |

`replicated` engine/profile is reserved and invalid in stable v1 manifests until the post-v1 conformance gate assigns a new profile/API minor version.

Private allocations are host-owned and may use `.vulcan/apps/<instance-id>/cache/` or `state/` on ordinary direct-mode vaults. Paths are never API values and are excluded from vault scan, publication, Git/file-tree sync, and app-controlled symlink traversal. Temporary storage uses the OS temporary facility. Secrets use the secret service, never a store.

KV values are canonical JSON, at most 1 MiB each, addressed by UTF-8 keys matching `[A-Za-z0-9._@+/-]{1,240}` without empty/`.`/`..` components. Transactions contain at most 1,000 operations and 8 MiB combined input. SQL accepts parameterized statements only, allows one host-parsed statement per operation, disables extension loading and arbitrary attach/VFS paths, and enforces read-only versus write transactions. Default/max private database quota is 256 MiB/4 GiB. Migrations are packaged, versioned, checksum-verified, previewed, journaled, backed up according to retention, and post-validated.

An mdbase binding names collection root plus optional types/contracts/path selectors and required conformance profiles. Version 1 app writes require Vulcan's `core_write` support; unsupported profiles block only dependent entrypoints. Apps never mutate mdbase files outside the shared planner.

Canonical SQLite is an artifact, not a private SQL handle by default. A declared canonical-artifact adapter may expose SQL through the same guarded host connection, but every committed transaction produces an atomic captured artifact revision. Concurrent file-tree versions require review in v1.

## 11. Events, jobs, network, and secrets

Events are delivered only after the originating mutation and incremental scan reach consistent readable state. File events contain event ID, kind, canonical visible path, media type, old/new BLAKE3 where authorized, mutation/transaction ID, and origin instance ID; never file bytes. Filters are compiled from the capability selector. Delivery is at-least-once, so handlers MUST use event IDs/idempotency keys. The host coalesces editor save bursts and suppresses self-trigger loops by origin plus declared policy, without dropping genuine later user changes.

Interactive hooks never perform long work. An event handler may validate and enqueue a retained job. Jobs have `queued`, `running`, `succeeded`, `failed`, `cancelled`, or `blocked` states; persist function ID, redacted canonical input, effective grant identity, package/instance/schema revisions, attempts, progress, result reference, and cancellation state. Restart requeues only explicitly retryable idempotent jobs. Schedules require separate approval and daemon availability; direct mode reports `unsupported_feature` rather than starting a daemon.

`network.fetch` accepts HTTPS and explicitly approved loopback HTTP only. It reauthorizes every redirect, strips credentials on origin change, applies domain/port selectors, blocks private/link-local/metadata destinations unless the exact loopback capability permits them, bounds DNS rebinding, headers, redirects, body bytes, decompression, and time, and never forwards an opaque secret except through `secrets.invoke` for a matching operation and origin.

## 12. CLI surface

Host commands are fixed:

```text
vulcan apps discover|inspect|validate|list|show|install|update|disable|uninstall|doctor
vulcan apps instances list|show|create|set|enable|disable|remove
vulcan apps grants show|plan|apply|revoke
vulcan apps stores list|show|export|reset|archive|delete
vulcan apps commands list|show
vulcan apps run <instance> <command> [declared arguments]
vulcan apps pack|lint|test|unpack
```

Mutations support `--dry-run` or plan/apply. Every command has stable `--output json`; streams use JSON Lines. App commands are manifest-declared, namespaced, parsed by Vulcan into structured input, and invoke a declared function. They never receive raw argv/environment/stdout/stderr. Descriptor rules, including flags, positionals, stdin modes, defaults, bounds, exclusions, and schemas, are closed by `manifest.schema.json`. Full-screen terminal apps remain outside v1.

## 13. Conformance and implementation order

Required fixture families:

1. canonical minimal package plus known payload/content/blob/signature identities;
2. every ZIP/path/JSON/schema/bijection/hash/signature rejection;
3. lifecycle interruption, stale plans, rollback, migration, disable/uninstall retention;
4. capability intersection, selector filtering, revocation, nested calls, and side-channel-safe derived results;
5. bridge origin/source/session/message/limit/revocation attacks;
6. QuickJS and WASM denied imports, timeouts, cancellation, memory/output limits, and malformed results;
7. store classification, cache reset, private-path exclusion, migration, quota, backup, canonical conflicts, live expiry, and blob GC;
8. event/job duplication, restart, self-trigger prevention, schedule denial, and permission changes; and
9. direct/daemon/browser/CLI conformance against the same domain snapshots.

Implementation MUST follow the roadmap gates. Gate A is complete only when this specification, schema, vectors, fixtures, parser, and validator agree. Gate B cannot execute code. Gate C enables only the static browser subset. Each later runtime/storage/network feature remains unavailable until its own conformance family passes. The replicated-store investigation is not part of stable v1 completion.
