---
name: git-workflow
description: Inspect vault changes, review history, create intentional commits, or synchronize a Git-backed vault through Vulcan's hidden live ref.
version: 26
tools:
  - git_status
  - git_diff
  - git_log
  - git_blame
  - git_commit
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Git Workflow

## When to Use This Skill

Use this skill when you need repository state rather than note content.

Use `vulcan sync` when the user wants device/file-tree synchronization. This is separate from `git commit`: live sync snapshots are non-semantic and use Vulcan-owned refs without advancing the user's current branch.

## Recommended Flow

- Start with `git status` to see whether the change is isolated or mixed with unrelated edits.
- Review `git diff` or `changes` before writing a commit message.
- Use `git log` or `git blame` when provenance matters.
- Commit only after the note or refactor workflow is understood.
- Run `vulcan sync status` before a sync when staged changes or an in-progress Git operation may be present.
- Daemon and companion status use explicit states: `dirty` means watcher work is queued; `capture_pending`, `capturing`, `captured_unpushed`, `fetching`, `fetched`, `merging`, `pushing`, and `applying` describe an active or recoverable transaction; `conflicted`, `paused`, `offline`, and `error` require the indicated review or retry. Durable journal/apply evidence takes precedence over stale terminal job state after restart.
- If a finite cycle reports `paused`, inspect `pause.reason`: `staged_changes`, `operation_in_progress`, or `head_moved`. Vulcan has already captured current bytes and fetched an existing remote tip, but it has not reconciled or applied while that state is unsafe. Resolve the normal Git state yourself, then rerun sync; do not delete the retained journal or Vulcan refs.
- Run `vulcan sync doctor [<wiki>]` for a read-only installation, layout, hidden-ref/object, remote, lock, recovery-journal, ignore, filter/LFS, and cache-coherence check. Warnings describe reviewable or offline state; `healthy: false` means at least one error-level invariant failed.
- If doctor reports `state.apply-marker`, a worktree application may have been interrupted. Preserve the marker and transaction journal, avoid editing Vulcan-owned refs, and rerun a finite sync so Vulcan can recapture current bytes and verify the accepted tree. The marker lives in the private Git directory and is cleared only after successful verification.
- Use `vulcan sync conflicts` to list unresolved preserved conflicts for the selected vault, `vulcan sync conflicts <id>` for the immutable full record and current resolution state, or add `--wiki <id>` for a registered wiki. Detail output gives each path a stable `classification` with its conflict class, content kind, matched policy rule, configured action, effective action, and diagnostic code. Artifact paths are device-local evidence, not vault-relative note paths.
- Use `vulcan sync propose <conflict-id> --model <model> [--base-url <openai-compatible-base>]` only after the user requests model-assisted conflict resolution. Add `--api-key-env <name>` to read credentials from the environment, never from vault files or command arguments, and repeat `--context <vault-relative-path>` only for specifically relevant paths. Proposal generation sends bounded exact base/local/remote text, requires Git/read/network grants, retains an unreferenced review tree and JSON record, and does not update live refs or the worktree.
- Only after the user explicitly chooses a preserved side, preview it with `vulcan sync resolve <id> --side base|local|remote --dry-run`, then rerun without `--dry-run` on approval. The choice applies only to conflicted paths while retaining clean merge results; Vulcan captures current bytes first, rejects stale inputs, publishes with compare-and-swap, and retains the original conflict refs and record.
- When a reviewed agent workflow has already retained a proposal ID, preview that exact object with `vulcan sync resolve <conflict-id> --approve-proposal <proposal-id> --dry-run`. Applying it requires the same command without `--dry-run` and explicit user approval. Never substitute a proposal ID, bypass stale-input or parser failures, or treat model output as accepted merely because proposal generation succeeded; approval reconstructs and validates the tree, captures recovery state, uses a remote lease, and writes a content-free audit record.
- Inspect JSON sync reports: `state.recovered_from` means Vulcan found an interruption-sensitive device-local transaction and recaptured before continuing; `state.retained` identifies the exact paused, conflicted, cancelled, or failed phase and any captured object IDs available for follow-up.
- When a sync applies an accepted tree, inspect `application.additions`, `updates`, `deletions`, `type_changes`, and the per-path expected/target object metadata. Vulcan aborts with `worktree_changed` if the complete current worktree no longer exactly matches the captured pre-apply revision; do not bypass that check or manually replay only part of the plan.
- Vulcan-created sync commits carry `Vulcan-Sync-*` trailers with the stable device ID, protocol/profile, policy, immutable sources, and `Semantic: false`. Use these trailers for provenance; do not infer an author's semantic intent from live snapshot subjects.
- Use `vulcan sync run --dry-run` to inspect the selected remote and live ref without creating objects, refs, or files; use `vulcan sync run` for one finite direct-mode cycle.
- `vulcan sync run --max-retries <n>` bounds rejected compare-and-swap reconciliation attempts. Retries recapture and re-fetch with capped exponential backoff and remain cancellable; do not replace them with unconditional force pushes.
- When a divergent sync reports `automatic_resolutions`, Vulcan first ran ordinary Git merge and then resolved every remaining listed path under the shared deterministic policy. Review each path's `kind`, `rule_id`, and `validation.checks`; the latter records successful path, syntax, schema, Markdown-link-surface, no-file-deletion, and exact-tree checks as applicable. An empty resolution list means no structured fallback was used. Markdown body overlaps, malformed structured content, binary data, delete-modify cases, built-in device-state paths, and any path requiring review remain preserved conflicts rather than receiving an implicit winner. A replacement shared policy may opt a narrowly selected Obsidian/plugin JSON path into bounded structured merging; never generalize that opt-in to other `.obsidian` state.
- Use `vulcan sync run <wiki>`, `--group <name>`, or `--all` for registered selections. Group/all results are independent per-wiki transactions with aggregate counts, never one atomic cross-repository operation.
- Use `vulcan sync pause [<wiki>]` and `vulcan sync resume [<wiki>]` to change device-local automatic behavior. Omitting the ID resolves the selected vault's registration; add `--dry-run` to preview the registry mutation.
- Use `vulcan sync checkpoint [<wiki>] --dry-run` before deliberately retaining the accepted live commit; add `--kind semantic` when the retention intent is human-facing semantic history rather than recovery. Checkpoints create unique local refs without copying objects or advancing the checked-out branch, and refuse when local accepted refs disagree with the remote.
- Use `vulcan sync semantic-plan [<wiki>] --from <rev> --to <accepted-live-rev> --dry-run` to review deterministic commit grouping and patches without creating objects or state. Rerun without `--dry-run` to retain the proposal under `refs/vulcan/proposals/semantic/<plan-id>`, then use `vulcan sync semantic-apply <plan-id> --dry-run` before explicit acceptance. Apply refuses stale source, proposal, or live refs and advances only the semantic branch with compare-and-swap; it never rewrites live history. Agent grouping is not yet available and `--agent` fails explicitly.
- Use `vulcan vault clone <remote> <path> --dry-run` to validate a new clone and registration. For Android shared storage accessed from Termux, add both `--git-dir <private-path>` and `--platform android-shared`; native policy remains the default elsewhere.

