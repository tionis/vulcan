# Semantic history

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

> **Frozen (2026-11-23).** This part belongs to the whole-worktree live mode (`sync.worktree = "all"`/`"paths"`). It stays maintained as shipped but is not extended; once no wiki uses the live mode it is deprecated and removed, and geth takes over replication. New work targets [committed-history-only sync](none-mode.md).

- Semantic history is a separate, reviewable projection over an accepted live snapshot. `vulcan sync
  semantic-plan` resolves immutable source and target revisions, requires the target to agree with
  local and remote live refs, and groups paths deterministically by top-level area, individual file,
  one complete change set, typed Git change, or separated textual hunk. Change grouping preserves
  rename pairs (and dependent rename chains) atomically and uses delete-before-add ordering so
  file/directory transitions remain valid. Hunk grouping splits only safe ordinary modified-file
  unified diffs at actual hunk boundaries; additions, deletions, renames, type changes, binaries,
  unsafe shapes, and single-hunk files retain atomic change behavior. Each hunk patch applies to the
  preceding isolated tree and may change only its declared path. Every strategy constructs and
  validates each proposal commit through typed Git plumbing and anchors the exact final tree under
  `refs/vulcan/proposals/semantic/<plan-id>`. Group paths are sorted independently of Git output
  order, and the selected strategy is retained in the versioned device-local plan beside bounded
  patches. Version-6 dry-runs expose exact split patches without creating objects or state, while
  materialized plans still prove final-tree equality. A provider-neutral optional planning boundary
  receives only bounded exact per-path patches and may propose ordered whole-file groups plus
  messages; plan records retain provider/model/prompt identity. Its output must cover every accepted
  changed path exactly once, cannot name other paths, cannot inject reserved provenance trailers,
  and still passes the same exact intermediate-commit and final-tree checks, so it organizes
  accepted bytes without inventing content. The first direct adapter uses an explicit
  OpenAI-compatible endpoint/model, obtains an optional key only from a named environment variable,
  refuses redirects, permission-checks network access, and accepts bounded exact JSON; `--agent
  --dry-run` still calls the provider but creates no Git objects or state. `semantic-apply`
  revalidates source, proposal, target, worktree safety, ancestry, and exact-tree assumptions before
  compare-and-swap advancing the semantic branch; when the semantic branch is the checked-out branch
  it first moves the normal index with the same two-way index-only merge used for adoption, so the
  checkout reports no spurious staged reversal and worktree bytes are untouched. Because live links
  the published branch, an applied and published semantic tip becomes an ancestor of the next live
  snapshot, so repeated planning keeps satisfying the source-ancestry requirement. A semantic branch
  that is not checked out is never linked; its applied tip instead satisfies the requirement when
  its `Vulcan-Semantic-Target` trailer names an ancestor of the new target whose tree equals the
  source tree; it never rewrites or advances live sync refs, and after durable application it
  lease-deletes the redundant proposal ref while the semantic branch retains the exact commits.
  `semantic-publish` separately revalidates the applied tip and exact target tree, requires the
  remote semantic ref to equal the recorded source (or already equal the tip), pushes with an exact
  object lease, and records publication provenance for idempotent retries; it never force-pushes or
  changes live refs. `semantic-reject` provides the complementary dry-run-first retention decision:
  under the repository lock it verifies the exact proposal tip, journals `rejecting`, deletes only
  that ref with an object lease, and retains a bounded rejected plan record for idempotent audit
  without keeping the proposed history reachable. Dry-run creates no objects, refs, or plan state,
  and apply/reject/publish transitions are crash-recoverable and idempotent.
