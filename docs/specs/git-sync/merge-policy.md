# Merge policy, structured resolution, and whole-tree validation

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

> **Frozen (2026-11-23).** This part belongs to the whole-worktree live mode (`sync.worktree = "all"`/`"paths"`). It stays maintained as shipped but is not extended; once no wiki uses the live mode it is deprecated and removed, and geth takes over replication. New work targets [committed-history-only sync](none-mode.md).

- Merge policy is a versioned backend contract, not ad hoc daemon behavior. Ordered rules match
  portable case-sensitive Git-path globs plus content-aware file kinds, with fixed v1 defaults that
  permit structured handling for Markdown, JSON, Canvas, Bases, and Obsidian's JSON configuration
  (`.obsidian/**/*.json`: settings, hotkeys, enabled plugins, plugin data) while requiring review
  for other Obsidian state such as snippets and plugin code, binary data, and unmatched inputs. The
  one role-dependent exception is `.obsidian/workspace*.json`, whose built-in `prefer_local` rule
  keeps this device's copy when both sides changed: those files record per-device pane layout, carry
  nothing worth reviewing, and otherwise produce most real-world conflicts. `prefer_local` is
  deliberately role-dependent, so it is reserved for per-device state; it requires a surviving local
  regular file, still passes whole-tree validation, and a device-local review ceiling turns it back
  into required review. Its canonical hash is included in conflict identity and sync commit
  trailers. A device-local automation ceiling is deliberately outside that shared hash and may only
  replace an automatic policy result with required review; it cannot select a different tree.
- Configuration preserves that ownership boundary: `.vulcan/config.toml` may provide one complete
  `sync.merge_policy` plus shared `sync.tree_validation` ceilings, while `.vulcan/config.local.toml`
  may provide device-only `sync.merge_automation` and the default-off `sync.agent_auto_accept` gate.
  Shared automation/agent-acceptance settings and local policy or whole-tree-limit replacements are
  diagnosed and ignored. Alternate-index snapshots and worktree pre/postconditions always exclude
  the local config and rebuildable cache files, independent of user ignore rules, so device policy
  cannot enter canonical live history or create false worktree drift. The application layer resolves
  and validates the effective options before creating a transaction journal, so direct CLI, daemon,
  and companion execution use the same policy and malformed vault configuration cannot silently fall
  back during synchronization. An embedding caller's own review ceiling is combined restrictively
  and cannot be relaxed by config.
- A tracked path that a shared `.gitignore` starts to ignore stops being tracked, and every device
  keeps its own copy. The decision reads only the `.gitignore` files recorded in the trees being
  compared (the root and each ancestor directory, loaded per path rather than per tree), never the
  worktree, `info/exclude`, or `core.excludesFile`, so all devices agree; a path that was already
  ignored while tracked, such as one added with `git add --force`, stays tracked. A capture whose
  `.gitignore` files differ from its base drops the tracked paths they newly ignore without touching
  the files. A delete/modify conflict whose deleting side's tree newly ignores the path relative to
  the merge base resolves as that deletion (rule `gitignore-untracked`, validation check
  `newly_ignored`), so a device still editing the file cannot track it again and the `prefer_local`
  workspace rule does not undo the untracking. Applying an accepted tree removes such a path from
  Vulcan's private index before `read-tree -u`, so the file stays in the worktree as an ignored
  untracked file; deleting a path that was ignored all along still removes it. Devices on older
  Vulcan versions delete their copy instead. `vulcan sync ignore-device-files [--dry-run]` appends
  anchored entries for Obsidian's per-device layout files (`.obsidian/workspace.json`,
  `.obsidian/workspace-mobile.json`) to the vault's `.gitignore` when no shared `.gitignore` already
  ignores them, and `sync doctor` reports `sync.device-files` as a warning while the latest capture
  tracks such a file that neither its `.gitignore` files nor the work tree's ignore (info once only
  the work tree ignores it). Sync never edits the `.gitignore` itself: a finite sync lists the
  per-device files its accepted tree still shares in `device_files`, the CLI prints a hint, and
  `sync doctor --fix` applies the same entries. Registering a vault with a `.obsidian` directory for
  Git sync (`vault add`, `vault clone`, `sync clone`) adds them as part of setup unless
  `--keep-device-files` is given.
