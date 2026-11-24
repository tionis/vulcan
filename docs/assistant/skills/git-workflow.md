---
name: git-workflow
description: Inspect vault changes, review history, create intentional commits, or synchronize a Git-backed vault through Vulcan's hidden live ref.
version: 66
tools:
  - git_status
  - git_diff
  - git_log
  - git_blame
  - git_commit
  - changeset_begin
  - changeset_switch
  - changeset_list
  - changeset_land
  - changeset_abandon
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Git Workflow

## When to Use This Skill

Use this skill when you need repository state rather than note content.

Use `vulcan sync` when the user wants device/file-tree synchronization. This is separate from `git commit`: live sync snapshots are non-semantic and use Vulcan-owned refs without creating commits on the user's current branch. Sync does pull and push that branch, links its published tip into live, and reports `adopted` when upstream commits had already reached the worktree through live.

Registered directories default to the versioned `knowledge` profile. Use `vulcan vault add <id> <path> --profile files-only`, either clone command with `--profile files-only`, or `vulcan vault set <id> --profile files-only` when a full working tree should be synchronized without Markdown indexing or knowledge-specific validation, scripts, semantic history, and agent conflict resolution. Both profiles currently use full-tree materialization. Files-only devices retain concurrent automatic merges for review to preserve shared accepted bytes. The daemon's unattended preflight rejects detached HEAD, staged changes, linked worktrees, in-progress Git operations, and nested repositories or submodules; branch changes are checked again before applying. Vulcan's lock does not exclude external Git processes. Do not use unattended sync for active development checkouts, and do not describe file replication as a complete refs/history/LFS backup.

A vault can be one directory of a larger Git repository, such as the `docs/` directory of an MkDocs site. A repository-root `.vulcan.toml` (`vault = "docs"`, written by `vulcan init --repository-pointer`) lets Vulcan find that vault from anywhere in the checkout. `vulcan status` then reports `git_repository_root` and `git_vault_prefix`. `git status`, `git diff`, `git log`, `git blame`, `git commit`, and auto-commit stay scoped to the vault and report vault-relative paths; code and other files outside the vault are never staged. `vulcan sync` is file-tree replication of the whole enclosing repository: it captures and applies every work-tree change, while reading sync configuration and taking the write lock from the vault.

## Recommended Flow

- Over MCP, enable the `git` tool pack: `git_status`, `git_diff`, `git_log`, `git_blame`, and `git_commit` mirror the CLI commands below. Commit your own work in a vault whose sync does not capture the worktree (`sync.worktree = "none"`), since nothing else will. `git_commit` commits only the files your session's writes changed (including every file a `note_move` rewrote), so other writers' edits stay out of your commit; its result lists anything of yours still uncommitted as `session_uncommitted`. Use `scope: "all"` only after `git_status` shows every change is meant to go in, or `paths` to name files changed outside MCP writes. The commit records your client in `Vulcan-MCP-*` trailers, and so does a batched auto-commit that takes writes you left uncommitted.

- For a larger or multi-step change that should not be half-visible (a restructure, many linked
  edits), start a changeset with `changeset_begin`. Your session's tools then work in a separate
  worktree outside the vault, on branch `vulcan/changeset/<id>`, which other writers, sync, and
  auto-commit do not see; write results report `uncommitted` there. Commit steps with
  `git_commit` as usual, then `changeset_land` (with `message` to commit leftovers) fast-forwards
  the vault's branch, rebasing first if it moved. A rebase conflict keeps the changeset unchanged
  and names the paths: `changeset_switch {}` back to the vault, look, and either fix the vault
  side or `changeset_abandon` and redo the change. `changeset_list` shows open changesets;
  `changeset_switch {"id": …}` resumes one. Changesets are not trusted vaults, so custom skill
  tools are not callable inside one. If someone lands or abandons your changeset meanwhile, your
  next call fails once with "no longer exists" and the session is back on the vault: check
  `git_log` before repeating the call. Empty changesets idle for a day are removed
  automatically; land or abandon yours rather than leaving them.
