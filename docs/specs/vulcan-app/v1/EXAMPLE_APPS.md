# Vulcan App v1 Reference Application Contracts

Status: normative product and conformance target for the Phase 19 reference apps. These contracts define the minimum coherent product; additional features require separate backlog items and cannot delay the stated MVP.

Every reference app MUST be an ordinary package using only public v1 contracts. Each package project includes its source manifest, deterministic lockfile/build, sample vault/store data, expected capability snapshot, CLI JSON snapshots, browser accessibility tests, direct/daemon parity tests where applicable, and hostile fixtures. No example receives private endpoints, implicit grants, raw paths, relaxed validation, or test-only authority.

## 1. Shared acceptance contract

For every app:

1. Installation, instance creation, grant review, migration, update, disable, uninstall, and data-retention behavior work through the standard host workflows.
2. Every view renders a useful denied/unsupported/offline state and does not infer hidden resources.
3. Every mutation uses a typed plan/apply operation with exact revisions and idempotency.
4. CLI commands support `--output json`, non-interactive use, stable errors, and direct mode unless the operation inherently requires a daemon.
5. Browser, CLI, QuickJS, and WASM surfaces share domain functions rather than implementing divergent business rules.
6. Package, runtime, cache, local state, canonical data, secrets, jobs, and live sessions survive or disappear according to their declared profile.
7. Static publication emits only explicitly supported public views/data and never bundles secrets, private stores, grants, live control channels, or unpublished content.

## 2. Presenter (`dev.vulcan.presenter`)

### 2.1 Product boundary

The MVP presents one canonical Markdown note as a reveal.js deck, supplies audience and presenter views, reloads after consistent note changes, and exports a static deck. It does not provide a second slide editor, collaborative slide CRDT, cloud presentation service, or arbitrary reveal.js plugin execution.

Pin reveal.js and its license in the package lock/provenance. Only bundled reviewed reveal.js modules are enabled. Vulcan renders Markdown and resolves embeds before the deck receives HTML.

### 2.2 Source contract

Deck notes opt in with:

```yaml
---
type: presentation
vulcan_presenter:
  split: h1
  vertical: h2
  theme: black
  transition: slide
---
```

Supported `split` is `h1` or `comment`; supported `vertical` is `none`, `h2`, or `comment`. Comment separators are standalone `<!-- vulcan:slide -->` and `<!-- vulcan:vslide -->`. A heading starts the corresponding slide and remains its title. Content before the first boundary becomes a title slide when nonempty. Empty slides are invalid. Duplicate heading text is allowed because slide IDs derive from source byte range plus note revision, not title.

Speaker notes are contiguous blockquotes beginning `> [!speaker-note]`; they render only in the presenter view. A list item or paragraph followed immediately by `<!-- vulcan:fragment -->` becomes one reveal fragment. Unknown `vulcan_presenter` keys and unsupported raw reveal attributes produce diagnostics. Raw HTML follows the ordinary Vulcan rendering policy.

### 2.3 State, views, capabilities, and CLI

- canonical: source note and referenced permitted attachments;
- durable local: last selected deck, theme override, and presenter preferences;
- live session: deck revision, current horizontal/vertical slide, fragment, timer origin, and controller session;
- derived: rendered deck/dependency projection.

Views are `audience`, `presenter`, and `embed`. Presenter control requires `app.sessions.manage`; audience/embed requires join only. Read selectors bind to the selected deck and its permission-filtered dependencies. Static export additionally requests `publication.write`.

Commands:

```text
present <note> [--view audience|presenter] [--session <id>]
list-slides <note>
export <note> --output-path <path> [--theme <name>]
doctor <note>
```

`list-slides` returns stable index, title, source range, speaker-note presence, and diagnostics. Reload preserves the current source-backed slide when its boundary survives; otherwise it selects the nearest preceding surviving slide and reports the relocation.

Acceptance fixtures cover heading/comment splits, vertical slides, notes/fragments, wikilinks, transclusions, callouts, Mermaid, math, code, denied embeds, structural reload, controller revocation, print CSS, and deterministic static export.

## 3. Meeting Tool (`dev.vulcan.meeting`)

### 3.1 Product boundary and source

The MVP supports one facilitator, multiple viewers/participants, an ordered speaker queue, timers, decisions, and action items over an ordinary meeting note. Simultaneous free-form minute editing waits for Phase 16 and is not emulated with an app-specific CRDT.

Canonical meeting notes use:

```yaml
---
type: meeting
meeting_id: 01J00000000000000000000000
title: Planning
scheduled: 2026-09-07T10:00:00+02:00
status: planned
---

# Planning

## Agenda
- Budget review ^01J00000000000000000000001
- Release readiness ^01J00000000000000000000002

## Decisions

## Action items
```

