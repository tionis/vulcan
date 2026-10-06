---
name: daily-notes
description: Work with daily and periodic notes, including event extraction.
version: 4
tools:
  - daily
  - daily_latest
  - daily_today
  - daily_open
  - daily_show
  - daily_list
  - daily_append
  - daily_export_ics
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Daily Notes

## When to Use This Skill

Use this skill for daily note creation, review, journaling, and event-oriented workflows.

## Recommended Flow

- For “latest daily note,” use MCP `daily` with `operation: latest` or CLI `daily latest`. This means the newest existing configured daily note, not today.
- For a read of today, use MCP `daily` with `operation: today`; `exists: false` means today is absent and never falls back to latest.
- Use CLI `today`/`daily today` only when the intent is to open or create today’s note. Use `daily show` for a read-only known-date lookup.
- For any other day, use `daily open <date> --no-edit` (or `--dry-run` to learn the resolved path without creating it). New notes render the configured daily template with `{{date}}` set to that day.
- CLI date arguments (`open`, `show`, `append --date`, `list --from/--to`) accept `YYYY-MM-DD`, `today`, `yesterday`, `tomorrow`, signed offsets such as `-1`, `+3`, `-2w`, `-1m`, and `last <weekday>` / `next <weekday>`, resolved against the local calendar day. Prefer an explicit `YYYY-MM-DD` when the user named a specific date.
- Prefer `daily append` when adding log lines, follow-ups, or structured event entries.
- Use `daily list` when reviewing several days at once.
- MCP `daily` list/range defaults to 20 newest-first items, omits full event objects, and returns `page.next_offset`; set `include_events: true` only when event details are needed.
- Use `daily export-ics` only when the events need to leave the vault.

## Guardrails

- Do not create a second note for a date that already has a tracked daily note.
- `daily calendar` and bare `daily` are an interactive picker for humans. Without a terminal they only list one month's notes; use `daily open`, `daily show`, or `daily list` instead.
- Do not use search or a vault-wide query to locate daily notes; the daily API uses the configured folder and filename/date semantics directly.
- Under scoped permissions, “latest” means the newest daily note the caller may read.
- If a daily or periodic read reports a pending ordinary-write journal, stop reading that vault and inspect it with `vulcan repair ordinary-write status`. Direct CLI and MCP daily list, show, and latest reads refuse a partially published multi-note change until recovery or explicit reconciliation.
- Keep event syntax consistent so later extraction and export remain reliable.
- If the workflow spans weeks or months, switch to the `periodic` command group instead of forcing everything through daily notes.

## Example Moves

- Open today’s note, then append a meeting summary under the right heading.
- Review the last week’s daily notes before preparing a weekly summary.
- Export structured daily-note events to ICS when a calendar handoff is needed.
