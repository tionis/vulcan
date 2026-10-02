# Changelog

## Unreleased

### Changed

- `vulcan sync run` and `sync status` make far fewer remote round trips. An unchanged sync now
  queries the remote once instead of pushing the device backup, fetching the branch upstream, and
  querying the live ref separately. Device-key transports share one SSH connection on Unix. The
  private sync index stops re-reading racily clean files, which with Git LFS re-ran the clean
  filter on every sync. On a forge over SSH, an idle sync drops from about 5s to about 0.6s.

### Fixed

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