- In a shell, `vulcan git changeset list|show|land|abandon|prune` does the same for people:
  review an agent's changeset with `show` (and `git -C <worktree> log -p`), then `land` or
  `abandon` it. `prune --include-work` deletes idle changesets that still hold work.

- Start with `git status` to see whether the change is isolated or mixed with unrelated edits.
- Review `git diff` or `changes` before writing a commit message.
- Use `git log` or `git blame` when provenance matters.
- Commit only after the note or refactor workflow is understood.
- Run `vulcan sync status` before a sync when an in-progress Git operation may be present.
  Staged changes need no special handling: they sync as ordinary worktree bytes while the
  normal index is left untouched.
- Daemon and companion status use explicit states: `dirty` means watcher work is queued; `capture_pending`, `capturing`, `captured_unpushed`, `fetching`, `fetched`, `merging`, `pushing`, and `applying` describe an active or recoverable transaction; `conflicted`, `paused`, `offline`, and `error` require the indicated review or retry. Durable journal/apply evidence takes precedence over stale terminal job state after restart.
- If a finite cycle reports `paused`, inspect `pause.reason`: `operation_in_progress` or
  `head_moved`. Vulcan has already captured current bytes and fetched an existing remote tip,
  but it has not reconciled or applied while that state is unsafe. Resolve the normal Git
  state yourself, then rerun sync; do not delete the retained journal or Vulcan refs. A
  `branch` report with action `paused` concerns only the branch lane (diverged past
  `pull.ff=only`, interactive rebase, or a merge/rebase conflict that sync aborted, leaving the checkout as it was); the file lane proceeds
  independently.
