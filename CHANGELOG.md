# Changelog

## Unreleased

This release brings Vulcan into text editors and lets other programs use a vault through the
daemon. `vulcan lsp` gives Neovim, Vim, Helix, and other LSP editors link-aware navigation,
completion, diagnostics, and renames. The daemon gains a per-vault REST API with tokens, answers
routed CLI reads from its warm index, runs scheduled jobs, and batches auto-commits. Notes now
render with Dataview, Tasks, and embeds evaluated in the terminal, browse, exports, and editors.
Sync gains committed-history-only and path-scoped modes. Vulcan is still pre-alpha: keep
independent backups and review mutations with `--dry-run`.

### Highlights

- **Editor integration.** `vulcan lsp` is a Language Server for vaults: definitions and references
  for links, headings, and block IDs, hover previews, completion of links, tags, and properties,
  diagnostics, semantic highlighting, backlink lenses, due-date hints, task toggling, section
  extraction, multi-vault workspaces, and link-safe renames and moves, including in unsaved
  buffers. `vulcan editor nvim|vim install` installs the vulcan.nvim and vulcan.vim plugins this
  binary carries, and `vulcan editor helix install` merges configuration into Helix's own files.
  See `docs/guide/editors.md`.
- **Evaluated notes.** `vulcan render`, `note get --evaluate`, daily and periodic notes, browse
  previews, exports with `--evaluate`, and editors show inline expressions, Dataview, DataviewJS,
  Tasks, `.base` embeds, and embedded notes evaluated, read-only and under the selected permission
  profile. Terminal output is laid out as styled Markdown that fits the width, and browse draws
  images (Kitty, sixel, iTerm2, or half-blocks), follows links, and shows images full screen.
- **Daemon REST API.** Per-vault API tokens (`vulcan vault token create|list|revoke`) reach one
  vault's routes on the daemon's loopback listener: notes, search, queries, graph reports, history
  and diffs, doctor, vectors, health, and idempotent writes. See `docs/guide/rest-api.md`.
- **Routed reads.** With `--route daemon|auto`, `VULCAN_ROUTE`, or `vulcan device config set-route`,
  `query`, DQL, keyword `search`, `ls`, `tasks query`, graph reports, and `note info|links|backlinks`
  are answered from the daemon's retained index, never staler than running directly.
- **Schedules and batched commits.** `[[schedules.jobs]]` in the shared vault config runs `scan` and
  `batch-commit` jobs on named devices through the daemon; `vulcan schedule` lists, runs, and
  enables them, and `vulcan schedule trust` can require a signed commit for each job.
  `[git] strategy = "batched"` commits accumulated changes together, optionally with agent-written
  messages. See `docs/guide/schedules.md`.
- **Sync modes.** `sync.worktree = "none"` syncs only committed history, and `"paths"` with
  `sync.paths` syncs uncommitted edits only under those paths. Obsidian settings merge key by key,
  and newly ignored files stop syncing without being deleted anywhere.
- **Faster at scale.** Writes refresh only the paths they change (a note write at 100K notes takes
  about 90 ms instead of 600 ms), and note lookups, `status`, graph reports, task queries, and broad
  searches no longer do work proportional to the whole vault.

### Upgrade notes

- **Permission profiles you select must be defined on this device.** `--permissions <name>`
  (including for `vulcan mcp` and `vulcan lsp` started by an agent), a registration's
  `--permissions-profile`, MCP remote ceilings, and the schedule ceiling now refuse a profile that
  only the vault's synced `.vulcan/config.toml` defines or that the synced file widens, so a pushed
  edit cannot raise them. Run `vulcan config permissions profile pin <name>` to accept the vault's
  definition into `config.local.toml`, or define it there. Built-in profiles are unaffected.
- **Stable signing-key rotation, final step.** The stable channel now trusts only the
  hardware-held `stable-2026-10` key, and stable releases are signed only with it. The retired
  `stable-2026-09` key is no longer accepted. Installations on 0.3.0 update normally. Older
  binaries must first update to 0.3.0, which both keys signed, or be reinstalled from a
  checksum-verified archive or package.
