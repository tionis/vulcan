# Changelog

## Unreleased

### Changed

- Sync conflicts closed for 30 days move into a compact per-repository archive
  (`refs/vulcan/conflict-archive`) during sync. Their records, decisions, and conflicted file
  versions stay readable with `vulcan sync conflicts <id>`, while their directories and per-conflict
  Git refs no longer accumulate. `vulcan sync conflicts` reports the archived count, and
  `vulcan sync conflicts-archive --older-than-days <n>` archives sooner on demand.

- Every sync used to read every conflict record ever preserved for the repository to decide
  which unresolved ones to supersede or carry forward. A device-local index of open conflicts
  now limits that to the conflicts still open; resolved and superseded records are no longer
  reopened. Recording a new conflict no longer reads the resolution of every pruned conflict
  either.

- `vulcan inbox`, `daily append`/`periodic append`, `tasks create`, and the script APIs
  `vault.inbox()` and `vault.daily.append()` no longer treat a note they cannot read (for example
  one that is not valid UTF-8) as empty. They used to replace such a note with only the new entry;
  they now fail and leave it untouched.
- Note checkbox toggles, inbox and periodic appends, periodic note creation, script vault writes,
  and config edits (plugin toggles, export profiles, settings import) now replace files atomically
  instead of rewriting them in place, so a crash can no longer leave a truncated note or
  `config.toml`. The CLI note writes also refuse to overwrite a note that changed since it was
  read, and route notes in mdbase collections through collection validation like `note append`.

- A device-key mismatch now names both devices and the identity directory in use. When the
  installation's own key is the bound one, the message says this process reads a different
  identity directory (check its `HOME`/`XDG_DATA_HOME`) and warns against re-binding, instead of
  advising "re-bind", which would have locked the installation key out.

- Sync conflict resolution errors now say what to do next. A refusal because the live branch moved,
  a conflicted file changed again, or the worktree is out of date names `vulcan sync run` and when
  to use `--group`, instead of "require a fresh reconciliation" or "the remote live ref no longer
  matches the preserved conflict input".

- Conflicts in Obsidian's `.obsidian/workspace*.json` (open panes and layout) no longer stop a
  sync for review: when both devices changed it, this device's copy is kept. The new
  `prefer_local` merge-policy resolution can also be used in a shared `sync.merge_policy` for other
  per-device state; `sync.merge_automation = "require_review"` still turns it into review.

- A named MCP remote's dynamically registered OAuth clients moved from
  `mcp-remotes/<name>/oauth-clients.json`, rewritten whole on every registration and capped at
  1 MiB, into the store that holds its connection grants. Because registration needs no
  authentication, anyone who could reach the endpoint could fill that file until new clients were
  refused. Registration now drops clients that no connection grant refers to a day after they
  registered, with their secrets. The JSON registry is imported on first start and kept as
  `oauth-clients.json.migrated`; a registry that still holds inline secrets needs
  `vulcan mcp remote migrate-credentials` first, as before. The foreground `vulcan mcp --http`
  registry moved to SQLite beside its JSON path the same way.
- Remote MCP connection grants and refresh-token families moved from
  `daemon/mcp-authorizations.json`, which every authenticated request parsed in full up to three
  times, to an owner-only SQLite store (`daemon/mcp-authorizations.sqlite`). A request now reads
  only its own grant and writes at most once a minute. Approvals that expired or were revoked more
  than 30 days ago are dropped instead of accumulating until the entry limit blocked new
  connections. The JSON state is imported on the first write and kept as
  `mcp-authorizations.json.migrated`.
- The daemon job ledger moved from `jobs.json`, rewritten whole on every job change, to a SQLite
  store with one row per job (`daemon/jobs.sqlite`); running-job progress is no longer written to
  disk at all, and `daemon status` reads it from the running daemon. The JSON ledger is imported
  automatically and kept as `jobs.json.migrated`.
