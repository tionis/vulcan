---
name: diagnostics-and-repair
description: Diagnose vault health, broken links, parser diagnostics, suspicious state, and repairable problems. Use when the user asks why something is broken, wants a health check, sees diagnostics, or needs safe repair steps before editing notes.
version: 1
tools:
  - doctor
  - cache_verify
  - repair
  - search
  - graph
  - help
require_confirmation: false
---

# Diagnostics and Repair

## When to Use This Skill

Use this skill for investigation before mutation: broken links, malformed frontmatter, stale cache,
diagnostics, orphaned assets, search mismatches, and unexpected graph/query results.

## Recommended Flow

1. Run a read-only diagnostic command first: `doctor`, `cache verify`, `search --explain`, or graph diagnostics.
2. Classify the problem as source-note content, derived cache/index state, config/permission state, or unsupported syntax.
3. Use dry-run repair/fix modes when available.
4. Only patch source notes after identifying the smallest concrete fix.

## Guardrails

- Do not "fix" diagnostics by deleting content unless the user explicitly wants deletion.
- Parser unsupported-syntax diagnostics are not always data loss; preserve source where possible.
- Cache/index repair should not edit notes.
- For bulk repairs, inspect changed paths and commit separately from unrelated edits.

## Example Moves

- Explain why a wikilink is unresolved and propose the safest rename/move/link fix.
- Distinguish malformed frontmatter from a cache migration issue.
- Run doctor, apply a targeted repair, then re-run diagnostics to verify.
