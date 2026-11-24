# Daemon runtime, services, packaging, and companion transports

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

- The daemon sync supervisor is transport-independent and persists a versioned, bounded job ledger
  below the platform Vulcan state directory as a SQLite store (`daemon/jobs.sqlite`) with one row
  per job, aggregate, and idempotency record; each transition writes only the rows it changed in one
  transaction, and a JSON ledger from earlier versions is imported once and kept as
  `jobs.json.migrated`. It coalesces duplicate queued triggers per wiki, creates at most one
  follow-up when events arrive during a running job, prevents two claimed mutations for the same
  wiki while allowing different repositories to progress concurrently, and exposes cloneable
  cooperative cancellation to the finite application workflow. Registered wiki, group, and all-wiki
  requests create a durable aggregate parent around normalized per-wiki children; parent state is
  derived from independent child outcomes and never claims a cross-repository transaction.
  Credential-scoped replay survives restart. Cancelling one aggregate preserves a coalesced child
  still referenced by another active aggregate and otherwise delegates to normal queued or
  cooperative child cancellation. A restart converts interrupted running jobs back to queued
  recovery work; Git-reachable snapshots and the application journal remain the authoritative
  byte-recovery boundary. The ledger persists transitions a restart must honor (enqueue with its
  idempotency record, claim, cancellation, completion) but not a running job's intermediate
  progress, which a restart discards anyway: progress lives in the daemon's memory and change
  notifications only.
- `vulcan daemon start` makes those library boundaries deployable without a second binary. One
  device-local process lock prevents duplicate runtimes; the process binds the configured loopback
  address before atomically publishing a bounded runtime record, loads the device companion
  credential, starts the registry-driven watcher/periodic-trigger coordinator and retained-job
  worker, and serves the companion router. Foreground Ctrl-C, service-manager termination, and
  authenticated `POST /shutdown` first enqueue a retained final sync for every active Git-backed
  wiki, allow up to 30 seconds for those jobs, cooperatively cancel unfinished work at the deadline,
  and only then set the stop flag; the listener, worker, and every watcher join before the owned
  runtime record is removed. A running daemon checks every 30 seconds that its runtime record still
  exists and names its own process; if the record was removed (its state directory deleted) or
  replaced, the daemon is orphaned and stops at once, without the final sync, instead of running
  unattended indefinitely. Each vault's sync, index, and auto-commit consumers sleep until its
  observation hub delivers an event, a debounce or batch deadline passes, or shutdown wakes them, so
  an idle daemon does not poll. A suspend-sized wall/monotonic clock gap adds a resume
  reconciliation immediately after wake on platforms that cannot provide a reliable pre-sleep event.
  `daemon status` authenticates a capability probe instead of trusting a stale PID file; a missing
  runtime is stopped, while existing runtime metadata whose probe fails is explicitly unresponsive
  rather than falsely labeled stopped. `--detach` re-executes the same binary with inherited device
  configuration and redirects output to the state-directory log. Quiet daemons emit structured,
  deduplicated, secret-minimal warning/error records when a job is conflicted, paused, or failed
  (wiki, job identity, category, retryability, and retained-status guidance), plus the existing
  specific checked-out-branch failures; full error text stays on authenticated status surfaces,
  while `--verbose` adds every completed-job summary. Native desktop delivery is an opt-in consumer
  of the same attention event. A device-local policy can ignore network failures or delay a single
  native alert until a failed-job count or observed duration; operational logs and remote sinks
  retain their independent event policy. Native notices identify the wiki, category, retryability,
  and job, while full error text stays on authenticated status. It runs on a bounded background
  queue, invokes the platform helper without a shell, times the helper out, and reports delivery
  failure without delaying or rewriting synchronization results. Remote JSON webhooks, ntfy
  publishing, and local command adapters consume the same versioned event through a bounded
  device-state ledger: the daemon persists intended targets before dispatch, reconciles retained
  terminal jobs at startup, uses stable job identities for receiver idempotency, retries with
  bounded exponential backoff, and retains completed records to suppress replay. HTTP follows no
  redirects, accepts only HTTPS or loopback HTTP, and reads optional bearer values from named
  environment variables; local adapters receive JSON on stdin through an absolute executable without
  a shell. Every delivery re-applies the affected wiki's network or execute permission. Sink failure
  is visible through rate-limited warning logs and `daemon alert-status` but never changes sync
  state. `daemon config
  show/set-bind/set-agent/clear-agent/set-notifications/set-notification-webhook/set-notification-command/remove-notification-sink`
  atomically manages non-secret device configuration with dry-run previews: agent entries contain
  only a validated OpenAI-compatible base URL, explicit model, and optional environment-variable
  name. Startup resolves named secret environment variables into memory and never persists or
  reports credential values. None of this changes direct CLI behavior.
