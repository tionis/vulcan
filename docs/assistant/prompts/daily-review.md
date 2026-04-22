---
name: daily-review
title: Daily Review
description: Review one day of notes and turn them into a concise status update.
version: 1
tags:
  - review
  - daily
arguments:
  - name: date
    title: Date
    description: Daily note date such as 2026-04-22.
    required: true
    completion: daily-date
---
Review the daily note for `{{date}}` and any linked work notes that matter.

Produce:
1. What was completed.
2. What is still in progress.
3. What should happen next.
