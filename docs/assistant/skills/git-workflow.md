---
name: git-workflow
description: Inspect vault changes, review history, and create intentional commits.
tools:
  - git_status
  - git_diff
  - git_log
  - git_blame
  - git_commit
require_confirmation: false
---

## When to use

Use this skill when you need repository state rather than note content.

## Core patterns

- Check `git status` before writing a commit message.
- Review `git diff` before `git commit`.
- Use `git blame` when note provenance matters.

## Common mistakes

- Committing without inspecting the staged changes.
