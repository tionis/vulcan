# SilverBullet 3.0: integration review

Date: 2026-11-25. Source reviewed: `silverbulletmd/silverbullet` tag `3.0.0`
(`0f9b90bb896ccb996208ebdfcfce004623f38a6f`) plus the few fixes up to `6da7f263` on `main`, the
in-repo docs (`docs/`), the bundled Space Lua library (`libraries/Library/Std/`), and the 3.0
release notes (https://no.silverbullet.md/3, dated 2026-11-26). The licence is MIT.

This note re-baselines the SB appendix in `docs/ROADMAP.md`, which was written against 2.x. That
appendix assumed a private, browser-defined `/.fs` protocol and a TypeScript runtime, and built its
plan on top of those assumptions. Several of those assumptions no longer hold.

## What changed upstream that matters to Vulcan

| Area | 3.0 state | Why it matters |
|---|---|---|
| Server | Rewritten in Rust (`server`, `server-common`, `server-merge`, `server-space-tools`, `server-mcp`, `server-search` crates). Always multi-space, with accounts and spaces managed in a Dashboard. Single-space mode is removed. | Each space has its own URL prefix (for example `https://host/notes/`). A connector addresses *space URL + path*, not a host. |
| HTTP file API | Documented in `docs/Integrations/HTTP API.md`: `GET /.fs` (listing: `name, created, lastModified, contentType, size, perm`), `GET/PUT/DELETE /.fs/<path>`, `X-Sync-Mode: true`, `X-Get-Meta`, and `/.ping` (with `X-Server-Version`). | The protocol is now a documented integration surface, not just client internals. SB.1's "private protocol" caveat weakens. |
| Concurrency | `ETag: "sha256:<hex>"` on GET/PUT. `If-Match` and `If-None-Match: *` on PUT/DELETE return `412` when the precondition fails. The server three-way merges concurrent edits and writes conflict markers it can later resolve (`server-merge`: diff3 plus markers). | A client connector gets real optimistic concurrency based on content hashes. It no longer needs to guess from timestamps. |
| Auth | Session cookie, or `Authorization: Bearer <API token>` (tokens are issued per account in the Dashboard). OAuth device flow at `/.auth/device/code` and `/.auth/device` (used by SB-to-SB sharing). OAuth 2.1 with PKCE and dynamic registration for MCP only. | Headless automation uses an API token. MCP **refuses API tokens**, so Vulcan's daemon should talk HTTP, not MCP. |
| Revisions | Each space can keep Git-backed history: Managed (SB commits about 30 s after edits go quiet, at least every 5 min), Unmanaged (reads an existing repo, never commits), or Disabled. Commits are attributed to the account, to "SilverBullet", or to "External" for edits SB detected but did not make. `/.revisions/` serves JSON log, per-file history, content at a revision, and diffs. | SB can version and see the same Git history Vulcan produces. Vulcan's direct edits show up as "External" commits. |
| Git sync | A Managed space can fetch, merge and push to one remote branch (deploy key or server credentials). No rebase or force-push. Conflicts are reviewed in SB; marker removal resolves text conflicts. SB documents this as "primarily a backup mechanism". | It opens a zero-protocol integration path: SB and Vulcan meet at the same Git remote (see option A). |
| Search | Server-side full-text and optional semantic search: `POST /.search/query`, `POST /.search/related`, `GET /.search/status`. | SB.7's read-only plug value (search, related notes) is now largely native to SB. |
| Agents | `GET /.help` returns skills (pages tagged `meta/skill`), a space outline, and runtime availability. `/.mcp` exposes `help, read, write, edit, search, lua, screenshot`. The `sb` CLI covers `fs read/write/edit/rm --revision`, `search`, `lua`, `query`, and `help`. | This mirrors Vulcan's own skills and MCP design, so the two systems can share agent conventions. |
| Sharing | The `share.uri`, `share.mode` (`push`, `pull`, `sync`) and `share.transform` frontmatter keys are written by `Library/Std/Infrastructure/Share`. Providers: https (pull), GitHub and Gist (device-code login), and another SB space via that space's `/.fs` with a device-flow token. | This is SB's own selective-route model, and it overlaps with Phase 15. See the `share.hash` discrepancy below. |
| Publishing | `silverbullet publish <folder>` serves any Markdown folder as a public read-only site with the full client, live queries, and full-text search, and picks up file changes live. A Docker `publish` image is available. Alternatively, a Dashboard space can be made public-read. | This is a ready-made target for Vulcan exports and for 9.41 audience-safe publication. |
| Markdown semantics | Space Lua and Space Style now run **only from pages under `Library/`**. `CONFIG` moved to `Library/Config`. Bundled code lives in `Library/Repository/Std`. `${name = expr}` page variables. Baked sections (`<!--#lua …-->` body `<!--/lua-->`). `@name` mentions with `recipients`. A one-root-tag object model. `pageDecoration.tree.priority` is removed. | These are the inputs to SB.2 and the doctor rules. Executable content is now confined to `Library/`, which makes inert-by-default policy simpler. |
| Plugs | Plugs can declare dockable panel views. New `object.update/delete/append` and `index.awaitPendingChanges()` APIs. | A first-party plug (SB.7) would have a proper side-panel host. |

**`share.hash` discrepancy.** The 3.0 changelog says sharing "no longer changes the source page or
stores `share.hash` in its frontmatter". However, `Features/Share.md` and the shipped
`Library/Std/Infrastructure/Share.md` at both reviewed commits still read and write `share.hash`
to decide which side of a sync share changed. Vulcan should treat all `share.*` keys as
SilverBullet-owned metadata it preserves byte-for-byte. Vulcan must not use these keys for its own
bindings, because the repository constraints forbid sync markers in frontmatter. This point needs
rechecking at the next pinned release.

## Integration options, ranked by value per effort

### A. Shared Git remote (recommended first; nearly no new code)

Set up an SB Managed space with Git sync to a Forgejo repository. A Vulcan vault enrolled in Git
sync uses the same remote and branch. Both sides only ever add commits: SB merges, and Vulcan
rebases its own unpublished commits. Neither side force-pushes. This gives an Obsidian, Vulcan and
SilverBullet triangle without a SilverBullet protocol adapter.

Work needed:

- A **conflict-marker guard.** SB can leave committed or working-tree conflict markers while a
  conflict waits for review in its UI, and Vulcan has hit committed markers before. Vulcan's sync
  should detect diff3 or Git markers in tracked text, refuse to commit or push them, and report
  them in sync status. This guard benefits all users, not just SB users. It should be checked
  against `server-merge/src/markers.rs` so SB's marker dialect is recognised.
- **Layout policy for `Library/`.** Decide whether SB's `Library/` (Config, Repository/Std, Space
  Tree Outline) is synced, indexed, or excluded from Vulcan's scan, publication, and default
  queries. Vulcan should not index `Library/Repository/Std` as user notes.
