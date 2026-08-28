---
name: git-workflow
description: Inspect vault changes, review history, create intentional commits, or synchronize a Git-backed vault through Vulcan's hidden live ref.
version: 3
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
- Use `vulcan sync run --dry-run` to inspect the selected remote and live ref without creating objects, refs, or files; use `vulcan sync run` for one finite direct-mode cycle.
- Use `vulcan sync run <wiki>`, `--group <name>`, or `--all` for registered selections. Group/all results are independent per-wiki transactions with aggregate counts, never one atomic cross-repository operation.
- Use `vulcan vault clone <remote> <path> --dry-run` to validate a new clone and registration. Add `--git-dir <private-path>` when the materialized worktree is on constrained storage, such as Android shared storage accessed from Termux.

## Guardrails

- Do not write a commit message before inspecting what actually changed.
- Treat unrelated dirty worktree state as a coordination issue, not something to silently overwrite.
- Prefer explicit commits over assuming auto-commit covers every workflow.
- Do not reset or discard staged state to make synchronization proceed. Vulcan pauses worktree sync until that state is resolved by the user.
- Treat a `conflicted` sync outcome as preserved work requiring review. Do not choose a side or edit Vulcan-owned refs without explicit direction.
- Sync defaults to remote `origin` and `refs/heads/__vulcan-sync/live`; pass `--remote` or `--live-ref` only when the repository uses a different agreed profile.
- A clone that succeeds before registration fails is deliberately preserved. Report the partial state and register or remove it only with explicit user direction.

## Example Moves

- Inspect the diff after a vault-wide refactor before committing.
- Use `git blame` to explain why one task line changed.
- Check note-scoped history before editing a long-lived project note.
- Synchronize an unregistered vault directly with `vulcan --vault ./wiki sync run`.
- Synchronize every wiki in a device-local group with `vulcan sync run --group daily`.
- Preview a detached Android-style layout with `vulcan vault clone <remote> /storage/emulated/0/Documents/wiki --git-dir ~/.local/share/vulcan/git/wiki --dry-run`.
