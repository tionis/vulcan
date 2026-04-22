---
name: summarize-note
title: Summarize Note
description: Summarize one vault note into key points and next actions.
version: 1
tags:
  - notes
  - summary
arguments:
  - name: note
    title: Note
    description: Vault note path or title to summarize.
    required: true
    completion: note
---
Read `{{note}}` with Vulcan before responding.

Return:
1. A one-paragraph summary.
2. A flat list of follow-up actions.
3. Any open questions or ambiguities that still need review.
