---
name: git-workflow
description: Inspect vault changes, review history, and create intentional commits.
version: 1
tools:
  - git_status
  - git_diff
  - git_log
  - git_blame
  - git_commit
require_confirmation: false
---

# Git Workflow

## When to Use This Skill

Use this skill when you need repository state rather than note content.

## Recommended Flow

- Start with `git status` to see whether the change is isolated or mixed with unrelated edits.
- Review `git diff` or `changes` before writing a commit message.
- Use `git log` or `git blame` when provenance matters.
- Commit only after the note or refactor workflow is understood.

## Guardrails

- Do not write a commit message before inspecting what actually changed.
- Treat unrelated dirty worktree state as a coordination issue, not something to silently overwrite.
- Prefer explicit commits over assuming auto-commit covers every workflow.

## Example Moves

- Inspect the diff after a vault-wide refactor before committing.
- Use `git blame` to explain why one task line changed.
- Check note-scoped history before editing a long-lived project note.