- **DataviewJS follows vault trust.** Rendering commands now run DataviewJS in the strict sandbox by
  default and with the configured sandbox tier (still read-only) in vaults trusted with
  `vulcan trust`. `[dataview] render_dataview_js` or `--dataviewjs off` turns it off.
  `vulcan render` now evaluates vault notes unless given `--raw`.
- **Verbosity is counted.** `-v`, `-vv`, and `-vvv` select verbose, debug, and trace output, with
  `VULCAN_VERBOSITY` as the default. `kanban show --verbose` is now `kanban show --cards`.
- **Path-scoped sync needs this release on every device.** Live commits written with
  `sync.worktree = "paths"` use a newer format that older versions refuse.
- **Building from source needs Rust 1.90.**

### Added

- Editors: `vulcan lsp` serves definition, references, hover, completion (including inline-field
  keys), document and workspace symbols, pushed and pulled diagnostics, semantic tokens, code lenses,
  inlay hints, and code actions (create a missing note, pick an ambiguous target, convert link
  style, toggle a task, extract a section). Renames of a link's note, heading, or block, and file
  moves from the editor, rewrite links across the vault. `vulcan/render`, `vulcan/outline`,
  `vulcan/backlinks`, and `vulcan/status` requests and the `vulcan.*` commands (scan, capture,
  periodic notes, template insertion, note moves) serve editor plugins. An `[editor]` config section
  turns features on and off. Under `--permissions`, notes the profile cannot read are invisible to
  the editor.
- `vulcan editor nvim|vim install|status|path|uninstall` and
  `vulcan editor helix install|status|uninstall|print-config` install and update the bundled
  editor integrations. `self-update` refreshes installed copies and leaves locally edited ones
  alone unless `--force`.
- Rendering: `note get --evaluate`, `--dataviewjs off|static|on`, and MCP `note_get` `evaluate`
  return evaluated notes. Note embeds are transcluded, Obsidian `%% comments %%` are hidden, inline
  DataviewJS (`$=`) is evaluated where the vault enables it, inline fields render as Dataview's
  reading view shows them, and `strictLineBreaks` is honored. Markdown, JSON, ZIP, and SQLite
  exports take `--evaluate` and `--dataviewjs` (profiles: `evaluate`, `dataview_js`).
- Terminal output: tables wrap cells instead of truncating, callouts, quotes, nested lists, and
  `#tags` are styled, and `daily show`, `daily latest`, and `periodic show` print the note
  evaluated. Browse renders the preview on a worker thread, caches it, shows the whole source with
  line numbers (Ctrl-V), selects and follows links (Tab, Enter), and draws embedded images;
  `VULCAN_IMAGE_PROTOCOL` forces a graphics mode.
- Templater templates that use JavaScript run as one program, as Templater does, so declarations,
  conditions, and loops can span tags. Errors name the template line, `tp.system.prompt` and
  QuickAdd inputs come from `--var` or a terminal prompt instead of silently taking the first
  option, `template show` lists the inputs, and more of `tp.obsidian` and `tp.file.cursor()` work.
  See `vulcan help templates`.
- Daemon REST API (`docs/guide/rest-api.md`): API tokens, reads served from the retained index
  under the registration's permission profile, history and diffs, doctor, vector models,
  neighbors, and indexing, bulk property updates, note create, replace, patch, append, delete, move,
  inbox, and scan with required `Idempotency-Key`s, `GET /health` with per-vault watcher and
  consumer health, and `GET /openapi.json`. `GET /capabilities` lists the REST routes.
- `--route direct|daemon|auto`, `VULCAN_ROUTE`, and `vulcan device config set-route|clear-route`
  choose where routable reads run. `direct` is the default and never contacts the daemon; `auto`
  falls back to running directly whenever the daemon cannot vouch for a fresh index.
- Scheduled jobs (`docs/guide/schedules.md`): `[[schedules.jobs]]` with `every` or five-field
  `cron` triggers in an IANA zone, exact device IDs as nodes, a capped authority profile, and
  runtime and retry limits. The daemon coalesces missed runs and never overlaps a job, and
  `vulcan schedule list|show|next|history|run|enable|disable|trust` work without it.