`meeting_id` and every agenda item block ID are ULIDs and stable identities. Missing IDs are added only through an explicit preparation plan. Agenda order is file order. Decisions append `- <text> (agenda: [[#^ID]]) ^ULID`. Actions use ordinary Vulcan task syntax and include an `agenda` link plus assignee/due metadata where supplied. Finishing changes frontmatter status to `finished` and appends a bounded session summary; it never rewrites the agenda body wholesale.

### 3.2 State machine and roles

```text
planned -> active <-> paused -> finished
```

Only the facilitator may start/pause/resume/finish, change the current item, reorder/remove speakers, record decisions, or create actions. Participants may join, raise/lower their own hand, and yield their own active slot. Viewers are read-only. A facilitator token is session-bound, revocable, and cannot be inherited by an embedded audience view.

The live-session snapshot contains meeting/note revision, lifecycle state, current agenda ID, ordered queue entries `(participant-id, joined-sequence)`, timer origin/paused elapsed time, and monotonic sequence. Presence expires after 45 seconds without heartbeat; raised hands survive reconnect for 5 minutes; a live session expires 24 hours after finish. Optional checkpoints persist only lifecycle/current-item/queue/timer recovery data, never presence credentials.

### 3.3 Capabilities, CLI, and acceptance

Read requires the one meeting note. Preparation/decision/action/finish require scoped note/task writes. Joining/managing use separate live-session capabilities.

Commands are `start`, `show`, `next`, `previous`, `pause`, `resume`, `queue add|remove|list`, `yield`, `decision`, `action`, and `finish`. Every transition accepts expected session sequence and note revision; stale calls return the current snapshot without applying.

Acceptance tests cover missing/duplicate IDs, facilitator revocation, simultaneous queue joins ordered by accepted server sequence, reconnect/expiry, stale agenda edits, deleted current items, timer pause/resume, action validation failure, finish retry idempotency, denied viewers, and daemon restart from a checkpoint.

## 4. Ember Run and Wiki Quest (`dev.vulcan.ember-run`)

### 4.1 Capability-free game

The base game is a deterministic, single-screen maze named Ember Run. The player uses arrow/WASD keys or four touch controls to collect eight runes and reach an exit while avoiding moving hazards. A level seed, input sequence, and fixed 60 Hz logical tick reproduce the same result. Rendering may use Canvas 2D or bundled browser WASM but cannot depend on wall-clock timing for game rules.

It requests no vault, network, secret, job, event, tool, mutation, or host-execution capability. Settings and high scores use one bounded `device-local` KV store only after the user enables persistence; otherwise state is in memory. Audio defaults off, reduced-motion is honored, controls are remappable, all status has text alternatives, and gameplay remains possible without audio.

This app is the denial baseline: forged bridge calls, denied capabilities, cross-instance handles, oversized saves, and revoked persistence must fail while a new in-memory game remains playable.

### 4.2 Optional Wiki Quest mode

Wiki Quest is a separate optional view/request. It reads a permission-filtered graph selector and creates a deterministic puzzle from only visible node IDs, titles, and permitted edges. Restricted nodes contribute no placeholder, count, degree, timing distinction, or missing-target hint. Seed identity includes the visible graph digest so two users are never told their differing maps are equivalent.

Achievements remain local unless an explicit `vault.notes.write` binding targets a user-selected achievement note. No automatic canonical write is allowed.

Acceptance tests cover deterministic replay, keyboard/touch/reduced-motion operation, no-capability mode, quota denial, browser-WASM failure fallback, graph filtering, hidden-node non-inference, and opt-in achievement plans.

## 5. Blobforge Workbench (`dev.tionis.blobforge`)

### 5.1 Product and protocol boundary

The MVP is a WebUI and CLI client for one configured Blobforge coordinator: ingest a source, inspect queue/workers/jobs/artifacts, request conversion, download/preview results, hydrate supported outputs, and cancel when the coordinator advertises cancellation. It is not a second coordinator and does not execute the Blobforge schema.

Before client code lands, check in a pinned Blobforge revision, license/provenance record, and exact OpenAPI or captured typed protocol under the app project. Generate the transport client and fake coordinator from that snapshot. An upstream change is an explicit adapter upgrade; unknown fields are preserved only where the pinned contract permits them and unknown enum variants fail visibly.