- Run `vulcan sync doctor [<wiki>]` for a read-only installation, layout, hidden-ref/object, remote, lock, recovery-journal, ignore, filter/LFS, cache-coherence, and target-platform check. Use `--profile files-only` for an unregistered files-only path; a registration uses its stored profile. Unattended files-only doctoring also reports whether repository state meets its conservative preflight. A registered wiki uses its recorded platform profile rather than the current host. Inspect `platform_preflight`: case-fold, canonical-Unicode, or Windows-reserved-name errors make the selected tree unsafe to materialize, while executable-bit, link-file symlink, and long-path warnings describe explicit target-filesystem limitations. `healthy: false` means at least one error-level invariant failed.
- Finite sync carries that same recorded profile through direct registered and daemon jobs. Successful JSON reports retain `local_platform_preflight` and `accepted_platform_preflight`. A platform rejection after local capture is recoverable from the local candidate ref and `captured` journal and occurs before remote contact; a rejected fetched or merged tree is neither published nor applied. Do not change the registration profile merely to bypass incompatible path diagnostics.
- Successful sync JSON also retains `requirements.required_filters` with clean, smudge, process, and executable readiness. A declared filter without a complete round-trip driver—or Git LFS without its executable—is a configuration error before capture or remote access. Install/configure the driver; do not remove `.gitattributes` merely to make synchronization proceed.
- Treat `refs.namespace_version` as the parser contract for every reported Vulcan-owned ref. Current writers emit version 3 and name device refs by the key-derived `vdev1_` device ID; older `devices/<profile>/<ULID>` heads are legacy recovery evidence only. An unknown future version on the remote live tip stops canonical reconciliation after the local safety backup is published; upgrade Vulcan before retrying, and retain that backup. Device candidates, fetched state, conflicts, checkpoints, proposals, and recovery refs remain local under `refs/vulcan/**`. Do not push or edit those local refs manually. Generic Git custom refs are transport-capable, but Vulcan deliberately retains the hidden-branch default until the hosting Forgejo deployment passes its separate conformance checklist.
- `vulcan sync devices list` is a per-vault recovery inventory, not fleet or access status. When remote observation is unavailable, rely only on its local recovery refs and labels; `remote_observation.state` and its sanitized `error` explain that remote fields are unknown. A listed remote ref is unauthenticated recovery evidence, and a local recovery ref is not proof that its source device is online.
- To prune one integrated remote backup, fetch and review it first, then use `vulcan sync devices prune-backup <device-id> --dry-run` and rerun without `--dry-run` after review. The legacy `remove` spelling remains an alias. This deletes only that remote safety backup ref; it does not revoke device, Git, or vault access. The durable local recovery ref remains available.
- If doctor reports `state.apply-marker`, a worktree application may have been interrupted. Preserve the marker and transaction journal, avoid editing Vulcan-owned refs, and rerun a finite sync so Vulcan can recapture current bytes and verify the accepted tree. The marker lives in the private Git directory and is cleared only after successful verification.
- Use `vulcan sync conflicts` to list unresolved preserved conflicts for the selected vault, `vulcan sync conflicts <id>` for the immutable full record and current resolution state, or add `--wiki <id>` for a registered wiki. Detail output gives each path a stable `classification` with its conflict class, content kind, matched policy rule, configured action, effective action, and diagnostic code. Artifact paths are device-local evidence, not vault-relative note paths.
- Treat the conflict embedded in JSON `sync run` output as a bounded preview. Check its counts and completeness flags, follow `detail_conflict_id` with `vulcan sync conflicts <id>`, and page large records; do not infer that the first 16 paths or classifications are complete.
- The list response contains at most a 16-path preview per conflict. Treat `paths_complete: false` as an instruction to request bounded detail pages; `path_count` and progress counters remain global.
- `classification.formatting_candidate` is only a conservative queueing hint derived from immutable sides. It does not weaken `require_review`, select a formatter, or authorize either side. If the user explicitly chooses a formatter, use `vulcan sync format-propose <conflict-id> --group <group-id> --formatter <absolute-path> --formatter-version '<exact output>' --dry-run`; provide explicit `--formatter-arg` and `--formatter-config` values instead of relying on ambient discovery. Vulcan normalizes private base/local/remote copies, requires a clean normalized merge, and binds executable/version/arguments/config plus the accepted frontier into a review-only proposal. Omit `--dry-run` only to retain it, then review and approve the exact proposal separately.
- A later conflict supersedes an older session only when it covers every unfinished path in that session. Unrelated pending groups—including groups loaded from legacy records—remain separately actionable; do not discard an older conflict merely because a newer conflict ID exists. Once any unfinished group changed again on the accepted live frontier, a later sync carries the session forward: it re-merges the preserved local side onto the live version, publishes clean results, and moves still-conflicting paths into a replacement session whose `carried_from` names the original. Resolve the replacement, not the superseded original; its preserved evidence stays inspectable.
- For conflict storms, request bounded detail with `vulcan sync conflicts <id> --path-offset <n> --path-limit <1-256>`. Follow `path_page.next_offset` until absent and use the reported totals/progress; `progress.groups` is page-scoped when `groups_complete` is false, while its counters remain global. Never treat one page as the complete conflict. Companion clients must first check `conflict_contract` capabilities and obey the advertised page and group-batch ceilings.
- Inspect JSON sync reports: `state.recovered_from` means Vulcan found an interruption-sensitive device-local transaction and recaptured before continuing; `state.retained` identifies the exact paused, conflicted, cancelled, or failed phase and any captured object IDs available for follow-up.
- When a sync applies an accepted tree, inspect `application.additions`, `updates`, `deletions`, `type_changes`, and the per-path expected/target object metadata. Vulcan aborts with `worktree_changed` if the complete current worktree no longer exactly matches the captured pre-apply revision; do not bypass that check or manually replay only part of the plan.
- Vulcan-created sync commits carry `Vulcan-Sync-*` trailers with the stable device ID, protocol/profile, policy, immutable sources, and `Semantic: false`. Use these trailers for provenance; do not infer an author's semantic intent from live snapshot subjects.
- Use `vulcan sync run --dry-run` to inspect the selected remote and live ref without creating objects, refs, or files; use `vulcan sync run` for one finite direct-mode cycle.
- Agent clients may request the MCP `sync` pack for `sync_status`, mutation-free `sync_plan`, `sync_doctor`, and `sync_conflicts`. The pack deliberately has no resolver or generic Git shell and is visible only with Git plus full-vault read permission; keep human approval and the ordinary CLI/companion transaction for mutation.
- `vulcan sync run --max-retries <n>` bounds rejected compare-and-swap reconciliation attempts. Retries recapture and re-fetch with capped exponential backoff and remain cancellable; do not replace them with unconditional force pushes.
- When a divergent sync reports `automatic_resolutions`, Vulcan first ran ordinary Git merge and then resolved every remaining listed path under the shared deterministic policy. Review each path's `kind`, `rule_id`, `merger` (for structured results: the registered merger's `id` and `version` and the exact `base`, `local`, `remote`, and `output` blob IDs, which `git cat-file -p <id>` shows; the live merge commit repeats it as `Vulcan-Sync-Merger` trailers), and `validation.checks`; the latter records successful path, syntax, schema, Markdown-link-surface, no-file-deletion, exact-tree, whole-tree link-resolution, and mass-deletion checks as applicable. An empty resolution list means no structured fallback was used. A newly unresolved or ambiguous link, an exceeded shared `sync.tree_validation` ceiling, Markdown body overlap, malformed structured content, binary data, delete-modify case, built-in device-state path, or any path requiring review remains a preserved conflict rather than receiving an implicit winner. Obsidian JSON configuration (`.obsidian/**/*.json`, rule `obsidian-json-structured`) merges key by key, and lists of distinct strings such as enabled plugins or frontmatter tags merge as sets; the same setting changed on both sides, and every other `.obsidian` file (snippets, plugin code), still needs review. A delete/modify conflict whose deleting side newly ignores the path in a committed `.gitignore` resolves as untracking (rule `gitignore-untracked`): the path leaves live history and every device keeps its copy. The only role-dependent exception is `.obsidian/workspace*.json` (rule `obsidian-workspace-local`, resolution `prefer_local`): this device's pane layout is kept automatically and reported like any other automatic resolution. Do not propose `prefer_local` for files that hold content or settings.
- Use `vulcan sync run <wiki>`, `--group <name>`, or `--all` for registered selections. Group/all results are independent per-wiki transactions with aggregate counts, never one atomic cross-repository operation.
- Use `vulcan daemon status` to verify the authenticated background service before diagnosing automatic sync. Start it in the foreground with `vulcan daemon start` or explicitly choose `--detach`; use `vulcan daemon stop` for graceful cross-platform shutdown. A stopped or stale daemon does not prevent direct `vulcan sync run`, and runtime PID metadata alone is not proof that the service is live.
- For an explicit companion-client setup, inspect the running endpoint and credential identity with `vulcan daemon companion --output json`. Reveal bearer authority only with `vulcan daemon companion --reveal-token --output json`, transfer it directly into device-local client storage, and keep it out of vault files, synchronized Obsidian plugin settings, shell history, logs, and source control. The command intentionally refuses a stopped daemon or mismatched runtime credential.
- Install the embedded reference plugin with `vulcan daemon companion install [<wiki>] --dry-run` and then rerun without `--dry-run`. This installs only managed assets and first-install non-secret endpoint/wiki preferences; it deliberately does not put the bearer into the vault.
- The reference Obsidian companion is a thin daemon client. Manual sync requests the current editor save and enqueues the ordinary finite transaction; optional after-save triggers are debounced and skip busy, paused, or conflicted state. Its conflict UI always previews one explicit preserved side before exposing a separate apply action. Do not use the plugin and another Git-sync plugin as independent writers for the same worktree, and never treat its UI as permission to bypass daemon validation.
- Use `vulcan sync pause [<wiki>]` and `vulcan sync resume [<wiki>]` to change device-local automatic behavior. Omitting the ID resolves the selected vault's registration; add `--dry-run` to preview the registry mutation.
- For a designated unattended resolver, configure `vulcan daemon config set-agent resolution`, then preview and apply `vulcan daemon config set-conflict-worker --wiki <id> --poll-seconds <n> --dry-run`. Restart and inspect `vulcan daemon conflict-status`. When the vault's `sync.branch_pull` is `guarded-rebase` or `rebase`, the worker replays a branch whose rebase stopped on a conflict, asking the agent per conflicted commit in a scratch worktree, and leaves publication to the next sync; `conflict-status` shows `replayed_branch` or `declined_replay` with the reason. It never touches shared commits, merges, deletions, non-text files, or uncommitted edits, so a declined replay still needs `git pull --rebase` by a person. Configure only one resolver node per vault.
- Use `vulcan vault clone <remote> <path> --dry-run` to validate a general new clone and registration. Prefer `vulcan sync clone <remote> <path> --dry-run` for sync setup: it selects the detached `android-shared` layout and a private Git directory automatically in Termux while retaining native defaults elsewhere.
- On Termux, the supported baseline is one-shot direct execution: run `vulcan sync status <wiki>`, `vulcan sync doctor <wiki>`, and `vulcan sync run <wiki>` from a shell or shortcut. These commands do not require or start the daemon. Keep the Git directory in Termux-private storage and the materialized vault in shared storage; do not move Git locks, indexes, refs, or objects into Android shared storage. For an OS-managed energy-aware safety net, preview `vulcan sync termux-install <wiki> --period-minutes 60 --dry-run`; it requires Termux:API, schedules the same finite command under Android constraints, and is removed with `sync termux-uninstall`.
- If a registered detached Git directory was lost but its materialized vault remains, preview `vulcan vault recover-git <wiki> <remote> --dry-run`, then rerun only after verifying the registered path, missing Git directory, stale-pointer target, redacted remote, and loss warning. Recovery snapshots the untouched vault under the returned `refs/vulcan/recovery/detached-git-loss/<ulid>` ref before fetching and never checks remote content out over it. Any reported local namespace—including sync candidates, epochs, conflicts, checkpoints, proposals, recovery refs, and legacy development roots—that existed only in the lost directory cannot be reconstructed.

