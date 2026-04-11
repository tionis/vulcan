---
name: daily-notes
description: Work with daily and periodic notes, including event extraction.
version: 1
tools:
  - daily_today
  - daily_show
  - daily_list
  - daily_append
  - daily_export_ics
require_confirmation: false
---

# Daily Notes

## When to Use This Skill

Use this skill for daily note creation, review, journaling, and event-oriented workflows.

## Recommended Flow

- Use `today`, `daily today`, or `daily show` to locate the canonical note first.
- Prefer `daily append` when adding log lines, follow-ups, or structured event entries.
- Use `daily list` when reviewing several days at once.
- Use `daily export-ics` only when the events need to leave the vault.

## Guardrails

- Do not create a second note for a date that already has a tracked daily note.
- Keep event syntax consistent so later extraction and export remain reliable.
- If the workflow spans weeks or months, switch to the `periodic` command group instead of forcing everything through daily notes.

## Example Moves

- Open today’s note, then append a meeting summary under the right heading.
- Review the last week’s daily notes before preparing a weekly summary.
- Export structured daily-note events to ICS when a calendar handoff is needed.