Configuration contains non-secret endpoint, approved redirect/origin policy, recipe allowlist/default, output root binding, and optional PDF watch selector. The bearer token is an opaque instance secret scoped to the exact coordinator origin. Browser code never receives it. The default uses typed HTTPS requests; a local executable adapter separately requires `host.execute` bound to one administrator-configured executable ID, never a path or shell string.

### 5.2 Data and idempotency

- durable integration state: coordinator job/artifact IDs, source BLAKE3, exact recipe ID/version/digest, signed-transfer provenance, imported output bindings, and terminal status;
- derived cache: bounded status responses, thumbnails, and previews;
- canonical: explicitly imported MDAF/TextPack/Markdown/assets only;
- secrets: coordinator credentials;
- jobs: all ingestion/conversion/hydration work.

The idempotency key is BLAKE3 over the canonical tuple `(coordinator-id, source-digest, recipe-id, recipe-version, recipe-digest, output-kind)`. Repeated file events attach to the existing active/successful job. A changed source or recipe creates a new key. Downloaded results must validate container limits, source/recipe identity, and artifact digests before an import preview exists.

Commands are `ingest`, `dashboard`, `status`, `workers`, `artifacts`, `request-conversion`, `hydrate`, and `cancel`, with identifiers—not free-form coordinator paths—as arguments.

Acceptance uses only the fake coordinator and covers auth redaction, redirect reauthorization, signed URL expiry, retries/rate limits, cancellation races, duplicate events, restart, stale outputs, malformed artifacts, revoked network/secret grants, bounded status bodies, and mutation-free import previews.

## 6. Feed Reader (`dev.vulcan.feeds`)

### 6.1 Product boundary

The MVP supports RSS 2.0, Atom, JSON Feed, OPML import/export, manual/scheduled refresh, unread/star/archive state, search, sanitized reading, and explicit/rule-based capture to Markdown. It is not an email client, social reader, general browser, podcast player, crawler, or feed-sharing server.

### 6.2 Canonical subscription contract

When portable subscriptions are enabled, each is an mdbase-compatible record:

```yaml
---
type: feed-subscription
id: 01J00000000000000000000000
url: https://example.com/feed.xml
title: Example
enabled: true
tags: [news]
refresh_minutes: 60
capture_rule: manual
credential: private-feed
---
```

`credential` is an opaque configured handle name, never a token. Allowed `capture_rule` values are `manual`, `starred`, and `all`. Device-local subscriptions use the same validated object schema in private state. OPML excludes credential handles unless an explicit redacted Vulcan extension export is requested; it never exports values.

### 6.3 Identity and storage

Canonical feed identity is the normalized final self URL after an approved refresh, falling back to normalized configured URL. Entry identity prefers a nonempty feed-scoped Atom/RSS/JSON ID. Otherwise use the normalized absolute item URL; otherwise use BLAKE3 over feed identity plus normalized title, author, published timestamp, and sanitized textual content. Reused IDs with materially different content retain revisions and emit a diagnostic rather than replacing a captured note silently.

Private SQLite tables are `feeds`, `entries`, `entry_revisions`, `read_state`, and `refresh_state`, all keyed by stable IDs. Read/star/archive is durable device-local state. Raw bounded response bodies, parsed projections, search indexes, and localized media are derived cache. Enclosures use the blob store. Saved entries become ordinary Markdown with `type: feed-entry`, stable source/feed IDs, source URL, published/fetched timestamps, content digest, and capture provenance. Remote disappearance never deletes a saved note.

Refresh uses conditional requests, at most four concurrent origins and one request per origin at a time, bounded exponential retry, ten redirects, 16 MiB compressed and 64 MiB decompressed bodies, and the platform network/SSRF rules. XML DTD/entity expansion is disabled. HTML is sanitized through Vulcan's renderer; scripts, styles, forms, active embeds, event handlers, and unsafe URLs are removed.

Views are `subscriptions`, `inbox`, `entry`, and `settings`. Commands are `subscriptions`, `add`, `remove`, `refresh`, `entries`, `read`, `star`, `archive`, `capture`, and `opml import|export`. Scheduling without a daemon returns `unsupported_feature`; manual refresh remains available directly.

Acceptance fixtures include each format, relative URLs, malformed dates/XML/JSON, huge/entity-expansion feeds, duplicate/reused IDs, reorder, self-URL changes, redirects and credential stripping, private feeds, 304 responses, restart, eviction, hostile HTML/media, permission denial, idempotent capture, and OPML round-trip.

## 7. Personal Ledger (`dev.vulcan.ledger`)

### 7.1 Bounded domain

