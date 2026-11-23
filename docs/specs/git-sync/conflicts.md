# Conflict preservation and resolution

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

> **Frozen (2026-11-23).** This part belongs to the whole-worktree live mode (`sync.worktree = "all"`/`"paths"`). It stays maintained as shipped but is not extended; once no wiki uses the live mode it is deprecated and removed, and geth takes over replication. New work targets [committed-history-only sync](none-mode.md).

- Unresolved Git merges receive deterministic conflict identities derived from the versioned policy
  hash, merge base, order-independent candidate tips, and sorted conflicted paths. When live history
  criss-crosses (for example because two devices linked the checked-out branch into the live chain
  independently), the candidates have several best common ancestors; like Git's recursive strategy,
  Vulcan folds them in sorted object-ID order into a reproducible virtual merge-base commit (fixed
  Vulcan identity and epoch timestamp, merged tree retaining any marker-bearing conflicts, the
  folded bases as parents), so the merge base, and every identity derived from it, is identical on
  every device and for either merge direction. Callers that only need shared ancestry, such as the
  published branch tip, use a real common ancestor. Structured concurrent additions likewise order
  by immutable candidate object identity, so swapping local/remote roles or trigger arrival order
  cannot change a resolvable tree. No wall-clock timestamp, arrival position, or model confidence
  participates in winner selection. Before a conflict is surfaced, base/local/remote commits are
  anchored under create-only `refs/vulcan/conflicts/<id>/` refs. For bounded regular-file conflicts
  with a unique base, Vulcan constructs a safe projection: normal clean merge results are retained
  and the already accepted remote object remains provisionally at each original conflicted path, but
  no synthetic conflict artifacts enter the canonical vault. Structural objects fail closed to refs
  and the device-local record. A reproducible two-parent conflict-record commit is anchored at the
  sibling local `record` ref and carries the conflict ID, merge base, device, profile, policy, and
  immutable sources in trailers; when a safe projection exists that commit anchors its exact tree
  rather than Git's marker-bearing conflict tree. Vulcan publishes that immutable commit under
  `refs/heads/__vulcan-sync/conflicts/<id>/record`, compare-and-swap publishes the same provenance
  commit as the live ref, then applies and verifies it through the ordinary Git-native worktree
  transaction before reporting the conflict; stale live refs or worktree drift restart or fail
  closed. Reports and durable records explicitly distinguish published and applied projections. A
  pre-existing preservation ref that names any other commit is a hard failure rather than an
  overwrite. `vulcan-app` writes an immutable record beneath the device-local sync state directory,
  including projection metadata, provenance ref, revision, and each side's object ID, mode, size,
  and content hash; the side bytes themselves stay in Git, reachable through the preserved refs and
  later the conflict archive, so no second copy is written. These device-local records never appear
  in the vault or Obsidian index, while the refs keep every original Git tree and creation
  provenance reachable independently of local state retention. Reviewed side and proposal
  resolutions keep the immutable original merge inputs, lease publication against the live
  provenance commit, parent the resolution from it, and publish a sibling remote resolution ref.
  Resolution commits similarly carry the conflict, chosen side, device, policy, and source trailers.
