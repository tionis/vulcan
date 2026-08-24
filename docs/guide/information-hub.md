# Local information hub and external wikis

Vulcan's long-term integration model is a local-first information hub. Ordinary Markdown files and attachments in a materialized vault remain inspectable and canonical, while explicit adapters exchange selected logical documents with other devices, storage providers, and knowledge systems.

This document describes both the implemented baseline and the planned architecture. Commands and configuration marked **planned** do not exist yet; see the roadmap for delivery details.

## Current implementation status

| Capability | Status |
| --- | --- |
| Local Markdown indexing, graph, search, properties, safe mutations, and rebuildable SQLite cache | Implemented |
| Query-driven exports, publication transforms, attachments, static sites, and frontend bundles | Implemented |
| Repository-level folder-note convention and structural conversion | Implemented |
| Outline-compatible ZIP export | Implemented |
| One-way local-vault-to-Outline API publication with durable mappings and conflict detection | Implemented |
| Scoped Outline collection pull with durable three-way state, attachments, reviewed moves, and missing-document policies | Initial implementation; generic routes and narrower remote selectors remain planned |
| mdbase v0.3 discovery, configuration validation, bundled schemas, and secure schema-reference validation | Partially implemented; later conformance profiles remain a candidate track |
| Multi-vault daemon and device/file-tree sync backends | Planned in Phases 10–12 |
| Generic external-document bindings and content routes | Planned in Phase 15 |
| Generic Outline routes, HedgeDoc routes, Git-wiki routes, and selective SilverBullet routes | Planned first connector wave |
| Full-Space SilverBullet protocol peer, SilverBullet plug, and optional runtime adapter | Planned optional capabilities after their daemon/sync boundaries exist |

The existing Outline publisher remains strictly one-way. The focused Outline pull command is a separate inbound operation with an explicit destination and its own durable state; running both commands does not create implicit bidirectional synchronization.

## Four different integration concepts

These terms solve different problems and should not be used interchangeably.

### Sync backend

A sync backend replicates the canonical working tree across devices or storage systems. Examples include Git remote sync, a supervised Obsidian or Seafile client, and passive Syncthing-style operation.

It moves files; it does not select documents, translate hierarchy, or bind one note to a remote wiki object. Device synchronization is Phase 12 work.

### External document binding

A binding says that a local Markdown note is related to a particular remote object. Useful relationships include:

- `reference`: the note points to or describes the remote object without transferring content
- `publication`: local Markdown is the source for a remote representation
- `import`: a remote document is materialized into the local note
- `mirror`: both are representations of one logical document, with explicit conflict handling
- `proxy`: the note stores searchable metadata or commentary for content that cannot be represented faithfully as Markdown

### Content route

A route defines an operation over one or more bindings or selected documents: pull, push, or an explicitly reviewed mirror. It owns selection, destination, authority, transformations, attachment/link behavior, deletion policy, limits, and scheduling hints.

### Connector

A connector implements one system's actual capabilities: enumerate, read, create, update, move, archive, attachments, hierarchy, revisions, link translation, authentication, and version checks.

Connectors advertise capabilities. A route that requires unsupported behavior fails during planning instead of silently approximating it.

## Hub-and-spoke flow

External systems do not relay content directly through Vulcan:

```text
SilverBullet --pull--> Imported/SilverBullet/*.md
                              |
                              +--push--> Outline collection

Projects/Planning.md --push--> HedgeDoc document

Git wiki worktree <----------> selected local namespace
```

A chained route such as SilverBullet to Outline consists of two separately journaled operations:

1. Pull SilverBullet content into an explicit local namespace.
2. Atomically write and index that canonical local state.
3. Plan the Outline publication from the resulting vault revision.
4. Apply the outbound route.

This creates an audit point, prevents hidden remote-to-remote copying, and lets users inspect or modify the intermediate Markdown.

## Planned frontmatter bindings

The generic binding schema is not implemented yet. The design target is a versioned structure similar to:

```yaml
---
vulcan:
  bindings:
    - route: team-planning-pad
      remote_id: Fk8S3m2
      remote_type: document
      relation: publication
    - route: company-outline
      remote_id: 01JXYZABC
      remote_type: document
      relation: reference
---
```

Important rules:

- Immutable remote IDs are identity; URLs are derived presentation data.
- Credentials, endpoint secrets, authority, deletion policy, and schedules do not belong in note frontmatter.
- Multiple bindings are allowed, but accidental duplicate ownership is diagnosed.
- An ordinary URL-valued property never activates synchronization implicitly.
- Query-managed bulk publications may keep mappings entirely in durable route state; Vulcan does not insert hidden markers merely to synchronize them.
- Binding edits use YAML-preserving, dry-run-capable vault mutation workflows.

