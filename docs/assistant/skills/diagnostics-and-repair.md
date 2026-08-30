---
name: diagnostics-and-repair
description: Diagnose vault health, broken links, parser diagnostics, suspicious state, synchronization pauses or conflicts, and repairable problems. Use when the user asks why something is broken, wants a health check, sees diagnostics, or needs safe repair steps before editing notes.
version: 15
tools:
  - doctor
  - cache_verify
  - repair
  - search
  - graph
  - help
metadata:
  vulcan:
    managed: true
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
5. For Git-backed device sync, run `vulcan sync doctor [<wiki>]` before mutation, then `vulcan sync status` for the proposed finite cycle. Doctor distinguishes unavailable Git, unsupported layout, unreadable or divergent refs, offline remotes, active locks, retained journals, missing ignore rules, filter/LFS requirements, cache drift, and target-platform incompatibilities. A registered wiki uses its recorded platform profile even when doctor runs on another host. Case-fold, canonical-Unicode, and Windows-reserved-name errors mean the tree cannot be represented safely; executable-bit, link-file symlink, and long-path warnings require target-device review. A paused status identifies staged or in-progress Git state; a conflicted result preserves both candidate commits and local bytes for review.
6. Inspect `state.recovered_from` and `state.retained` in JSON sync output. The retained phase and captured object IDs distinguish an offline/cancelled cycle from an uncaptured failure. Recovery journals are authoritative device-local operational state outside `.vulcan/cache.db`; do not remove them as a cache repair.
7. `vulcan sync doctor` reports whether a stable device identity already exists but never creates one. Missing identity before the first mutating sync is informational; malformed or unsupported identity state is an error requiring preservation and review.
8. Treat `state.apply-marker` as an interrupted worktree application, not cache damage. Preserve the private-Git-directory marker and device-local journal, avoid manual ref cleanup, and rerun sync so current bytes are recaptured and the accepted revision is verified before the marker is cleared.
9. A mutating sync applies the same target-platform preflight as doctor. If a local tree is incompatible, its bytes and object ID are already captured and the journal remains at `captured`; no remote query occurred. If a fetched or merged tree is incompatible, Vulcan leaves the worktree and remote live ref unchanged. Fix or rename the reported paths on a platform that can represent them, then rerun sync rather than bypassing the profile.

## Guardrails

- Do not "fix" diagnostics by deleting content unless the user explicitly wants deletion.
- Parser unsupported-syntax diagnostics are not always data loss; preserve source where possible.
- Cache/index repair should not edit notes.
- For bulk repairs, inspect changed paths and commit separately from unrelated edits.
- Do not clear staged state, rewrite Vulcan-owned refs, or pick a conflict side merely to make synchronization continue.
- Do not remove `vulcan-sync/apply.json` as a repair shortcut. It is durable evidence that mutation began and verification may not have completed.
- For a sync conflict, retain the immutable conflict ID and inspect its base/local/remote revisions and path records. The original commits remain Git-reachable and file artifacts live in device-local sync state, so cache repair and note cleanup must never delete them.
- Start conflict investigation with `vulcan sync conflicts`, then use `vulcan sync conflicts <id>` to inspect per-side object IDs, modes, hashes, byte counts, and artifact locations. This read-only command is safe before deciding how to resolve the conflict.
- If the user explicitly selects `base`, `local`, or `remote`, run `vulcan sync resolve <id> --side <side> --dry-run` first. A stale worktree, changed preserved ref, active Git operation, or moved remote is a safety stop—not a reason to reset files or refs. The mutating form is appropriate only after reviewing the lossy path-level choice.
- If the user instead supplies reviewed replacement files, require one `--file '<conflict-path>=<source-file>'` for every conflicted path and run with `--dry-run` first. Missing, duplicate, unrelated, oversized, malformed, or ineligible files must remain unresolved; do not work around those diagnostics by choosing an arbitrary preserved side.
- For a reviewed unified patch, run `vulcan sync resolve <id> --patch <patch-file> --dry-run` first. Patch diagnostics are safety stops when it does not apply to the preserved local candidate, covers only part of the conflict, touches unrelated paths, deletes a conflict file, or uses unsupported rename/copy records.
- For interactive resolution, run `vulcan sync resolve <id> --editor --dry-run` before launching the mutating form. The editor works on private temporary marker files, not vault files; an unchanged file or remaining `VULCAN-CONFLICT-<id>` token is an incomplete resolution and must fail without publishing.

## Example Moves

- Explain why a wikilink is unresolved and propose the safest rename/move/link fix.
- Distinguish malformed frontmatter from a cache migration issue.
- Run doctor, apply a targeted repair, then re-run diagnostics to verify.