- `vulcan sync conflicts [<id>]` is a read-only projection over that state for either the selected
  path or `--wiki <id>`. A preserved divergence is an immutable conflict session whose bounded path
  pages carry deterministic resolution-group identities: ordinary blob conflicts are independent,
  structural changes remain conservatively atomic, and whole-tree validation failures remain
  indivisible. Mutable per-batch records beside the session bind an exact group selection, expected
  accepted revision, output tree and commit, recovery witness, and
  prepared/published/applied/needs-rebase state without rewriting the evidence. Unpublished work
  whose lease moved becomes rebase-required and may replan only if selected inputs are unchanged;
  published work can be applied through a verified later accepted descendant. The initial serialized
  sync report also stays bounded: its backend and durable-record conflict projections carry counts,
  completeness flags, at most 16 path/classification previews, bounded diagnostics, and a
  `detail_conflict_id` rather than embedding an arbitrarily large record; a projected worktree
  application likewise retains its aggregate action counts but serializes at most 16 path plans with
  explicit total and completeness fields. Each completed application report includes versioned,
  content-free `operational_stats` for this cycle: automatic and preserved path/group counts,
  formatting-candidate count, logical preserved-input bytes, an optional transaction-local Git
  subprocess count, and coarse whole-cycle/backend/conflict-state/cache timings. The counters never
  enter conflict identity, journals, or immutable evidence. Summaries expose total path/group and
  pending-group counts plus an explicitly complete-or-truncated 16-path preview; detail exposes
  derived group progress. Callers follow `detail_conflict_id` with `sync conflicts <id>` and can
  request `--path-offset` plus `--path-limit` for an explicitly bounded response. Current manifests
  read only overlapping evidence pages and expose `returned_groups` plus `groups_complete` beside
  global counters; older records retain a compatibility scan. Evidence pages split on serialized
  bytes and count, aggregate digests stream, and explicit path-count and diagnostics ceilings are
  checked before allocation or publication. The companion capability contract advertises the same
  256-path page and 128-group batch ceilings. Unpaged detail remains complete rather than silently
  truncated. Supersession is durable metadata beside the immutable record, never deletion or a
  fabricated resolution. Thirty days after a session is found resolved or superseded, sync
  maintenance moves it to a per-repository archive, at most once a day: the local ref
  `refs/vulcan/conflict-archive` names a parentless commit whose root lists immutable segments
  `sNNNNNN` plus a `count` blob. Each run writes its batch as one new segment holding each archived
  session under `<first two hex digits>/<id>/`, with its record, path pages, batches, resolution,
  supersession, and proposals as blobs and its conflicted base/local/remote versions as entries
  pointing at the original Git blobs under `sides/`, so contents are kept without copies. The root
  is rebuilt non-recursively from the existing segment entries, so a run costs its batch plus the
  number of segments, never the archive's size; an archive written before segments is wrapped
  unchanged as segment `s000000`. A lookup resolves the session's record path in every segment with
  one batched object query. A bounded batch moves per run, and each run compare-and-swaps the ref
  before removing the sessions' directories, markers, and local conflict refs, so an interrupted run
  is simply repeated. The same run then deletes the archived sessions' remote refs
  (`refs/heads/__vulcan-sync/conflicts/<id>/record` and published resolution refs) with one listing
  and one push carrying an exact lease per ref; nothing reads them back and their commits are in
  live history, and an unreachable remote only defers that cleanup. Detail reads fall back to the
  archive, listings report an archived count, and resolution refuses archived sessions. `vulcan sync
  conflicts-archive [--older-than-days <n>] [--dry-run]` runs the same move on demand under the
  vault and repository locks; a dry run takes no locks and changes nothing. Full base/local/remote
  commit snapshots are not retained once archived. Resolved and superseded are final states, so sync
  maintenance never rereads such sessions: a device-local index beside `conflicts/` holds one empty
  marker per possibly-open session, written before the session's record and dropped once a scan
  finds it resolved or superseded, and superseding or carrying forward reads only indexed sessions.
  Sessions found resolved or superseded also get a closed marker, so that a changed modification
  time of `conflicts/`, as when another version records a session without the index, triggers one
  full pass that re-indexes every session not already known closed without rereading closed records,
  and the read-only conflict listing repairs the index from the states it reads. The per-sync cost
  therefore follows the open sessions, not the repository's conflict history. A replacement may
  supersede an older session only when it covers every pending or rebase-required path; independent
  obligations and legacy grouped sessions remain actionable. Every sync that ends on an accepted
  live frontier, including one that records a new conflict and applies its projection, also carries
  forward an unresolved session with an overtaken group: a pending or rebase-required group with a
  path whose tree entry on that frontier differs from the session's recorded live input, so no
  resolution of the recorded sides can complete it; the check lists only that session's unfinished
  paths in the two revisions, so its cost follows the session's size rather than the vault's. Every
  unfinished path of that session is re-merged from the preserved base and local sides onto the
  frontier version exactly as a sync merges: a Git line merge, then the shared policy's structured
  or `prefer_local` handling. A local result already present on the frontier needs nothing; a clean
  result is written to the worktree only where the file still holds the frontier bytes and is
  published by a follow-up cycle of the same synchronization as an ordinary local edit; paths that
  still conflict, or involve deletions, non-regular files, or mode conflicts, move to a replacement
  session. The replacement keeps the original base and local commits, takes the frontier as its
  remote side, applied projection, and provenance, anchors those under its own preserved refs, and
  records `carried_from`. Because its local side is already an ancestor of the frontier, side, file,
  editor, patch, and proposal resolution of a replacement overlay its groups on the live tree
  instead of re-running the merge. The overtaken session is then superseded, naming the replacement
  when one exists, and keeps all evidence. A replacement that is itself overtaken is replaced the
  same way, but its successor names the original in `carried_from`, the original's supersession is
  repointed to the successor, and the intermediate record, artifacts, and preserved refs are removed
  unless someone started resolving it (batches, a resolution, or proposals): its base and local
  sides are the original's and its remote side is a past live commit, so a file another device keeps
  changing leaves the original plus one current replacement rather than state proportional to the
  number of syncs. An unchanged group reproduces its original conflict and moves to the same
  replacement, so the session's remaining work stays in one place. Structural groups (renames,
  directory/file and case collisions) are never re-merged path by path: they move to the replacement
  as one unit, keep their classification and therefore their structural grouping, and resolve
  atomically against the live tree. Whole-tree validation sessions, sessions with a resolution being
  published, and sessions whose worktree no longer matches the frontier are left actionable;
  carrying forward is opportunistic and never fails the sync. Human list/detail output explains
  editor, local-side, and remote-side workflows with dry-run guidance. It does not reconstruct
  conflict state from `cache.db` or require the daemon.