- Structured resolution is a second pass after ordinary Git merge and configured attribute drivers.
  It reads immutable base and candidate blobs, bounds input size and nesting, and supports recursive
  JSON objects, append-only arrays, arrays of distinct strings as ordered sets (an entry either side
  removed is removed, additions from both sides are appended in candidate-ID order, and one side's
  reorder of the surviving entries is kept, while conflicting reorders need review), stable-ID JSON
  Canvas arrays, Bases YAML, and Markdown frontmatter only when the Markdown body itself has an
  unambiguous three-way result. Apart from the `prefer_local` workspace files, the built-in policy
  merges `.obsidian/**/*.json` with the same bounded JSON merger (rule `obsidian-json-structured`),
  so concurrent changes to different settings or plugins combine while the same setting changed on
  both sides still needs review; every other `.obsidian/**` path remains required-review state, and
  a complete shared replacement policy may route any of these paths back to review. Deletion and
  absence are distinct from failure. Every path left conflicted by Git must resolve under the same
  shared policy before an exact alternate-index tree may be committed; unsupported kinds, syntax
  errors, overlapping scalars, ambiguous mode changes, and delete/modify conflicts preserve all
  original conflict inputs. Where concurrent additions need ordering, candidate commit object IDs
  provide an arrival-independent order. Reports name the file kind and policy rule used for each
  automatic resolution, while a device-local review ceiling prevents automation without altering the
  shared policy identity.
- The structured mergers are a closed, versioned registry (`vulcan_sync::ARTIFACT_MERGERS`):
  `vulcan.json-tree` (JSON and selected Obsidian JSON state), `vulcan.json-canvas`,
  `vulcan.bases-yaml`, and `vulcan.markdown-frontmatter`, each at version 1. Every entry declares
  its file kinds, whether its result is independent of which side is local, whether it needs a base,
  its byte and depth limits, its output form, its deletion semantics, and its post-merge validation.
  The merge policy selects among registered entries only; neither vault configuration nor packages
  can supply merge code. A version changes whenever an entry's output for some input could change.
  None of the four is role independent: concurrent appends are ordered by the candidates' commit
  IDs, which only sync knows, so they are not candidates for a Git merge driver. A structured
  resolution records `merger`: the entry's `id` and `version` and the exact `base`, `local`,
  `remote`, and `output` blob IDs (a side without the file has none), the output being the blob
  written into the validated resolved tree. The live merge commit carries the same evidence as one
  trailer per path, `Vulcan-Sync-Merger: <id>@<version> base=<blob|-> local=<blob|-> remote=<blob|->
  output=<blob> path=<JSON string>`, at most 32 of them followed by `Vulcan-Sync-Merger-Omitted:
  <count>`. These values follow from the merge inputs alone. Preserved conflict-record commits are
  reproducible and keep their existing message; for a projected conflict, the evidence is in the
  sync report and the record's merge tree holds the merged blobs. A merger only returns bytes; refs,
  the index, the worktree, and the cache are written by the sync engine after validation.
- Deterministically convergent artifact formats use that same second-pass boundary through a closed,
  versioned merger registry rather than acquiring sync-specific implementations. A registered merger
  declares exact path/media/format versions, whether it is commutative and role-independent, its
  structural and resource limits, deletion and compaction semantics, and validation/evidence
  contracts. Append-only event or database-change transports should prefer one immutable
  actor/sequence- or content-ID-named record per path, letting ordinary Git merge directory union
  without executing a driver. A packed event set may merge only by validated stable-ID union with
  canonical ordering; the same ID with different bytes, a missing predecessor/checkpoint,
  incompatible schema, invalid signature or digest, or a physical delete/modify remains a preserved
  conflict. A CRDT document merger may join two valid states and canonically serialize the result
  only when the pinned format proves convergence and idempotence; logical tombstones inside the
  document are not equivalent to deleting the artifact from the file tree. This registry is
  Vulcan-owned: apps and repositories cannot provide executable merger code merely by declaring a
  file type.
- The same registry may be exposed as a plumbing command for ordinary Git. A repository-scoped setup
  workflow installs narrowly named `merge.vulcan-<format>.driver` commands and plans explicit shared
  `.gitattributes` rules; it never globally claims generic extensions or silently rewrites existing
  attributes. The low-level driver reads Git's base/current/other temporary files, determines the
  format from an explicit registered selector and the conflict path, writes the current file
  atomically only after successful validation, and follows Git's `0` resolved, nonzero unresolved,
  and greater-than-`128` driver-failure convention. Because Git's external driver protocol does not
  supply stable candidate commit identities, only mergers proven independent of side labels and
  invocation order are eligible there; existing structured mergers that use commit IDs for
  deterministic tie-breaking remain internal. Installation, status, and removal are dry-run-capable,
  preserve unrelated Git configuration, and `sync doctor` verifies that every tracked
  `merge=vulcan-*` attribute resolves to the expected Vulcan executable and supported merger
  version. A complementary Git mergetool adapter may present the same deterministic attempt before
  human editing, but it does not claim to resolve structural Git conflicts that Git never passes to
  a content driver.
