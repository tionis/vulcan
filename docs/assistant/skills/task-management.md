---
name: task-management
description: Query task state across notes and periodic workflows.
version: 9
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
- Use `tasks show <path-or-name>` for one TaskNotes note's current details and body; an exact path disambiguates multiple readable notes with the same filename or alias.
- Use `tasks eval <file> [--block <zero-based-index>]` for indexed Tasks query blocks in a note.
- Reach for `tasks create`, `tasks complete`, `tasks next`, or `tasks blocked` when the workflow is task-specific.
- Combine task views with daily note review when date-based workflows matter.
- Use `help` when the Tasks query syntax or recurrence behavior is unclear.

## Guardrails

- Keep the selected `--permissions` profile on task mutations, including previews. Routing requires read access to `mdbase.yaml` even when that file is absent or the target is ordinary Markdown. A denied control read is not permission to retry with an unrestricted profile.
- Use `tasks query` or `tasks list` with the selected read scope: path/tag grants and policy decisions precede limits and grouping, and list expression filters see only readable backlink sources. CLI and MCP query/list use the same boundary. Counts describe the readable scope, not the whole vault; do not infer equivalent guarantees for other task-report commands.
- CLI `tasks show` resolves filenames and aliases only among readable indexed notes, then securely reads that source without requiring write permission. Hidden notes do not appear in ambiguity diagnostics. Direct paths can work without an index under static path-only grants; tag/policy-scoped resolution needs indexed candidates. Details come from current source, and changed source tags can invalidate cached read access. A not-found result is not proof that no hidden task exists; do not retry with a broader profile.
- CLI `tasks eval` authorizes the indexed source note before loading its query blocks and applies the same read scope before result limits and grouping. All blocks reuse one authorized note index and one decision per readable candidate for that operation; later reads evaluate policy again. A source/policy/index failure fails the operation, unlike a readable block's syntax error reported in that block's `error` field. Keep source grants and result visibility distinct; source access does not grant access to every task it queries.
- CLI `tasks next`, `tasks blocked`, and `tasks graph` apply the selected path/tag and policy scope before recurrence limits or dependency resolution. Each report derives from one authorized task set and rechecks authority before returning. Authored blocker IDs remain visible, but hidden targets stay unresolved without exposing their paths, text, or completion state. Treat unresolved blockers and empty reports as scoped observations, not proof that targets are absent or the whole vault is unblocked; do not broaden permissions to resolve them.
- CLI `tasks due`, `tasks reminders`, `tasks track status`, and `tasks track summary` derive rows and totals only from the selected path/tag and policy read scope. `tasks track log <task>` uses the same authorized current-source resolution as `tasks show`, without requiring write access. Empty reminders, no active sessions, or zero tracked time describe only that scope, not the whole vault. These guarantees do not extend to pomodoro status.
- CLI `tasks view list` shows only views from readable `.base` files, and `tasks view show` evaluates rows, backlinks, and linked notes within the same scope. A missing view under a restricted profile may be a hidden view file; do not retry with a broader profile.
- An explicit policy-hook denial hides that note from query/list; a broken, unavailable, or invalid hook is an error, not an empty successful result. Reads use one captured hook-source revision and reject changed grants, hook source, or trust before returning. On an authority-change error, review the current policy and start a fresh read/session with the intended profile; do not broaden permissions or treat the error as proof that no tasks exist.
- A profile with `write = "none"` skips automatic task archiving. Read-only query/list access does not need MDB-control grants for that upkeep, and due completed tasks remain in place. This does not disable separate pomodoro transitions or authorize any mutation.
- Tracking, pomodoro status, and other task reports can trigger configured automatic transitions. Those writes need the caller's authority too; daily-note pomodoro storage and archive destinations require their own path grants. Do not broaden grants merely to make a report succeed.
- Task-add and missing daily-note pomodoro templates are selected from readable candidates and rendered with the caller's authority. A dry run cannot create template side notes; use a non-mutating template for a preview that depends on rendered content. Template denial must not be treated as an empty template or retried with broader grants. Only unrestricted reads without policy hooks retain the periodic-note warning-and-empty-template fallback for a genuinely absent template; corrupt readable templates still fail.
- `tasks edit` requires read/write authority for the task and execute authority before launching the external editor. This is a direct filesystem edit followed by a rescan, not a validated MDB transaction or a sandbox for the editor process. Use structured task commands for validated changes; do not use the editor to bypass a validation denial.
- Do not assume task mutation exists everywhere the query layer does; inspect the concrete command first.
- Recurring tasks and dependencies need more care than one-off checkbox edits.
- Hand-written inline tasks should use the Tasks plugin markers Vulcan indexes: `📅` due (`📆`/`🗓` are also read), `⏳` scheduled, `🛫` start, `➕` created, `✅` done, `❌` cancelled, `🏁` on-completion action, and priorities `🔺` highest, `⏫` high, `🔼` medium, `🔽` low, `⏬` lowest.
- If the task is actually about TaskNotes note files, prefer the TaskNotes-aware commands rather than hand-editing the generated note.
- In an mdbase collection, task create/update/convert/archive workflows preflight proposed records and commit through the validated journal. Treat a validation failure as a schema or collection-rule conflict; do not bypass it with a direct Markdown edit.
- If a task line is converted from an ordinary note into a managed TaskNotes record, the source rewrite and new task are one journaled change; inspect both paths if the transaction reports a conflict.
- For a plain-Markdown line-to-task conversion, a changed source or newly occupied target fails instead of overwriting it. Its two-file create/rewrite is journaled. Guarded task commands refuse a pending journal rather than borrowing the previous writer's recovery authority. Inspect both files and preserve the journal for an authorized repair workflow; do not retry or delete it blindly.
- Ordinary TaskNotes archive moves reject a changed source or occupied archive path. A crash can temporarily leave both files present. Inspect both paths and use `vulcan repair ordinary-write status` to review the journal; after manual reconciliation, `accept-current` requires its exact transaction ID and review token. Never delete either file or the journal blindly.
- Direct CLI and MCP task show, query, list, view, due, reminder, dependency, and time-tracking reports refuse a pending ordinary-write journal. Inspect it with `vulcan repair ordinary-write status` before using those reports; a task conversion or archive move may otherwise be only partly published.
- Ordinary task edits that use the shared note-write boundary also refuse a pending journal, including writes to unrelated notes. Repair the interrupted batch before retrying those edits.
- Ordinary inline task create, complete, and reschedule writes are serialized with other Vulcan vault writes and reject a note changed after planning. On a changed-note error, reread the task and its source note before deciding whether to retry; never overwrite the newer edit.
- Converting an existing ordinary note into a TaskNotes task also rejects a source note changed after planning. Reread the note and rerun conversion only if it is still appropriate.
- Ordinary TaskNotes add refuses a new task path that another writer created after planning. Inspect that task before choosing a new title or retrying.
- Pomodoro state written to an ordinary or daily note rejects a note changed after planning, and creation of a missing daily note refuses a new path collision. Reread the note and session state before retrying.

## Example Moves

- Query open high-priority tasks, then inspect blocked items before editing.
- Append a new inline task to a note instead of rewriting the whole checklist.
- Review upcoming recurring tasks before planning the next daily note.