- `vulcan sync resolve <id> --side base|local|remote` is the deliberately narrow lossy-side mode.
  The selected side replaces only paths Git left conflicted, retaining clean three-way results.
  Repeatable `--group <group-id>` narrows that choice to complete bounded resolution groups: each
  batch overlays only its selected paths on the current accepted live tree, verifies that those
  provisional inputs have not changed, validates the exact whole tree, publishes through
  compare-and-swap and batch-specific refs/trailers, and leaves unselected groups actionable across
  restarts and unrelated live advancement. A side choice without `--group` whose recorded live input
  has since moved is applied the same way to every unfinished group, within the batch limit, instead
  of being refused; when the live commit is not yet present locally, resolution fetches it as a sync
  would. Reviewed complete-file resolutions use repeatable `--file <conflict-path>=<source>`
  arguments instead. Reviewed file, patch, and editor modes may also select complete groups with
  `--group`: a version-4 proposal binds the sorted group set, selection digest, and exact accepted
  frontier, overlays reviewed blobs on that frontier, verifies that no unrelated tree path changed,
  and uses the same durable group-batch publication and recovery transaction. Unlike side selection,
  reviewed proposal bytes never silently replan after the frontier moves. Dry-run validates exact
  files, applicable structured syntax, preserved state, worktree, and remote lease without
  constructing Git objects. This reviewed boundary accepts internal Obsidian state and binary bytes
  without disclosing them to a provider, and may create a regular file at a Git-synthesized rename
  destination whose three recorded sides are absent. A reviewed `--patch <file>` is interpreted
  against the immutable original local candidate through typed Git plumbing: check mode uses a
  temporary index and writes no repository objects, application requires the patch to touch exactly
  the selected complete-group paths, and only those non-deleted blobs are overlaid on the pinned
  accepted frontier. Deletion and unsupported rename/copy forms are rejected. Explicit `--editor`
  mode materializes conflict-ID-scoped base/local/remote markers for only the selected paths in a
  private temporary directory, invokes the standard `$VISUAL`/`$EDITOR` command parser, and rejects
  unchanged files or residual Vulcan markers before treating the cleaned results as supplied files;
  its dry-run performs all preflight checks but never launches the editor. Patch preparation and
  editor planning carry their captured selection/frontier into proposal creation, so movement during
  human review fails stale rather than rebinding the reviewed result. Before any mutating resolution
  queries the remote, Vulcan captures the current worktree under conflict-scoped recovery refs; it
  rejects stale worktrees, altered preserved refs, staged/index operations, and unsafe
  accepted-frontier changes. Prepared resolutions are durable and resumable, publication uses
  compare-and-swap, application follows remote acceptance, and completed state lives beside rather
  than inside the immutable conflict record. Terminal and rejected proposal records remain immutable
  audit history but do not prevent a later proposal. There is no implicit winner. Conflict
  classification may expose a conservative `formatting_candidate` review hint for
  whitespace-equivalent text sides, but the hint fails closed on formatting-sensitive Markdown and
  never weakens `require_review`. An explicit `sync format-propose` may operate only on selected
  formatting-candidate groups: it requires execute permission, an absolute formatter executable, an
  exact `--version` pin, optional explicit hashed configuration, bounded arguments/bytes/time, and
  formats private copies of base, local, and remote before requiring their normalized three-way
  merge to be clean. The executable, version, arguments, and config identity are bound into a
  review-only proposal on the captured accepted frontier; the command never accepts or applies that
  proposal.