## Guardrails

- Do not write a commit message before inspecting what actually changed.
- Treat unrelated dirty worktree state as a coordination issue, not something to silently overwrite.
- Prefer explicit commits over assuming auto-commit covers every workflow.
- With `[git] strategy = "batched"`, individual commands do not commit; the daemon commits accumulated changes on the vault's interval, and only while it runs. Check `vulcan daemon auto-commit-status` before assuming a change is already committed. Without a daemon, `vulcan git batch-commit --dry-run` previews the batch and `vulcan git batch-commit` commits it. A vault registered with the Git sync backend is captured by sync instead: per-write commits stand down with a warning and no batch runs.
- Never stage files just so a batch picks them up: batches leave paths you staged alone, and they wait out merges, rebases, and a detached `HEAD`. Agent-written batch messages need the vault's `batch_messages = "agent"` and this device's `vulcan daemon config set-auto-commit-agent --wiki <id>`; do not grant that consent unless the user wants this vault's changes sent to the configured provider.
- Do not reset or discard staged state to make synchronization proceed. Knowledge-profile and
  explicit manual sync capture staged worktree bytes without changing the normal index. Unattended
  files-only sync pauses when staged changes exist; review or commit them before retrying.
- Treat a `conflicted` sync outcome as preserved work requiring review. Its immutable `conflict.id`, base/local/remote revisions, path list, policy identity, `provenance_revision`, and `preserved_refs` are stable; the `record` ref names a Git-reachable trailer-bearing creation record, while `conflict_record` points to the device-local record outside the vault; the side bytes stay in Git behind the preserved refs. Do not choose a side, run mutating resolution, delete the record, or edit Vulcan-owned refs without explicit user direction. A preservation ref mismatch is evidence of unexpected mutation and must fail closed.
- A conflicted report may include `conflict.projection` and the durable record includes matching metadata. When `published` and `applied` are true, the live ref and worktree contain its exact safe tree: accepted remote bytes remain provisionally at original conflicted paths, clean merged paths continue synchronizing, and no conflict artifacts enter the vault. The immutable record ref is also published remotely. Do not interpret the visible remote bytes as a semantic winner; use `sync conflicts` and reviewed resolution workflows. Structural conflicts may omit a projection and remain fully preserved by refs and the device-local record.
- A projected conflict can also report `automatic_resolutions` for structured sibling paths that were resolved deterministically. Those bytes are already part of the validated projected tree; the immutable conflict record contains only the paths that still require review.
- A directory-rename or file-location conflict may name a Git-synthesized destination absent from both original candidate trees. Vulcan deliberately omits projection and refuses path-side resolution for that topology; do not turn the refusal into an implicit deletion or manipulate preserved refs. Report the structural diagnostic and keep both candidate revisions intact for a dedicated structural resolution workflow.
- A device-local automation ceiling may turn an otherwise deterministic structured resolution into a preserved conflict, but it must never produce a different accepted tree. Do not infer that two devices disagree merely because one requires additional review.
- For a vault nested inside a code or documentation repository, use `vulcan git commit` for vault-only commits and ordinary Git for the rest of the repository. Do not run `vulcan sync` there unless the user wants the entire work tree, including uncommitted code, replicated.
- Sync defaults to remote `origin` and `refs/heads/__vulcan-sync/live`; pass `--remote` or `--live-ref` only when the repository uses a different agreed profile.
- A clone that succeeds before registration fails is deliberately preserved. Report the partial state and register or remove it only with explicit user direction.
- Treat the Android shared-storage policy as a real capability constraint: executable bits are not representable, symlinks become link files, and case-only renames require an intermediate path. Do not silently substitute it for native Linux policy.
- Never replace or reinterpret a mismatched `.git` pointer during detached recovery. Do not claim that preserving materialized files also recovers unpushed hidden refs or objects from a deleted Termux-private Git directory.
- Pausing affects future automatic jobs only. Manual `sync status`, `sync run --dry-run`, and explicit `sync run` remain available and must not silently toggle the saved state.
- Do not delete a reported transaction journal to hide recovery state. It lives outside the vault and rebuildable cache; let a successful sync clear it or use a future explicit repair command.
- Do not delete or edit `vulcan-sync/apply.json` to hide an interrupted application. Its transaction and revision identities let Vulcan distinguish and safely recover a partially applied worktree.
- A remote/network failure after capture is not a lost sync: the local candidate remains reachable and its journal phase identifies where the finite cycle stopped. Do not replace it with a fresh clone as an error-recovery shortcut.

