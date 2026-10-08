# Changelog

## 0.3.0 — 2026-10-08

Vulcan 0.3.0 makes multi-device use dependable. Sync authenticates with per-device keys, can
manage deploy keys on a forge, and turns conflicts into small, resumable groups that can be
reviewed. One daemon now hosts every vault together with its MCP endpoints, previews, alerts, and
scheduled self-updates. Typed mdbase collections arrive, and DQL, Bases, and note queries now share
one query planner. Vulcan is still pre-alpha: keep independent backups and review mutations with
`--dry-run`.

### Highlights

- **Device-key sync.** `vulcan device init` creates a per-installation key, and
  `sync transport bind` makes a vault's Git sync use it over SSH, with no agent and no fallback.
  Devices publish registrations (`sync devices list|register|revoke|set-name`). `sync forge`
  reconciles Forgejo deploy keys with those registrations, logs in with OAuth, and works across
  the fleet with `--all-wikis`. `vault clone` and `vault add` enroll the device key automatically,
  and `device replace` with a fleet-wide revoke covers device rotation.
- **Conflicts you can work through.** A conflict is split into deterministic groups. Each group can
  be resolved by side choice, patch, editor, or a reviewed agent or formatter proposal, and progress
  is kept across syncs. The daemon can resolve safe text conflicts by itself. Conflicts that later
  syncs overtook are carried forward instead of getting stuck. Closed conflicts are archived after
  30 days. Conflict records now live outside the vault tree.
- **One daemon host.** Named MCP remotes provide IndieAuth login, per-connection consent,
  attenuated grants, revocation, and protected-file secret custody. The daemon supervises their
  listeners, the vault HTTP services, and site/bundle previews (`site serve`, `bundle serve`) as one
  host, reports health per wiki, and syncs before it shuts down gracefully.
- **Sync alerts.** Failed syncs raise desktop, Termux, and Obsidian-companion notifications with
  configurable thresholds, and are recorded durably until reconciled.
  `vulcan daemon companion install <vault>` installs the bundled Obsidian companion plugin.
- **Unattended self-update.** `vulcan self-update schedule` installs a timer for portable installs,
  with network and power policy (`vulcan self-update run` performs one pass).
- **mdbase collections.** `vulcan mdbase` adds typed records, CEL queries, link semantics,
  effective-schema reports, saved views (including Obsidian `.base` sources), and a validated,
  journaled, revision-checked write pipeline (`mdbase patch`). `mdbase conformance` reports which
  profiles are claimed. See the new mdbase guide and agent skill.
- **Faster, safer queries and writes.** DQL, Bases, Tasks, search filters, and DataviewJS run
  through one planner that loads only the notes a query needs (`bases eval --explain` reports a
  view's plan). Every surface evaluates under the caller's permission profile. Ordinary note, task, template, and move
  writes are journaled, coordinated across processes, and recovered after a crash.
- **Nested vaults.** A vault can be one directory of a larger repository, such as an MkDocs `docs/`
  directory or notes beside code. A root-level `.vulcan.toml` (`vulcan init --repository-pointer`)
  lets Vulcan find it from anywhere in the checkout.
- **Daily notes.** `vulcan daily open [date]` accepts relative dates. `vulcan daily calendar` is a
  month picker that previews and edits any day's note.

### Upgrade notes

- **Stable signing-key rotation, step 1.** This release adds the hardware-held `stable-2026-10`
  key (`sshsig-ed25519`, SSH fingerprint `SHA256:HYpEy7eKcHkSUuL1y4ADmFk89G0zkkI0jfVXucG/juQ`)
  to the trusted stable keys, and its update descriptor is signed by both `stable-2026-09` and
  `stable-2026-10`. `v0.2.1` verifies the update through `stable-2026-09` as usual. Please update
  every installation to 0.3.0: a later release will stop signing with `stable-2026-09`, and older
  binaries will then need a manual, checksum-verified install. Rolling `main` builds keep
  `main-2026-09`.
- An update check that finds a descriptor which is not signed yet now reports that it is awaiting
  signing and asks you to retry, instead of claiming that no trusted key signed it. Rolling and
  stable builds no longer publish an unsigned descriptor at their public URL.
- The daemon now syncs with the installation's device key. The separate daemon-only key under
  `~/.local/state/vulcan/sync/device-identity` is no longer read and can be deleted.
- Several JSON state files move to SQLite on first use (job ledger, MCP authorizations and OAuth
  clients, Outline publish/pull state). Originals are kept as `*.migrated`. A remote whose registry
  still holds inline secrets needs `vulcan mcp remote migrate-credentials` first.
- Task markers now follow the Tasks plugin. Tasks that Vulcan previously wrote with `🔺` for "high"
  now index as highest priority.
- The parser version is bumped, so existing caches reindex on the next scan.

### Added

- `vulcan device` manages this installation's identity (`init`, `show`, `public-key`, `config`,
  `replace`, `repair-permissions`), including Windows identities protected by verified private ACLs.
  Sync uses key-derived device IDs as its actor, and `sync devices list` shows friendly names and
  an offline recovery inventory.