- Git and configured attribute drivers remain authoritative for clean text merges. Paths still
  unresolved afterward receive a stable classification covering overlapping text or binary content,
  delete/modify, rename/rename, directory/file, case-collision, device-local state, unsupported
  objects, and ambiguous shapes. The classification records the content kind, matched shared rule,
  configured action, effective required-review action, and a machine-readable diagnostic code. It is
  emitted in the finite report and persisted beside each immutable conflict artifact, so CLI,
  daemon, companion, and future agent workflows do not parse localized Git prose or mistake a failed
  structured attempt for permission to choose a side.
- Structured results carry explicit validation evidence. Before the merge commit or live-ref
  compare-and-swap, the backend has already enforced repository-path safety, parsed the selected
  syntax, checked Canvas and Bases root/entry shapes, kept a Markdown body's link-bearing surface
  byte-equivalent, rejected conflicted-file deletion, and reread the exact blob bytes and modes from
  the produced tree. The application-layer validator then enumerates immutable local, remote, and
  proposed trees through typed Git operations, parses every bounded Markdown blob with the effective
  vault configuration, and rejects any unresolved or ambiguous authored link not already problematic
  in either candidate. It also compares the union of candidate paths to the result under shared
  `sync.tree_validation` limits; both the absolute count and percentage ceiling must be exceeded
  before a merge is rejected, allowing ordinary small deletions while stopping mass-loss results.
  Successful reports add whole-tree link and deletion evidence, while failure returns to
  preserved-conflict handling before a merge commit or remote update. The validator keeps the link
  and alias summary of each parsed Markdown or Canvas blob in a SQLite store at
  `.git/vulcan-sync/merge-validation-cache.sqlite`, keyed by blob ID and cleared whenever the store
  format, parser version, build, or effective vault configuration changes. A merge looks up only the
  blobs of the trees it validates and appends analyses of unseen blobs, so neither reads nor writes
  scale with the store; once rows for blobs outside the validated trees outnumber them, the store is
  pruned to those trees. Link resolution and deletion counting still run over the complete trees
  every time, and an unusable store is rebuilt or bypassed rather than failing the merge.
  Immediately before every bootstrap, fast-forward, or merge push, the backend also rebuilds the
  complete worktree tree and requires it to equal the captured candidate; intervening editor writes
  therefore cause a fresh finite-cycle attempt while the remote ref remains unchanged.
- `vulcan-app` persists interruption-sensitive sync transaction journals under the platform Vulcan
  user-state directory (`XDG_STATE_HOME` or the platform/home fallback), keyed by the canonical
  materialized worktree. Journals are versioned, bounded, and atomically replaced; they never live
  in `.vulcan/cache.db` or the synchronized tree. A later daemon projects and advances the same
  reusable state rather than introducing a second transaction format.
- The direct sync doctor composes read-only `GitEngine` diagnostics with `vulcan-app` journal and
  cache inspection. It reports the resolved Git CLI/version, layout and operation safety, readable
  and agreeing hidden refs, remote reachability, advisory lock ownership, required filters/LFS
  availability, ignored cache files, retained recovery state, and cache/file inventory coherence
  without creating a repository, cache, ref, or state directory. Finite cycles perform the same
  tracked-path filter inspection before capture or remote access: each declared filter must have a
  process driver or both clean and smudge drivers, and Git LFS must also be executable. The
  successful report retains the typed readiness evidence; an unavailable driver fails as unsupported
  repository configuration before any sync ref can encode unfiltered bytes. A typed immutable-tree
  preflight uses the registered platform profile (or the native profile for an unregistered path)
  rather than the host running the test: case-fold and canonical-Unicode aliases plus
  Windows-reserved components are error-level incompatibilities, while non-representable executable
  modes, link-file symlinks, and filesystem-dependent long paths remain explicit bounded warnings.
  Finite direct and daemon cycles use the same recorded profile and retain both local and accepted
  preflights in successful reports. They capture an incompatible local tree, journal its object ID,
  and publish it only to that device's safety head before stopping; it never reaches canonical live
  or the worktree of another device automatically. An incompatible pulled or merged tree is rejected
  before canonical publication or worktree application, leaving current bytes and the accepted
  remote unchanged.