The finance reference app is a personal double-entry bookkeeping ledger. It supports accounts, commodities, balanced transactions/postings, payees, categories, cleared/reconciled state, CSV import preview, reports, and deterministic CSV/JSON export. It explicitly excludes bank credential access, payment initiation, investment pricing, tax advice, payroll, invoicing, shared household replication, and automatic exchange-rate fetches.

Accounts and categories are optional mdbase-compatible records with stable ULIDs, names, types, commodity, and active/archive status. The canonical SQLite ledger contains:

```text
commodities(id, code, scale)
transactions(id, occurred_at, payee, description, created_at, source_digest)
postings(id, transaction_id, account_id, commodity_id, amount_minor, memo,
         cleared, reconciled_at)
imports(id, source_digest, imported_at)
```

All IDs are ULIDs. Amounts are signed 64-bit integer minor units; floats are forbidden. For every transaction, postings sum to zero independently per commodity. Foreign keys and checks are enabled. Published transactions are corrected by explicit superseding transactions, not silent destructive edits; deletion is allowed only for unposted drafts. An account/category record revision is captured when a posting plan is created, so stale or archived references fail apply.

The SQLite file is a canonical artifact selected during instance binding. Vulcan owns connection and atomic artifact capture. Cross-device file conflicts require review. No network capability is requested. Local report caches remain derived and disposable.

Views are `journal`, `transaction`, `accounts`, `import`, `reconcile`, and `reports`. Commands are `accounts`, `transactions`, `add`, `correct`, `import plan|apply`, `reconcile plan|apply`, `trial-balance`, `report`, `export`, and `doctor`.

Acceptance tests prove balancing, integer precision, multi-commodity separation, stale account revisions, duplicate import digest, correction history, reconciliation, migration interruption/rollback, database integrity failure, canonical artifact conflicts, audit/export reproducibility, permission filtering, and uninstall preservation.

## 8. Collection Studio (`dev.vulcan.collection-studio`)

### 8.1 Product boundary

Collection Studio provides generated list/table/detail/form views over one bound typed Markdown or mdbase collection. It does not introduce another schema language, database, query language, ownership model, or hidden record store.

An instance binds collection root, permitted types/contracts, default saved view, and writable paths. Read-only operation requires mdbase `core_read` and query profiles. Create/edit/delete is enabled only when Vulcan advertises the corresponding `core_write`/lifecycle profiles; otherwise the UI remains useful and visibly read-only.

### 8.2 Widget mapping and mutations

The v1 JSON Schema mapping is fixed:

| Schema | Widget |
| --- | --- |
| `string` | text; `enum` becomes select; `format: date/date-time` uses typed picker |
| `integer` / `number` | validated numeric input; no coercion from invalid text |
| `boolean` | checkbox |
| scalar `array` | ordered repeatable inputs |
| declared mdbase link | permission-filtered record picker |
| nested object | fieldset to depth 4 |

Unsupported unions, recursive schemas, deeper objects, custom widgets, or unknown formats render read-only JSON plus a diagnostic. Required, defaults, descriptions, bounds, patterns, and enum values come from the effective matched type composition; conflicted types disable mutation.

Create/edit/delete and bulk updates always produce a preview showing exact files, revisions, field changes, lifecycle-generated values, validation diagnostics, links affected, and permission decisions. Forms patch frontmatter while preserving body, comments, ordering, quoting, and unrelated fields whenever the shared mdbase mutation contract permits it. Raw-source replacement is not a Studio operation.

Views are `records`, `table`, `detail`, `form`, `schema`, and `import`. Commands are `collections`, `types`, `views`, `list`, `show`, `create`, `edit`, `delete`, `bulk plan|apply`, `import plan|apply`, `export`, and `doctor`.

Acceptance covers multiple matched types, defaults versus persisted values, required/uniqueness/path/link constraints, denied linked records, unsupported schema fallback, stale revisions, bulk atomicity, lifecycle values, multiple instances, `.base`/saved views, import collisions, and exact Markdown preservation.

## 9. Delivery mapping

The examples intentionally unlock in this order:

1. Minimal fixture and Ember Run base mode validate package/iframe isolation without vault authority.
2. Presenter validates read/render/embed/events/static publication.
3. Meeting Tool validates mutation plans and live sessions.
4. Collection Studio validates typed mdbase bindings and schema-driven writes.
5. Feed Reader validates local SQLite, jobs, schedules, network, secrets, blobs, and canonical capture.
6. Blobforge Workbench validates external processor jobs and artifact import.
7. Personal Ledger validates canonical SQLite, migrations, audit, export, and conflict preservation.

Wiki Quest, server WASM implementations, native Blobforge execution, and replicated structured stores are optional extensions after their separate capabilities pass conformance; none is required to call the initial reference portfolio complete.