- `vulcan sync transport bind|status|unbind`, `sync devices register|revoke|unregister|set-name`,
  `sync forge init|set|show|clear|sync|login|logout|authorize-self`, and `vault enroll`
  (also `--all-wikis`) set up device-key transport and forge deploy keys.
- Conflict groups with incremental, scoped resolutions (side, patch, editor, agent and formatter
  proposals), bounded and paged conflict reports, conflict-storm metrics, retained partial
  resolutions, configurable daemon conflict-resolver workers, and auto-resolution of safe text
  conflicts.
- Daemon sync alerts: per-wiki health, desktop notifications, durable remote alerts, native Termux
  notifications, Obsidian companion notices, and configurable failure thresholds.
- `vulcan daemon companion install` installs or updates the bundled Obsidian companion.
- `vulcan self-update schedule` and `self-update run` add unattended portable updates with network
  and power policy.
- Managed directory profiles for sync, including a files-only profile.
- `vulcan mdbase` commands: `status`, `types`, `schema`, `validate`, `read`, `query`, `views`,
  `view`, `view-source`, `patch`, and `conformance`. They provide bounded CEL evaluation, link
  helpers, saved views, crash-safe write journals with revision preconditions, migration of
  TaskNotes v0.2 assets, and standalone shell and Python integrations.
- The shared query planner serves DQL, Bases, Tasks, search filters, and note queries, with explain
  reports, lazily loaded DataviewJS pages, and sorted top-k queries that stop early.
- Vaults nested in larger Git repositories and MkDocs sites, plus `.vulcan.toml` repository
  pointers resolved by discovery, `vault add`, and `vault clone`.
- Hosted site and bundle previews with live reload and last-good-output rebuilds.
- MCP: stable bounded retrieval tools, authenticated HTTP request cancellation, per-instance
  connection limits, and durable status for hosted writes.
- Artifact imports are adaptive by default.
- `vault.create` accepts string content.
- `vulcan daily open [date]` opens or creates the daily note for any day, with `--dry-run` to
  preview the resolved path. Date arguments across `daily` and `periodic` now accept `yesterday`,
  `tomorrow`, signed offsets (`-1`, `+3`, `-2w`, `-1m`), and `last <weekday>` / `next <weekday>`
  besides `YYYY-MM-DD`.
- `vulcan daily calendar` (and bare `vulcan daily` in a terminal) is a month-calendar picker:
  it marks days with notes and events, previews the selected note, and creates or edits any day's
  note in `$EDITOR` from the daily template without leaving the calendar.
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

### Changed

- Rolling and stable releases never serve an unsigned update descriptor. A rolling build stages its
  descriptor as `vulcan-update-channel.unsigned.json` until the signer replaces it, and a stable
  release becomes `latest` only after it is signed.
- The updater accepts `sshsig-ed25519` signatures, so a smartcard-held SSH key can sign stable
  releases. It trusts the new `stable-2026-10` key alongside `stable-2026-09`.
- `.md` is recognised as Markdown in any letter case.
- "Today" for daily and periodic notes is now the local calendar day rather than the UTC date,
  so late-evening or early-morning work no longer lands in the neighbouring day's note.
- Creating a periodic note for another day renders template built-ins such as `{{date}}` as that
  note's date instead of the current date. `tp.date.now()` is unchanged.
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
