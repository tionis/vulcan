---
name: daily-notes
description: Work with daily and periodic notes, including event extraction.
version: 2
tools:
  - daily
  - daily_latest
  - daily_today
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
- Prefer `daily append` when adding log lines, follow-ups, or structured event entries.
- Use `daily list` when reviewing several days at once.
- Use `daily export-ics` only when the events need to leave the vault.

## Guardrails

- Do not create a second note for a date that already has a tracked daily note.
- Do not use search or a vault-wide query to locate daily notes; the daily API uses the configured folder and filename/date semantics directly.
- Keep event syntax consistent so later extraction and export remain reliable.
- If the workflow spans weeks or months, switch to the `periodic` command group instead of forcing everything through daily notes.

## Example Moves

- Open today’s note, then append a meeting summary under the right heading.
- Review the last week’s daily notes before preparing a weekly summary.
- Export structured daily-note events to ICS when a calendar handoff is needed.
