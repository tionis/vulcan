---
name: task-management
description: Query task state across notes and periodic workflows.
version: 1
tools:
  - tasks_query
  - query
  - daily_list
  - help
metadata:
  vulcan:
    managed: true
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
- In an mdbase collection, task create/update/convert/archive workflows preflight proposed records and commit through the validated journal. Treat a validation failure as a schema or collection-rule conflict; do not bypass it with a direct Markdown edit.
- If a task line is converted from an ordinary note into a managed TaskNotes record, the source rewrite and new task are one journaled change; inspect both paths if the transaction reports a conflict.
- Ordinary inline task create, complete, and reschedule writes are serialized with other Vulcan vault writes and reject a note changed after planning. On a changed-note error, reread the task and its source note before deciding whether to retry; never overwrite the newer edit.
- Converting an existing ordinary note into a TaskNotes task also rejects a source note changed after planning. Reread the note and rerun conversion only if it is still appropriate.
- Ordinary TaskNotes add refuses a new task path that another writer created after planning. Inspect that task before choosing a new title or retrying.
- Pomodoro state written to an ordinary or daily note rejects a note changed after planning, and creation of a missing daily note refuses a new path collision. Reread the note and session state before retrying.

## Example Moves

- Query open high-priority tasks, then inspect blocked items before editing.
- Append a new inline task to a note instead of rewriting the whole checklist.
- Review upcoming recurring tasks before planning the next daily note.