- Agent-assisted resolution begins at a provider-neutral `vulcan-app` boundary only after a durable
  conflict exists. The synchronous provider receives bounded exact base/local/remote bytes plus
  policy identity and explicitly permitted focused context; immutable sides are inventoried in three
  batched Git reads independent of selected path count. Git’s `cat-file --batch` headers are parsed
  incrementally, so per-blob and aggregate limits reject declared oversized content before
  allocation, and the proposal-specific remaining budget is passed through each side read before
  provider invocation. Logical per-path accounting also rejects repeated paths sharing one object
  before cloning those bytes. A request may select up to the bounded complete-group limit;
  permission checks, eligibility, side inventories, provider disclosure, output coverage, and byte
  accounting then apply only to those paths, so unselected internal or binary conflicts are neither
  exposed nor disqualifying. The request and version-4 proposal identity bind the sorted groups,
  selection digest, and exact accepted remote/live frontier. Named permission profiles gate Git and
  every exposed path, internal Obsidian/Vulcan state and binary/missing selected inputs are
  excluded, and broad context is an explicit capability rather than ambient filesystem access.
  Explicit focused files are symlink-safe UTF-8 reads, sorted and deduplicated, sent with exact
  bytes and hashes, and retained as hash/size proposal inputs. A provider-neutral tool boundary adds
  fixed-call/fixed-byte `vault_read`, `vault_search`, `vault_query`, and `vault_links` operations;
  indexed operations apply `PermissionFilter` before materialization and policy hooks before
  disclosure, while full-file reads outside explicit context additionally require the caller's
  `--allow-broad-context` opt-in. The OpenAI-compatible adapter handles bounded tool turns; the
  distinct reviewed supplied-file path is local and performs no network or tool access. Proposal
  identity binds tool names, exact argument/result hashes, and returned paths without retaining
  tool-result content. Retained version-3 records continue to receive their original integrity
  verification. A provider cannot claim a context path that was neither supplied nor returned by a
  tool. Output must resolve exactly the selected conflicted paths within byte limits and cannot
  request deletion. Vulcan constructs an unreferenced proposal tree over the pinned accepted
  frontier, rereads every selected blob and mode, renders a bounded patch, verifies the worktree,
  remote frontier, and all preservation refs remained unchanged, and runs the same effective-config
  whole-tree link/deletion validator used by deterministic merging before atomically retaining a
  versioned `ResolutionProposal` outside the vault/cache. The proposal records explanation,
  referenced context, provider/model, prompt/tool contract versions, immutable conflict/policy
  inputs, path hashes, and validation evidence. Provider execution releases the repository mutation
  lock, is cooperatively cancellable, revalidates under a fresh lock, permits one ready proposal per
  conflict, and never writes the worktree or any ref. Approval has a read-only preview and a
  separately locked application transaction: it rejects stale inputs, unsafe Git state, moved live
  refs, invalid syntax, altered objects/patches, changed whole-tree validation policy, newly broken
  links, excessive deletions, an explicit rejection record, or a tree that cannot be reconstructed
  from the preserved merge; captures recovery state before remote access; publishes only with a
  lease; applies only the accepted proposal commit; and records a deterministic audit event without
  prompts, explanations, patches, or note content. Rejection is a separate previewable, idempotent,
  repository-locked transaction which retains the proposal and all original objects, writes only a
  deterministic content-free audit decision, and permanently closes that proposal ID against
  approval. The direct CLI exposes generation through `sync propose`, including repeatable
  `--group`, rejection through `sync reject`, exact approval through `sync resolve
  --approve-proposal`, and the composed manual transaction through `sync resolve --file`. Companion
  creation accepts the same group IDs and explicit accepted target while keeping provider
  configuration server-side. `sync propose --auto-accept` is a deliberate two-factor policy
  exception to human review: it is rejected before provider execution unless the caller also enabled
  device-local `sync.agent_auto_accept`, then persists the proposal and runs the exact ordinary
  approval transaction; failures leave the proposal reviewable and success uses a distinct
  content-free `auto_accepted` audit action. Credentials come only from an explicitly named
  environment variable, while endpoint access is permission-gated and responses are bounded exact
  JSON. Configurable retention, cross-device claiming, and additional provider protocols build on
  these boundaries rather than bypassing them.