## Guardrails

- Do not write a commit message before inspecting what actually changed.
- Treat unrelated dirty worktree state as a coordination issue, not something to silently overwrite.
- Prefer explicit commits over assuming auto-commit covers every workflow.
- Do not reset or discard staged state to make synchronization proceed. Vulcan preserves and reports it, captures the worktree, fetches safely, and pauses before reconciliation/application until the user resolves that state.
- Treat a `conflicted` sync outcome as preserved work requiring review. Its immutable `conflict.id`, base/local/remote revisions, path list, policy identity, `provenance_revision`, and `preserved_refs` are stable; the `record` ref names a Git-reachable trailer-bearing creation record, while `conflict_record` points to device-local byte-preserving artifacts outside the vault. Do not choose a side, run mutating resolution, delete the record, or edit Vulcan-owned refs without explicit user direction. A preservation ref mismatch is evidence of unexpected mutation and must fail closed.
- A device-local automation ceiling may turn an otherwise deterministic structured resolution into a preserved conflict, but it must never produce a different accepted tree. Do not infer that two devices disagree merely because one requires additional review.
- Sync defaults to remote `origin` and `refs/heads/__vulcan-sync/live`; pass `--remote` or `--live-ref` only when the repository uses a different agreed profile.
- A clone that succeeds before registration fails is deliberately preserved. Report the partial state and register or remove it only with explicit user direction.
- Treat the Android shared-storage policy as a real capability constraint: executable bits are not representable, symlinks become link files, and case-only renames require an intermediate path. Do not silently substitute it for native Linux policy.
- Pausing affects future automatic jobs only. Manual `sync status`, `sync run --dry-run`, and explicit `sync run` remain available and must not silently toggle the saved state.
- Do not delete a reported transaction journal to hide recovery state. It lives outside the vault and rebuildable cache; let a successful sync clear it or use a future explicit repair command.
- Do not delete or edit `vulcan-sync/apply.json` to hide an interrupted application. Its transaction and revision identities let Vulcan distinguish and safely recover a partially applied worktree.
- Treat semantic plan patches and messages as proposals for human review. Do not edit proposal refs or device-local plan JSON, and do not apply a plan after changing its source branch or accepted live target; create a new plan instead.
- Treat conflict proposal JSON and its unreferenced tree as immutable review state. Do not edit the file, manufacture an approval ID, or approve a proposal for a different conflict; use the exact IDs returned by `vulcan sync propose`.
- Provider output is untrusted even when it is valid JSON. Review the returned explanation, patch, referenced context, path set, model identity, and validation checks before previewing approval. A successful `sync propose` is never authorization to run `--approve-proposal`.
- A remote/network failure after capture is not a lost sync: the local candidate remains reachable and its journal phase identifies where the finite cycle stopped. Do not replace it with a fresh clone as an error-recovery shortcut.

## Example Moves

- Inspect the diff after a vault-wide refactor before committing.
- Use `git blame` to explain why one task line changed.
- Check note-scoped history before editing a long-lived project note.
- Synchronize an unregistered vault directly with `vulcan --vault ./wiki sync run`.
- Synchronize every wiki in a device-local group with `vulcan sync run --group daily`.
- Pause future automatic sync from inside a registered vault with `vulcan sync pause --dry-run`, then apply it without `--dry-run` after review.
- Turn accepted live snapshots into reviewable commits with `vulcan sync semantic-plan --from main --to <accepted-live-rev> --dry-run`, create the plan after review, and explicitly validate/apply its returned plan ID.
- Preview a detached Android-style layout with `vulcan vault clone <remote> /storage/emulated/0/Documents/wiki --git-dir ~/.local/share/vulcan/git/wiki --platform android-shared --dry-run`.