- `vulcan sync run` and `sync status` make far fewer remote round trips. An unchanged sync now
  queries the remote once instead of pushing the device backup, fetching the branch upstream, and
  querying the live ref separately. Device-key transports share one SSH connection on Unix. The
  private sync index stops re-reading racily clean files, which with Git LFS re-ran the clean
  filter on every sync. On a forge over SSH, an idle sync drops from about 5s to about 0.6s.
- Outline publish and pull state moved from one JSON file per profile to a SQLite store per
  profile (`.vulcan/publish/outline/<profile>.sqlite`, `.vulcan/integrations/outline-pull/<profile>.sqlite`).
  Each checkpoint now rewrites only the documents it changed, instead of the whole mapping file
  (and, for pulls, re-reading every content snapshot) once or twice per document. Existing JSON
  state is imported automatically on the next publish or pull and kept as `<profile>.json.migrated`.

### Fixed

- After `vulcan sync semantic-plan` or a semantic auto-commit, the next sync no longer retries
  until it gives up. Building the plan's trees left the private sync index without file stat data,
  so an unchanged worktree looked modified; plan trees are also built with a fixed number of Git
  processes instead of two per changed path.

- A structured sync merge whose result matched this device's files no longer retries until it
  gives up. Building the merged tree reused the private sync index and left it without file stat
  data, so the following working-tree check reported unchanged files as modified.

- `vulcan sync resolve <id> --side …` without `--group` no longer fails with "the remote live ref
  no longer matches" after another device synced unrelated changes. The choice now applies to every
  unfinished conflict group on the current live tree, and a live commit not yet fetched is fetched.

- The daemon now syncs with the installation's device key. It previously created and used a
  second, daemon-only key, so every vault bound to the device-key transport failed daemon syncs
  with "the device key changed since binding; re-bind" while `vulcan sync run` worked. The stray
  key under `~/.local/state/vulcan/sync/device-identity` is no longer read and can be deleted.
- A sync conflict whose conflicted files changed again in later syncs no longer stays
  unresolved forever. The next sync re-merges this device's version onto the live one: clean
  merges are published automatically, and files that still conflict move to a new conflict that
  resolves against the live version. Rename and directory/file conflicts move to the new conflict
  as one unit. Previously no resolution could complete such a conflict, so the wiki stayed marked
  conflicted. The original conflict's evidence remains inspectable, and a file another device
  keeps changing leaves only the original plus one current conflict, not one per sync.

- The trusted-vault list is written atomically and durably. A crash mid-write previously could
  truncate it, after which every vault was silently treated as untrusted.
- Applying a change pulled by `vulcan sync` no longer rewrites every file in the vault. Only the
  changed paths are written, so unchanged notes keep their modification times and Obsidian,
  watchers, and the Vulcan cache no longer see the whole vault as modified.
- Incremental scans now re-resolve every note's links when a note's `aliases` change, and stale
  resolutions from removed aliases or targets are cleared.
- `%%` inside code, math, or frontmatter no longer opens an Obsidian comment, so prose after such
  code is indexed and later comments stay out of search and embeddings.
- Inline tags accept non-ASCII letters and reject all-digit bodies such as `#1984`; trailing
  ` ^id` block IDs on paragraphs, quotes, and list items are recognised.
- Tasks plugin markers follow upstream: `📅` due (`📆`/`🗓` still read), `❌` cancelled, `🏁`
  on-completion, `⌛` scheduled, and `🔺` highest / `⏫` high. Task create and reschedule now write `📅`.
  **Behavior change:** tasks previously written by Vulcan with `🔺` for "high" now index as
  highest priority, matching the Tasks plugin.
- With `newLinkFormat` set to `relative` or `absolute`, links that miss the exact path now fall
  back to note-name and alias resolution as in Obsidian instead of being reported unresolved.
- Embedding requests cap each chunk at the model's `max_input_tokens` estimate; `vectors index`
  JSON output adds a `truncated` count.
- The embedding cap is script-aware: CJK, kana, Hangul, Thai, and emoji count as about one token
  per character, so such chunks are truncated instead of being rejected by the provider on every
  pass.