- Batched auto-commits: `[git] strategy = "batched"` with `batch_interval_seconds` and
  `batch_messages = "template"|"deterministic"|"agent"`. The daemon commits once edits pause, and
  `vulcan git batch-commit [--dry-run]` runs one batch for cron or a timer. Agent messages need
  `vulcan daemon config set-auto-commit-agent` on the device; `vulcan daemon auto-commit-status`
  reports each vault.
- `sync.worktree = "none"` (committed history only) and `sync.worktree = "paths"` with
  `sync.paths` (uncommitted edits only under those paths), plus `sync.branch_push` to stop
  publishing local commits.
- `vulcan sync run --queue` hands a sync to the running daemon without waiting.
- `vulcan vault list` and `vault show` report each vault's `health` (missing, unindexed,
  conflicted, recovery_required, paused, failing, offline, syncing, or healthy), with or without
  a running daemon.
- `vulcan config permissions profile pin <name>` copies a vault-defined permission profile into
  `config.local.toml`.
- `vulcan device key status` reports where the device's private key is kept (`file_v1` today), whether
  it is available, and what its provider can do, without creating, signing, or prompting. A device
  identity whose manifest names a reserved provider (`system_secret_v1`, `ssh_agent_v1`) is now
  reported as degraded with that provider `unsupported`, instead of invalid; its key file is never
  used in its place. `vulcan device key verify` proves the provider holds the key by signing and
  checking a fresh challenge, without writing anything.
- `vulcan daemon config set-automatic-sync [--network unmetered] [--power battery-not-low|charging|...]
  [--defer-on-unknown]` lets the daemon's own sync cycles wait for an unmetered network or enough
  power. A held-back cycle is reported as paused with the reason and retried at the next poll.
  Manual, queued, recovery, and shutdown syncs always run, and the default is unchanged: no
  conditions.
- Sync's structured mergers are a closed, versioned registry (`vulcan_sync::ARTIFACT_MERGERS`), and
  each structured automatic resolution in a sync report now carries `merger`: the merger's `id` and
  `version` and the exact base, local, remote, and output blob IDs. The live merge commit records the
  same evidence as `Vulcan-Sync-Merger` trailers (at most 32, then `Vulcan-Sync-Merger-Omitted`).
- `vulcan vault add --git-init` creates a Git repository at the path first when it is not inside
  one (no commit, no remote). The registration is validated before the repository is created, so
  a refused add leaves the folder untouched.
- `vulcan vault discover [<dir>...]` finds vaults in a folder that are not registered yet: each
  immediate subdirectory holding `.obsidian/`, `.vulcan/`, or a `.vulcan.toml` pointer, with a
  free suggested wiki ID and the `vulcan vault add` command to run. It never registers anything.
  `vulcan vault scan-dir add|remove|list` remembers folders to scan (`scan_dirs` in `daemon.toml`).
- MDAF v1 gains optional document metadata and page labels, additive and backwards compatible.
  `info.json` may carry `language` (a BCP 47 tag) and `cover` (a declared image asset that
  represents the document), and `source-map.json` may carry `page_labels`: per paged source,
  label ranges in the model of PDF page labels (decimal, roman, or letter numbering with an
  optional prefix and first number), published only from evidence. `vulcan artifact validate`
  checks them (`language_invalid`, `cover_invalid`, `page_labels_source_unknown`,
  `page_labels_invalid`), and `artifact::page_label` turns a page into its printed label.
- Container Core's lineage section says how a derivative leaves members out truthfully: the
  omitting activity names them, earlier activities stop listing them as outputs, and an activity
  that read one names the parent package, declared as a non-embedded source, instead.

### Changed

- One process indexes each vault. The daemon, `vulcan serve`, MCP over HTTP, and the language
  server elect an owner through `.vulcan/index-owner.lock`; the others follow its changes and take
  over when it exits. `vulcan daemon status` shows a following index as `following`.
- Writes refresh only the paths they change instead of rescanning the vault: note, task, inbox,
  periodic, template, bases, and MCP writes, and REST writes. Note-name resolution, `note info`,
  `note backlinks`, `status`, graph reports, task queries, and broad keyword searches use indexes
  instead of whole-vault passes (cache schema v35 to v38, migrated automatically).