- `vulcan daemon install/uninstall` projects that same foreground process into a native per-user
  service without introducing a second lifecycle implementation. Linux uses an atomically replaced
  `systemd --user` unit with restart-on-failure, Windows uses an XML-defined Task Scheduler logon
  task whose trigger and interactive-token least-privilege principal are both bound to the current
  user SID, and macOS uses a per-user LaunchAgent managed through explicit `launchctl` argument
  vectors. Windows task names include that SID so registrations from different users cannot collide,
  and installation does not require elevation or a stored password. The Windows task invokes the
  existing detached-start path, and installation explicitly runs the registered task rather than
  waiting for the next logon. The one supported Windows binary remains a console-subsystem
  executable but embeds Windows 11 24H2's `consoleAllocationPolicy=detached` application manifest,
  preserving normal output and blocking inside an existing terminal while preventing Task Scheduler
  from allocating even a transient console; no helper executable is required. Planning is explicit
  and mutation-free under `--dry-run`, service definitions contain no secret values, symlinked
  definitions fail closed, and uninstall preserves all Vulcan registry, credential, journal,
  conflict, and vault state. A permission-checked device-local `daemon.env` is resolved by a
  platform-neutral startup path where the service manager cannot safely provide an environment file;
  generated plists or task definitions never embed expanded credentials.
- Android defaults to finite wake-driven execution rather than a resident daemon. `sync
  termux-install` projects the ordinary registered-wiki `sync run` command into a private Termux
  wrapper managed by Android JobScheduler. The adapter requires the detached `android_shared`
  layout, respects Android network, battery-not-low, storage-not-low, optional charging, and
  reboot-persistence constraints, and uses the platform's 15-minute minimum period. Its stable
  per-wiki job and device-local bounded manifest are packaging state, not synchronization authority.
  A Termux shortcut, Obsidian resume/save bridge, or future native WorkManager client may invoke the
  identical finite command for lower latency; the scheduled job is a coalescible safety net and no
  Android-specific merge path exists.
- Distribution is archive-first and forge-neutral. Versioned per-target Linux, Windows, and macOS
  archives plus a distinct full-feature aarch64 Android/Bionic archive, deterministic amd64/arm64
  Debian packages, checksums, and a machine-readable artifact manifest form the canonical release
  contract; installer scripts and package-manager definitions project the same binaries and
  generated assets rather than rebuilding Vulcan's configuration or service semantics. The Android
  build retains the ordinary CLI feature set, generates QuickJS bindings against the matching NDK
  sysroot through `bindgen`, and disables LLVM's SLP vectorizer to avoid the verified AArch64
  release-optimization compiler crash. The POSIX installer selects the Android artifact only in
  Termux and must never relabel a GNU/Linux arm64 binary as Android-compatible. The signed-envelope
  update-channel contract defines `stable` and opt-in rolling `main` streams independently of any
  forge or registry; the latter is published under a distinct `rolling-main` channel tag that is
  created once and never moved, with only its release assets replaced in place, so it neither
  collides with the source branch nor requires a forced tag update. Portable manual installs may
  verify and atomically replace themselves through the shared application updater, while
  package-managed installs remain owned by their package manager. Signature requirements, trusted
  keys, and downgrade policy are local and cannot be weakened by remote metadata. Portable
  installations may opt into a device-local unattended schedule: Linux uses paired `systemd --user`
  oneshot/timer units, macOS uses a calendar-triggered per-user LaunchAgent, Windows uses a
  SID-scoped daily Task Scheduler definition, and Android/Termux uses a persisted periodic
  JobScheduler wrapper because Android cannot guarantee an exact wall-clock execution time.
  Scheduler definitions contain only the absolute executable and non-secret channel and condition
  options, invoke one vault-independent update cycle, and cannot enable unsigned metadata. A cycle
  downloads and verifies the complete candidate before interacting with the daemon, refuses an
  unresponsive runtime, gracefully stops a live daemon for the shortest replacement window, and
  restores its installed native service or detached lifecycle after either success or replacement
  failure. Optional failure delivery invokes the platform notification helper without a shell and
  never changes update results. Installation never registers a wiki or enables the daemon
  implicitly, and removal preserves device and vault state. Homebrew may expose its own explicit
  service integration using a stable package path, while the built-in service installer remains
  available for direct archives and Debian installations. Git stays an explicit package/runtime
  dependency for the initial synchronization backend, and Android remains subject to its separate
  real-device certification gate.