Connector-native fields remain useful compatibility surfaces. [HedgeSync](https://community.obsidian.md/plugins/hedgesync), for example, maps a note to one HedgeDoc document through a configurable frontmatter property whose value can be a URL, note ID, or object. Vulcan plans to read and preserve that convention and offer an explicit migration to generic bindings rather than silently rewriting it.

## Planned route configuration

The exact schema will be validated during implementation. The intended separation resembles:

```toml
[integrations.profiles.team_hedgedoc]
connector = "hedgedoc"
base_url = "https://pad.example.com"
credential_env = "HEDGEDOC_SESSION"

[[integrations.routes]]
name = "planning-pad"
profile = "team_hedgedoc"
direction = "push"
authority = "local"
source = "Projects/Planning.md"

[[integrations.routes]]
name = "silverbullet-import"
profile = "team_silverbullet"
direction = "pull"
authority = "remote"
remote_scope = "Projects/"
destination = "Imported/SilverBullet/"

[[integrations.routes]]
name = "outline-publication"
profile = "company_outline"
direction = "push"
authority = "local"
query = 'from notes where file.path starts_with "Published/"'
```

Shared configuration contains non-secret topology and policy. Device-local endpoint overrides, executable paths, and credential environment-variable names may live in ignored local or daemon configuration. Credential values stay in environment variables or a device secret store and must never be logged.

Planned CLI surfaces include:

```sh
vulcan integration binding list
vulcan integration binding validate
vulcan integration route list
vulcan integration route plan silverbullet-import
vulcan integration route run silverbullet-import
vulcan integration run silverbullet-import outline-publication
vulcan integration run --all --dry-run --output json
```

Manual operations work without a daemon. The daemon adds schedules, route dependencies, cancellation, history, and authenticated remote triggers over the same request/report contracts.

## Authority and conflict behavior

Every mutating route declares authority:

- `local`: unexpected remote changes are conflicts; Vulcan does not overwrite them by default.
- `remote`: unexpected local changes are conflicts; Vulcan does not overwrite them by default.
- `review`: preserve both representations and produce a reconciliation artifact.
- true bidirectional behavior is future work requiring a durable three-way base; it is not last-writer-wins.

Pull removals are quarantined or otherwise made recoverable by default. Push removals archive remotely when supported. Unmanaged remote objects remain untouched.

## Durable state and repairability

Operational route state will live under an ignored `.vulcan/integrations/` state area, outside `.vulcan/cache.db`. It records information such as:

- connector, profile, route, and remote server identity
- local source identity and current path
- remote object ID, type, and parent
- last pulled remote revision/hash
- last pushed local and transformed-content hashes
- last agreed base hash
- attachment mappings, cursors, tombstones, and incomplete operation journal entries

Writes are locked and atomic. Malformed state stops reconciliation without mutation. Cache document ULIDs may be hints but are not durable external identity. Deleting `cache.db` and rebuilding it must not destroy bindings or change conflict decisions.

## First connector wave

### Outline

The current ZIP exporter and one-way API publisher are the outbound baseline. Phase 15 will adapt their shared planner/state concepts to the generic connector model and add a separately configured, scoped inbound route. See [Outline publishing](outline-publishing.md) for commands that exist today.

### HedgeDoc

The first goal is document-focused binding: push, pull, create, and open one HedgeDoc document associated with one Markdown note. Vulcan should prefer a supervised maintained HedgeSync/CLI boundary where practical and should not reimplement live operational-transform sessions without a stable protocol and concrete need.

### Simple Git-backed wikis

A Git-wiki connector works through a materialized worktree, maps a configured content root and file conventions, and imports or publishes a selected tree with explicit commit/pull/push policy. This differs from device Git sync: the connector translates a wiki content tree, while the sync backend replicates the canonical vault itself.

### SilverBullet

Selective page import and publication use content routes. Full-Space filesystem protocol synchronization remains optional Phase 12 work. SilverBullet-specific Markdown, runtime evaluation, and the first-party plug are separate capabilities and do not need to land together.

## Safety invariants

- The Markdown vault and attachments remain the inspectable canonical hub.
- SQLite and external-system indexes remain rebuildable derivatives.
- Every mutating operation has deterministic planning and structured reporting.
- Dry-run does not write local files, remote objects, mappings, locks, or journals.
- Remote drift and local drift are conflicts unless the route's recorded base proves an idempotent retry.
- Connector retries are bounded, interruption-safe, and credential-sanitized.
- Route dependencies are acyclic and never trigger endlessly from their own writes.
- Unsupported or lossy conversions surface diagnostics before apply.

See [the roadmap](../ROADMAP.md) Phase 12 and Phase 15 for implementation milestones.