- **Tests against an SB fixture tree.** Cover conflict copies (`<page>.conflicted-<sha8>`), SB
  commit authors, and `CONFIG.md` being renamed on first start.
- **User guide section.** Write a "Vulcan alongside SilverBullet over Git" page that is honest about
  SB calling its Git sync a backup mechanism and about commit latency (30 s to 5 min, plus remote
  check frequency).

The low-cost variant: when SB and Vulcan run on one host over the same directory, nothing is
needed beyond the layout policy. SB explicitly expects external editors, watches files, three-way
merges, and attributes such changes to "External". Vulcan's daemon watcher already handles the
other direction.

### B. HTTP client connector (Phase 15 SilverBullet routes; replaces most of SB.5)

The 3.0 file API is sufficient for selective pull and push routes with proper safety:

- Identity: the space URL from `/.ping` probing (as in `sbShare.findSpaceRoot`) plus the path.
  `X-Server-Version` gates the compatibility matrix.
- Reconciliation state: the last agreed ETag (`sha256:` of bytes) per binding, stored in Vulcan's
  durable route state outside `cache.db`. This is the same model as SB's share sync, but it keeps
  frontmatter clean.
- Push uses `If-Match` with the last ETag, or `If-None-Match: *` for creates. A `412` becomes a
  conflict with both versions preserved, never last-writer-wins.
- Pull uses the listing plus `X-Get-Meta` for cheap change detection. Bodies are fetched only when
  `lastModified` or `size` differ, and the hash is verified afterwards.
- Auth uses an API token from the environment or secret storage, never config. Device flow is
  optional later, for interactive enrolment.
- History: `/.revisions/` can supply the remote base version for a three-way merge proposal when
  both sides changed, without SB-specific merge logic in Vulcan.

This is also the cleanest **full-space mirror** (SB.5) if it is ever needed. The listing plus
conditional writes is what SB's own sync uses. Option A covers most mirror use cases more cheaply,
though.

### C. Publication target via `silverbullet publish`