- Unattended work uses a reusable application-layer network and power policy with explicit
  unknown-state behavior. New portable self-update schedules default to unmetered network and
  battery-not-low, checks conditions before its metadata request and again before an archive
  download, and reports deferred cycles without treating them as failures. Android/Termux projects
  supported network, battery, and charging constraints into JobScheduler; Windows projects the
  optional charging requirement into Task Scheduler. Runtime probes use the active NetworkManager
  metering state and Linux battery data, plus bounded macOS and Windows battery readings, where
  available. Unsupported or unavailable readings remain unknown, rather than being inferred from an
  interface type; the default permits them to avoid indefinitely starving scheduled updates.
  Automatic sync can reuse the policy with its own defaults without changing manual sync semantics.
- A claimed daemon job invokes `vulcan-app`'s finite transaction rather than a daemon-specific Git
  path. The app observer persists its journal/apply marker first and only then forwards each phase
  to the supervisor's retained `SyncStatus`; registration identity, path, backend, pause state, and
  named permission profile are revalidated immediately before execution. Automatic triggers stop at
  a paused registration without discovering Git, while explicit manual jobs retain direct-mode
  semantics. Terminal success, conflict, pause, cancellation, or typed failure is atomically
  retained with the job and can be projected by later CLI or companion transports. Engine
  classifications such as network, busy, repository, configuration, and invariant cross the
  application boundary as structured data; the daemon must not flatten them into retryable `unknown`
  errors, because offline projection and repair guidance depend on category and retryability. A
  failed fetch or pull retains the daemon job as failed with `error` status; a deferred pull or a
  rejected push stays a detail of an otherwise successful job, and a paused branch lane retains the
  job as paused.
- Daemon status is reconstructed rather than maintained as a second authority. The shared state
  contract distinguishes clean, dirty, capture-pending, capturing, captured-unpushed, fetching,
  fetched, merging, pushing, applying, conflicted, paused, offline, and error. Reconstruction
  prioritizes an active retained job, then a retained application marker (written only by the
  removed live mode) and the durable transaction journal, unresolved legacy conflict records,
  registration pause state, and finally the latest terminal job.
  Retryable network/authentication failures project as offline, watcher work waiting in the queue
  projects as dirty, and journal phases retain transaction identity plus whether recovery is
  required. A retained journal error overrides its last active phase in the projected state, so a
  failed merge or apply is never displayed indefinitely as running. `daemon status` performs this
  reconstruction read-only for every registration, asking a running daemon over its authenticated
  local API so active jobs report live progress, and reading the ledger directly only when no daemon
  answers, reports the latest daemon-supervised attempt time from the retained job identity, It does
  not fetch a remote merely to produce status. Direct one-shot `sync run` invocations are outside
  the daemon job ledger. None of this state is inferred from `cache.db`.
- Each registered Git worktree has a daemon-owned recursive watcher feeding the observation hub.
  Because sync moves only commits, the sync consumer drains worktree hints without starting a
  cycle; lost events still queue a reconciling cycle, and a native watch on the Git directory's
  `HEAD`, `packed-refs`, and `refs/heads/` turns commits into debounced sync triggers. The watcher
  coalesces editor save sequences after a short quiet period but caps continuous activity with a
  maximum dirty age. Access events and
  `.git`/`.vulcan` paths are ignored; transient notify errors confined to ignored device-local state
  are discarded. Failed setup or a runtime native-watcher failure falls back to content-comparing
  polling, while a bounded incremental safety rescan detects a native watcher that remains connected
  but silently misses an edit. Watcher rescan notices and callback errors force a recovery trigger. Startup schedules an idempotent resume or
  recovery job before consuming events; periodic remote polling remains a separate trigger producer
  over the same supervisor.
- A synchronous trigger-runtime coordinator projects the current registry into watcher ownership. An
  active, unpaused Git registration owns exactly one watcher; removal, pause, or a watcher-relevant
  identity/path/backend change stops and joins it, while resume starts a fresh watcher and therefore
  a startup reconciliation. Watcher failure is retained as recovery metadata and retried on a later
  registry pass. Remote polling is adaptive and per wiki: each active registration has its own
  schedule, bounded by the vault's `sync.poll_min_seconds` (default 15, floor 5) and
  `sync.poll_max_seconds` (default 900). A terminal job triggered by anything other than a poll (a
  watched edit or commit, a manual, resume, or recovery sync), or a poll whose reported local,
  remote, or accepted revision differs from the previous job's, resets the wiki to its minimum
  interval; each poll then doubles the interval up to the maximum. The coordinator rescans retained
  jobs only when the supervisor's change generation moves. Local events, startup, and remote polling
  all converge on the supervisor's per-wiki serialization and coalescing rules. Runtime exit and
  coordinator errors stop and join all child watchers.
