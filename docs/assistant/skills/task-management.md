---
name: task-management
description: Query task state across notes and periodic workflows.
version: 1
tools:
  - tasks_query
  - query
  - daily_list
  - help
require_confirmation: false
---

# Task Management

## When to Use This Skill

Use this skill when the task depends on extracting, filtering, reviewing, or updating tasks across the vault.

## Recommended Flow

- Use `tasks query` or `tasks list` to inspect existing task state before mutating anything.
- Reach for `tasks append`, `tasks complete`, `tasks upcoming`, or `tasks blocked` when the workflow is task-specific.
- Combine task views with daily note review when date-based workflows matter.
- Use `help` when the Tasks query syntax or recurrence behavior is unclear.

## Guardrails

- Do not assume task mutation exists everywhere the query layer does; inspect the concrete command first.
- Recurring tasks and dependencies need more care than one-off checkbox edits.
- If the task is actually about TaskNotes note files, prefer the TaskNotes-aware commands rather than hand-editing the generated note.

## Example Moves

- Query open high-priority tasks, then inspect blocked items before editing.
- Append a new inline task to a note instead of rewriting the whole checklist.
- Review upcoming recurring tasks before planning the next daily note.
