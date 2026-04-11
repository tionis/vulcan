---
name: web-research
description: Search the web and fetch article content into a vault-oriented workflow.
version: 1
tools:
  - web_search
  - web_fetch
  - note_create
  - note_append
require_confirmation: false
---

# Web Research

## When to Use This Skill

Use this skill when external web material needs to be summarized or incorporated into notes.

## Recommended Flow

- Start with `web search` to narrow the candidate set.
- Fetch only the pages that matter instead of crawling broadly.
- Use article extraction when the page is readable prose; fall back to raw or generic fetch modes for technical content.
- Save the synthesis into a note instead of leaving the result ephemeral.

## Guardrails

- Do not fetch many pages before narrowing the search space.
- Web results are outside the vault’s source-of-truth model. Summaries should capture provenance clearly.
- If the task is mostly vault lookup with one external citation, keep the web step minimal.

## Example Moves

- Search for release notes, fetch the most relevant article, then append a short summary to a project note.
- Use `web fetch` in markdown mode for an article and raw mode for a machine-readable payload.
- Combine one web result with existing vault notes rather than replacing the vault workflow with browsing.