- Renamed and moved notes keep their document identity, so their chunks, embeddings, clusters, and
  link-suggestion state survive the rename instead of being rebuilt. **Behavior change:** scan
  reports count a detected rename as one `updated` document instead of one `added` plus one
  `deleted`.
- Accepted and rejected link suggestions persist in device-local state
  (`.vulcan/link-suggestion-feedback.json`) and survive `reindex`, cache migrations, and edits to
  the source note.
- `move` refuses files edited after it planned its rewrites, rolls back partial rewrites on
  failure, and rolls back a move interrupted by a crash at the next move or scan.
- New vaults track `.vulcan/templates/` in the default `.vulcan/.gitignore`. Existing ignore files
  are not rewritten; add `!templates/` and `!templates/**` by hand.

### Added

- Named MCP remotes with device-global definitions, concurrent instances, foreground/resident
  multi-vault hosting, IndieAuth login and per-connection consent, attenuated grants, and revocation.
  Legacy credentials migrate explicitly into protected-file SecretStore custody. Hosted writes
  expose durable operation status for timeout recovery. See `vulcan help mcp-remotes` for setup
  and recovery, and `vulcan help chatgpt-mcp` for the hosted-client workflow.
- Added Container Core v1, the rules shared by MDAF and wiki packages. The rules moved from
  MDAF v1 without changes, so existing artifacts stay valid.
- Added Knowledge v1, a source-neutral snapshot of cited entities and claims.
- Added Markdown Wiki Package v2, with required provenance, a note-to-source map, and a hosted
  knowledge snapshot. `exchange wiki export` now writes v2. `inspect`, `validate`, and `import`
  accept v1 and v2. Import records the package and member of mapped notes in `vulcan.source`
  frontmatter; `--source-locators full` also copies every span and locator.
- `integrations.routes.<name>.owner_device` restricts live route runs to one device so synced
  vaults cannot publish duplicate remote documents from two devices.

The parser version is bumped, so existing caches reindex on the next scan.

## 0.2.1 — 2026-09-06

Vulcan 0.2.1 is the first published stable release after 0.1.0. It contains all changes described
for 0.2.0 below and makes Android release builds compatible with both the current NDK 29 `lib/`
host-library layout and the legacy `lib64/` layout.

- Artifact imports now report source-byte coverage, per-note source sizes, diagnostic counts, and
  explicit semantic-review guidance for unusually large generated notes.

## 0.2.0 — 2026-09-06 (tagged, not published)

Vulcan 0.2.0 advances the project from a local vault CLI into an experimental multi-device
information hub. It remains pre-alpha; keep independent backups and review mutations with
`--dry-run`.

### Highlights

- Added finite Git-backed device synchronization with recoverable checkpoints, deterministic
  conflict artifacts, reviewed resolutions, semantic proposals, and notification-driven pulls.
- Added the multi-vault daemon, authenticated HTTP and MCP surfaces, native user-service
  definitions, background watching, and scheduled synchronization.
- Added Android/Termux support with detached Git storage, platform preflight diagnostics, a focused
  clone workflow, scheduler helpers, and a full-featured aarch64 Android release artifact.
- Added bidirectional, conflict-aware Outline workflows, named integration routes, MDAF import
  foundations, EPUB and static-site publication, and durable reconciliation state outside the
  rebuildable cache.
- Expanded Dataview, Bases, TaskNotes, Kanban, periodic-note, template, plugin, custom-tool, and
  agent-skill compatibility.
- Added immutable stable and bounded rolling release channels, channel-scoped signed update
  metadata, checksummed portable archives, Debian packages, installers, and self-update support.

### Upgrade notes

- Git 2.38 or newer is required for the Git synchronization backend.
- `v0.2.1` is the stable update-signing trust bootstrap. Install it once from a manually verified
  checksum, archive, or package when upgrading from `v0.1.0`; later stable updates can be verified
  through the embedded `stable-2026-09` identity.
- Synchronization state, credentials, journals, and publisher mappings are durable device state and
  are intentionally stored outside the rebuildable SQLite cache.
