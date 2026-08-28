---
name: git-workflow
description: Inspect vault changes, review history, create intentional commits, or synchronize a Git-backed vault through Vulcan's hidden live ref.
version: 13
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
- Run `vulcan sync doctor [<wiki>]` for a read-only installation, layout, hidden-ref/object, remote, lock, recovery-journal, ignore, filter/LFS, and cache-coherence check. Warnings describe reviewable or offline state; `healthy: false` means at least one error-level invariant failed.
- Use `vulcan sync conflicts` to list unresolved preserved conflicts for the selected vault, `vulcan sync conflicts <id>` for the immutable full record and current resolution state, or add `--wiki <id>` for a registered wiki. Artifact paths in detail output are device-local evidence, not vault-relative note paths.
- Only after the user explicitly chooses a preserved side, preview it with `vulcan sync resolve <id> --side base|local|remote --dry-run`, then rerun without `--dry-run` on approval. The choice applies only to conflicted paths while retaining clean merge results; Vulcan captures current bytes first, rejects stale inputs, publishes with compare-and-swap, and retains the original conflict refs and record.
- Inspect JSON sync reports: `state.recovered_from` means Vulcan found an interruption-sensitive device-local transaction and recaptured before continuing; `state.retained` identifies the exact paused, conflicted, cancelled, or failed phase and any captured object IDs available for follow-up.
- Use `vulcan sync run --dry-run` to inspect the selected remote and live ref without creating objects, refs, or files; use `vulcan sync run` for one finite direct-mode cycle.
- `vulcan sync run --max-retries <n>` bounds rejected compare-and-swap reconciliation attempts. Retries recapture and re-fetch with capped exponential backoff and remain cancellable; do not replace them with unconditional force pushes.
- Use `vulcan sync run <wiki>`, `--group <name>`, or `--all` for registered selections. Group/all results are independent per-wiki transactions with aggregate counts, never one atomic cross-repository operation.
- Use `vulcan sync pause [<wiki>]` and `vulcan sync resume [<wiki>]` to change device-local automatic behavior. Omitting the ID resolves the selected vault's registration; add `--dry-run` to preview the registry mutation.
- Use `vulcan sync checkpoint [<wiki>] --dry-run` before deliberately retaining the accepted live commit; add `--kind semantic` when the retention intent is human-facing semantic history rather than recovery. Checkpoints create unique local refs without copying objects or advancing the checked-out branch, and refuse when local accepted refs disagree with the remote.
- Use `vulcan vault clone <remote> <path> --dry-run` to validate a new clone and registration. For Android shared storage accessed from Termux, add both `--git-dir <private-path>` and `--platform android-shared`; native policy remains the default elsewhere.

## Guardrails

- Do not write a commit message before inspecting what actually changed.
- Treat unrelated dirty worktree state as a coordination issue, not something to silently overwrite.
- Prefer explicit commits over assuming auto-commit covers every workflow.
- Do not reset or discard staged state to make synchronization proceed. Vulcan pauses worktree sync until that state is resolved by the user.
- Treat a `conflicted` sync outcome as preserved work requiring review. Its immutable `conflict.id`, base/local/remote revisions, path list, policy identity, and `preserved_refs` are stable; `conflict_record` points to device-local byte-preserving artifacts outside the vault. Do not choose a side, run mutating resolution, delete the record, or edit Vulcan-owned refs without explicit user direction.
- Sync defaults to remote `origin` and `refs/heads/__vulcan-sync/live`; pass `--remote` or `--live-ref` only when the repository uses a different agreed profile.
- A clone that succeeds before registration fails is deliberately preserved. Report the partial state and register or remove it only with explicit user direction.
- Treat the Android shared-storage policy as a real capability constraint: executable bits are not representable, symlinks become link files, and case-only renames require an intermediate path. Do not silently substitute it for native Linux policy.
- Pausing affects future automatic jobs only. Manual `sync status`, `sync run --dry-run`, and explicit `sync run` remain available and must not silently toggle the saved state.
- Do not delete a reported transaction journal to hide recovery state. It lives outside the vault and rebuildable cache; let a successful sync clear it or use a future explicit repair command.
- A remote/network failure after capture is not a lost sync: the local candidate remains reachable and its journal phase identifies where the finite cycle stopped. Do not replace it with a fresh clone as an error-recovery shortcut.

## Example Moves

- Inspect the diff after a vault-wide refactor before committing.
- Use `git blame` to explain why one task line changed.
- Check note-scoped history before editing a long-lived project note.
- Synchronize an unregistered vault directly with `vulcan --vault ./wiki sync run`.
- Synchronize every wiki in a device-local group with `vulcan sync run --group daily`.
- Pause future automatic sync from inside a registered vault with `vulcan sync pause --dry-run`, then apply it without `--dry-run` after review.
- Preview a detached Android-style layout with `vulcan vault clone <remote> /storage/emulated/0/Documents/wiki --git-dir ~/.local/share/vulcan/git/wiki --platform android-shared --dry-run`.