- Remote changes are discovered by the adaptive per-wiki polling described above, and on demand by
  activity signals: `POST /{id}/sync/activity` (companion credential only; `vulcan sync refresh` is
  the CLI client) queues a `SyncJobTrigger::Activity` job unless a job for the wiki is queued or
  running (it is joined: a queued job gains the trigger, a running one is returned) or the newest
  terminal job was queued less than 30 seconds ago (`fresh`, judged from the job's ULID). Activity
  is an automatic trigger: registration pauses and background policy hold it back like a poll. Its
  completion resets adaptive polling like any non-poll job. The response reports `queued`,
  `pending`, `fresh`, `paused`, or `not_synchronized` with the relevant job. Sessions send the
  signal themselves, in the background and only for registered wikis: every MCP session (stdio
  `vulcan mcp`, HTTP, and daemon-hosted remotes, at protocol-core creation) and every vault `vulcan
  lsp` opens, including each root of a multi-root workspace. Agent harnesses without MCP run `vulcan
  sync refresh` from a session-start hook. The HTTPS long-poll wake-up advertisement
  (`refs/vulcan/notifications`, formerly `docs/specs/realtime-sync-notifications.md`) is retired: it
  needed a custom ref, a forge webhook, and a relay per repository, and interacted badly with forge
  permissions. The daemon no longer reads the ref, and `SyncJobTrigger::RemoteNotification` remains
  only so retained job ledgers still deserialize.
- Companion transports project a versioned synchronous service rather than calling Git directly. The
  service owns capability negotiation, wiki lookup, reconstructed status, credential-scoped
  idempotent single-wiki and aggregate selection enqueueing, pause/resume, read-only inspection of legacy
  conflict records, activity signals, and child/aggregate job status and cancellation. Conflict
  resolution, agent proposals, and semantic-plan creation were removed with the live mode.
  Repository-reading and mutating calls reapply the registration's named permission profile. HTTP and WebSocket adapters add their transport capabilities without duplicating
  these workflows.
- Companion authentication uses a device-local credential outside every vault and rebuildable cache.
  The initial credential contains a 256-bit OS-random URL-safe bearer secret, a stable non-secret
  credential ID used to scope idempotency records, and a bounded exact Origin allowlist. Creation is
  atomic and no-clobber; token comparison is constant-time and debug output is redacted. Unix loads
  reject group/other-readable files, while Windows relies on the inherited ACL of the per-user state
  directory. Browser WebSocket clients authenticate with a bearer-bearing subprotocol because the
  standard WebSocket constructor cannot set an `Authorization` header; HTTP requests continue to use
  the header.
- The reference Obsidian companion is a thin protocol client, not another Git owner. Its
  distributable `main.js` is bundled from the dependency-light source under
  `integrations/obsidian-vulcan` for desktop and mobile, and the release assets are also embedded
  into Vulcan for `vulcan daemon companion install [<wiki>] [--dry-run]`. Installation is limited to
  registered vaults with a real `.obsidian` directory, updates only managed assets, preserves
  existing plugin data, and seeds the non-secret loopback endpoint and wiki ID only on first
  install. Non-secret loopback endpoint, registered wiki ID, debounce, event, and failed-sync-notice
  preferences may use ordinary plugin data; the bearer secret requires Obsidian 1.11.4+
  `SecretStorage` and is never passed through `saveData` or written by the installer. Manual sync
  first requests Obsidian's editor-save command and then enqueues the ordinary daemon job. Optional
  save triggering starts only after completed vault `modify` events, is debounced, and refuses to
  enqueue while reconstructed state is busy, paused, or conflicted. Status bar and WebSocket
  snapshots expose the daemon's state; by default the authenticated stream and 30-second polling
  fallback also produce one bounded Obsidian notice per durable failed job or retained failed
  transaction, deduplicated by its identity. Temporary companion connection loss changes status to
  offline but is not reported as a sync failure. The client validates loopback origins
  and protocol payloads, sends credentials only in authorization/subprotocol fields, and contains no
  Git execution path.
- The initial companion transport refuses non-loopback listeners. HTTP operations use JSON,
  `Authorization: Bearer`, `Vulcan-Protocol-Version: 1`, exact Origin checks when the header is
  present, and authenticated CORS behavior; manual sync, aggregate selection sync, and resume
  additionally require `Idempotency-Key` and return HTTP 202 with the retained job report. `POST
  /sync` accepts exactly one of `wiki`, `group`, or `all`, while `/aggregate-jobs/{job}` monitors or
  cancels the parent and ordinary `/jobs/{job}` continues to expose each child. WebSocket upgrades
  negotiate `vulcan.v1` and authenticate a second `vulcan.bearer.<token>` subprotocol, avoiding URL
  credentials and browser-inaccessible custom headers. The bounded event loop sends an initial
  snapshot and then only changed snapshots of registrations, reconstructed per-wiki status, retained
  child jobs, and retained aggregate jobs. Synchronous app/registry/supervisor work runs behind the
  async boundary with `spawn_blocking`.
  The concrete v1 wire contract is documented in `docs/companion_protocol.md`.