Vulcan already plans SB.3 (`export silverbullet-space`) and has just added 9.41 audience-safe
publication. Combined: run an audience filter (for example player-safe pages of a GM vault), then
`vulcan export silverbullet-space --path site/`, then `silverbullet publish site/` or the `publish`
Docker image. That gives a navigable, searchable, read-only wiki with live SB queries. The SB
server reloads changed files live, so a scheduled daemon export keeps the site current. The export
must keep anything executable inside `Library/`, since 3.0 refuses to run scripts elsewhere, and
it must map Obsidian-only syntax using the SB.2 rules.

### D. Agent convergence

- Vulcan's daemon and agents should use SB's HTTP API with an API token. MCP needs per-client
  OAuth consent and rejects API tokens. Treat SB's MCP as something the user connects to their own
  assistants alongside Vulcan's MCP, not as a Vulcan dependency.
- Skills: SB's `meta/skill` pages and `/.help` closely match Vulcan's bundled skills. One option
  is to ship a Vulcan skill as an SB `meta/skill` page (installable as an SB library through
  `share.mode: pull`), so agents working in an SB space learn when to call Vulcan. A second option
  is a Vulcan skill describing how to work with an SB space (`sb fs edit --revision`, Library-only
  scripts, `system.reboot()` after external edits).

### E. First-party plug (SB.7): narrower scope

SB now has native full-text and semantic search and related pages, so a plug that only re-exposes
search adds little. The remaining distinct value is cross-vault graph and backlinks, Bases,
Dataview and TaskNotes results, doctor diagnostics, and Outline or other publication status. These
would appear in a dockable panel backed by Vulcan's REST API. This should stay low priority until
option A or B shows real demand.

### F. SB-compatible server peer (SB.4): deprioritise

With multi-space accounts, device-flow tokens, merging, revisions, and conflict review all
server-side in SB 3.0, reimplementing an SB-compatible server is a much larger target than it was
in 2.x. It also competes with running real SB over the same directory or a shared Git remote.
SB.4 should be moved to deferred work.

### G. Upstream Rust code reuse

`server-merge` (diff3 plus markers, depends only on `similar` and `sha2`) and `server-space-tools`
(page and path rules, skills) are pure Rust, MIT-licensed, and `publish = false`. They could only
be consumed as git dependencies pinned to a revision, which pulls in SB's workspace-inherited
dependency versions. Vendoring `server-merge`'s marker parser (with provenance) for the
conflict-marker guard is reasonable. Depending on SB's server crates is not, until upstream
publishes them.

## Effect on SB.2 (native Markdown semantics)

Add to the inventory:

- Treat `Library/` as SB's executable and config area. `space-lua` and `space-style` fences outside
  `Library/` are display-only in SB 3.0, and doctor should report them as SB lint does.
- Treat `${…}` expressions, including `${name = expr}` and `${local name = expr}` page variables,
  as inert source.
- Index baked sections (`<!--#lua …--> … <!--/lua-->`) as stale-able generated regions. Their body
  is ordinary Markdown that Vulcan indexes and publishes as is, and the expression is preserved.
  The same delimiters suit Vulcan's own export `evaluate` policy (SB.3) if Vulcan ever bakes DQL
  output.
- Recognise `@name` mentions (`recipients`) and data blocks; data blocks named after built-in
  tags are no longer indexed by SB.
- Preserve `share.*` frontmatter (see the discrepancy above).

## Proposed roadmap changes (not yet applied)

1. In the SB appendix header and SB.1, re-pin to 3.0.0 and replace the 2.x TypeScript file
   references with the documented HTTP API, the ETag contract, `/.ping`, and the Rust server.
2. Add a new item before SB.4: **SB.0 Shared-Git coexistence.** It covers the conflict-marker
   guard, the `Library/` layout policy, the fixture tests, and the guide (option A). Without the
   numbering, the marker guard alone is a Phase 12 item.
3. Rewrite SB.5 around ETag-based conditional reconciliation and API-token auth, and point the
   15.8 SilverBullet connector at it.
4. Move SB.4 to "Deferred SilverBullet work".
5. Narrow SB.7 to Vulcan-only capabilities in a dockable panel.
6. Add `silverbullet publish` as a named 9.41 and SB.3 target.

## Open questions

- Does Eric run SB 3.0 Server, Desktop, or both? Desktop's MCP listener and `sb` CLI target local
  folder spaces, which suits a single-machine setup (variant of option A).
- Should SB's `Library/` sync through Vulcan's Git remote at all, or stay SB-local?
- Is SB meant to be an editor over a Vulcan vault (option A), or a separate system Vulcan pulls
  selected pages from (option B)? The answer decides whether Phase 15's SilverBullet connector is
  needed at all.