- `dataview query` and `dataview eval` print results with the same formatter as rendered notes:
  integral numbers without `.0`, null as `-`, and diagnostics as a warning callout.
- Completing a recurring task inline (`tasks complete`, MCP, or the editor) now inserts its next
  occurrence, as the Tasks plugin does.
- A daemon agent whose API key variable is unset is reported as unavailable in `daemon status`
  instead of stopping the daemon. A daemon consumer that exits restarts alone with backoff, idle
  daemons wake about 20 times less often, and a daemon whose state directory was deleted stops.
- `vulcan sync semantic-plan` and `semantic-auto` with `--agent` and no `--base-url` use the
  daemon's configured semantic agent instead of a local Ollama default.
- Live sync snapshots link the published checked-out branch, so a device that received commits
  through live sync adopts them when they reach the branch instead of showing them as a large
  uncommitted diff. Criss-crossed live history merges against a virtual merge base.
- Sync accepts only key-derived (`vdev1_`) device IDs; a leftover device ref in the older ULID form
  is refused with an error instead of listed.
- `vulcan vault add` of a Git-sync vault whose folder is not a Git repository now says so
  (`not_a_git_repository` in JSON) and skips device-key enrollment instead of attempting it.
- Sync errors from a bound vault and device-key probe results show `<device key>` and
  `<device key directory>` where ssh would name the key file (`Load key "…/id_ed25519": bad
  permissions`). The path is not secret; this keeps home-directory paths out of journals and
  notifications, as the device-key specs already require. The key stays at `device/id_ed25519` in Vulcan's data
  directory.
- YAML parsing moves from the archived `serde_yaml` to `yaml_serde`, the YAML organization's
  maintained fork, which drops the unmaintained `unsafe-libyaml` dependency (RUSTSEC-2024-0449).
  Frontmatter, Bases, and other YAML parse and serialize exactly as before (compared over about
  7,700 documents; see `docs/investigations/yaml-parser-replacement.md`). For library users,
  `ParsedDocument::frontmatter` and other YAML-typed APIs now use `yaml_serde::Value`.
- mdbase query and permission failures keep their canonical code. `vulcan mdbase query` (and
  other mdbase commands) now report `invalid_query`, `context_not_found`, or `permission_denied`
  in `--output json` instead of `operation_failed`, as does the Python example's
  `CollectionError.code`. The HTTP mdbase routes (`vulcan serve` and the daemon) include the same
  `code` in error bodies and answer coded failures with 400 (invalid query or parameter), 403
  (permission denied), 404 (missing record or view), or 409 (conflict or stale state) instead of
  500; uncoded internal faults stay 500.
- Per-device Obsidian layout files no longer need a separate step. Setting a vault up for Git sync
  (`vulcan vault add`, `vulcan vault clone`, `vulcan sync clone`) adds their `.gitignore` entries
  when the vault has a `.obsidian` directory; `--keep-device-files` skips it. Sync itself still
  never edits `.gitignore`: while a vault shares the files, `vulcan sync run` and
  `vulcan sync status` print a hint and the report lists them in `device_files`, and
  `vulcan sync doctor --fix [--dry-run]` adds the entries. The doctor reports `sync.device-files`
  as info once the work tree ignores the files and only the next sync remains. The `.gitignore`
  is now written with ordinary permissions instead of owner-only.
- Sync reports and daemon wiki status now include this installation's `device_id`, and
  `vulcan sync status` prints it. When a sync has to bridge this device's remote backup because it
  holds history this checkout did not write, Vulcan now warns (CLI stderr, daemon log
  `event=device_backup_bridged`): repeated bridges mean two checkouts or a copied device key share
  one ID. `daemon.toml` no longer carries an unused `device_id` ULID; existing files still load and
  drop it on the next save.