## Example Moves

- Inspect the diff after a vault-wide refactor before committing.
- Use `git blame` to explain why one task line changed.
- Check note-scoped history before editing a long-lived project note.
- Synchronize an unregistered vault directly with `vulcan --vault ./wiki sync run`.
- Synchronize every wiki in a device-local group with `vulcan sync run --group daily`.
- Pause future automatic sync from inside a registered vault with `vulcan sync pause --dry-run`, then apply it without `--dry-run` after review.
- Preview a detached Android layout from Termux with `vulcan sync clone <remote> /storage/emulated/0/Documents/wiki --dry-run`; use `--git-dir` or `--platform` only for an intentional override.
- If Git rejects ownership of a successfully created checkout, `vault clone` and `sync clone` add its canonical worktree path to the user's global `safe.directory` entries and retry. Dry runs do not write trust entries, and ordinary discovery never automatically trusts an existing repository.
- Inspect a managed Termux job with `vulcan sync schedule show <wiki>`. Preview an interval change with `vulcan sync schedule set <wiki> --period-minutes 30 --dry-run`; unspecified network, battery, charging, and reboot settings are preserved. Apply without `--dry-run` after review.
- Recover that registered layout after private Git-data loss with `vulcan vault recover-git <wiki> <remote> --dry-run`, inspect the warning, then apply without `--dry-run`.

## Git authentication

Vulcan authenticates Git over SSH with the installation's device key by default (enrolled when a
vault is cloned or added; see `sync-workflow`). Do not set `GIT_SSH_COMMAND` or edit `core.sshCommand`
by hand for a bound vault, and never copy or print the private key. If Git fails to authenticate,
run `vulcan sync doctor <wiki>` and `vulcan vault enroll <wiki> --dry-run` rather than substituting
other credentials, and read `vulcan help device-keys` for the full flow.