- Adding a tracked path to a committed `.gitignore` now stops syncing it while every device keeps
  its own copy: the next capture drops it, a device still editing it no longer conflicts, and
  devices applying the change leave their file in place. Files added with `git add --force`
  despite an existing rule stay tracked. `vulcan sync ignore-device-files [--dry-run]` adds entries
  for Obsidian's per-device layout files (`.obsidian/workspace.json`,
  `.obsidian/workspace-mobile.json`), and `vulcan sync doctor` warns (`sync.device-files`) while
  they are synced.
- Sync merges Obsidian's JSON configuration (`.obsidian/**/*.json`: settings, hotkeys, enabled
  plugins, plugin data) key by key instead of requiring review, so devices that change different
  settings or enable different plugins no longer conflict. Lists of distinct strings, such as
  `community-plugins.json`, frontmatter `tags`, or Bases column lists, now merge as sets: removals
  and additions from both sides combine, and one side's reordering is kept. The same setting
  changed on both devices, and other `.obsidian` files such as snippets, still need review.
- Sync conflicts no longer write a copy of each side's bytes beside the conflict record. Nothing
  read those copies; the bytes stay in Git behind the conflict's preserved refs and, later, in the
  conflict archive.
- Sync sweeps Vulcan's own refs at most once a day per repository: remote backups of devices that
  are no longer registered are deleted when live already contains them, local device recovery refs
  are released once their backup is gone. Backups that hold content live lacks are kept and reported
  by `vulcan sync doctor`.
- Plain `git fetch` and `git pull` no longer download Vulcan's hidden sync refs (device backups,
  conflict records): the daily sweep adds `^refs/heads/__vulcan-sync/*` to the remote's fetch
  refspecs and drops the remote-tracking copies Git made of them, and Vulcan's own fetches no
  longer create such copies. Git tools without negative-refspec support (Git before 2.29,
  isomorphic-git) cannot fetch from a remote configured this way.

### Fixed

- Rendering a note no longer lets DataviewJS write: browse previews, `render`, `note get`, and MCP
  `note_get` run it read-only whatever the sandbox tier. HTML rendering ran DQL over the whole
  vault regardless of the permission profile; evaluated reads now honor it, and `daily show` and
  `periodic show` check read access.
- `vulcan vectors` honors `--permissions`: commands that embed need network access to the
  provider, and commands that write the vector index need the `index` grant.
- Device-key SSH no longer joins the user's own `ControlMaster` connections, which could let later
  plain `git` to the same forge run with the vault's key. Notification discovery on a bound vault
  uses the device key too, instead of the user's credentials and two extra processes per check.
- List items and tasks after a nested sublist were left out of the index, task queries, and
  Dataview. Bare `key:: value` fields on list lines were indexed with the list marker in the key.
- `FROM ""` in DQL means the whole vault, as in Dataview. `sort by description` and
  `group by description` in task queries use the task text.
- `doctor` counts attachments reached through plain links and reports only media as orphans. Web
  Clipper's linked image embeds (`[![[img.png]]](url)`) parse correctly, and HedgeDoc image size
  suffixes are reported as unsupported syntax.
- Semantic automation (`semantic-auto`) works after its first application on a lineage.
- `sync doctor` checks a nested vault's cache ignore rules in the vault's own `.vulcan/`.
- Notes in a vault opened through a symlink (such as macOS `/var`) are recognized as vault notes.
- Templater `tp.file.path` and `tp.file.folder` follow Templater's semantics.
- HTML rendering no longer breaks inline fields into empty spans and prints integral numbers
  without `.0`.
- `file.inlinks` and the matches of an ambiguous link are listed in a stable order.
- Building with `js_runtime` but without `web` compiles.
- Native queries run typed predicates exactly as given. A JSON `query` predicate whose text value
  contains both `'` and `"`, or whose field is a property key with a space, failed with
  `invalid property filter`; typed predicates no longer pass through filter text, so both now
  match as written. Other predicates return the same notes as before.

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
  (`refs/vulcan/conflict-archive`) during sync, at most once a day. Each run adds one immutable
  segment, so archiving stays fast however much history accumulates. Their records, decisions, and conflicted file
  versions stay readable with `vulcan sync conflicts <id>`, while their directories and per-conflict
  Git refs no longer accumulate, locally or on the remote. `vulcan sync conflicts` reports the archived count, and
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
