---
name: git-workflow
description: Inspect vault changes, review history, create intentional commits, or synchronize a Git-backed vault's committed history with `vulcan sync`.
version: 67
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

Use `vulcan sync` when the user wants device synchronization. Sync moves only committed history: it fetches, integrates the checked-out branch with its upstream, and pushes it. Uncommitted edits never travel, so work reaches other devices only once it is committed (auto-commit, `vulcan git commit`, plain `git commit`, or MCP `git_commit`).

Registered directories default to the versioned `knowledge` profile. Use `vulcan vault add <id> <path> --profile files-only`, either clone command with `--profile files-only`, or `vulcan vault set <id> --profile files-only` when a working tree should be synchronized without Markdown indexing or knowledge-specific validation, scripts, and agent services. Vulcan's lock does not exclude external Git processes. Do not use unattended sync for active development checkouts, and do not describe sync as a complete refs/history/LFS backup.

A vault can be one directory of a larger Git repository, such as the `docs/` directory of an MkDocs site. A repository-root `.vulcan.toml` (`vault = "docs"`, written by `vulcan init --repository-pointer`) lets Vulcan find that vault from anywhere in the checkout. `vulcan status` then reports `git_repository_root` and `git_vault_prefix`. `git status`, `git diff`, `git log`, `git blame`, `git commit`, and auto-commit stay scoped to the vault and report vault-relative paths; code and other files outside the vault are never staged. `vulcan sync` pulls and pushes the enclosing repository's checked-out branch, while reading sync configuration and taking the write lock from the vault.

## Recommended Flow

- Over MCP, a profile with Git access lists `git_status`, `git_diff`, `git_log`, `git_blame`, and `git_commit` mirror the CLI commands below. Commit your own work unless auto-commit already did, since sync carries only commits. `git_commit` commits only the files your session's writes changed (including every file a `note_move` rewrote), so other writers' edits stay out of your commit; its result lists anything of yours still uncommitted as `session_uncommitted`. Use `scope: "all"` only after `git_status` shows every change is meant to go in, or `paths` to name files changed outside MCP writes. The commit records your client in `Vulcan-MCP-*` trailers, and so does a batched auto-commit that takes writes you left uncommitted.

- For a larger or multi-step change that should not be half-visible (a restructure, many linked
  edits), start a changeset with `changeset_begin`. Your session's tools then work in a separate
  worktree outside the vault, on branch `vulcan/changeset/<id>`, which other writers, sync, and
  auto-commit do not see; write results report `uncommitted` there. Commit steps with
  `git_commit` as usual, then `changeset_land` (with `message` to commit leftovers) fast-forwards
  the vault's branch, rebasing first if it moved. A rebase conflict keeps the changeset unchanged
  and names the paths: `changeset_switch {}` back to the vault, look, and either fix the vault
  side or `changeset_abandon` and redo the change. `changeset_list` shows open changesets;
  `changeset_switch {"id": …}` resumes one. Inside a changeset the vault's skill tools (and
  their device grants) work as long as you have not edited the skill or its permission profile
  there; they run in the changeset's worktree, so a check tool checks your staged work. Edited
  tools are refused until landed. If someone lands or abandons your changeset meanwhile, your
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
  Sync never stages, resets, or rewrites the normal index; a staged or uncommitted change in a
  file the incoming commits touch defers the pull to the next cycle.
- Daemon and companion status use explicit states: `dirty` means watcher work is queued; `capture_pending`, `capturing`, `captured_unpushed`, `fetching`, `fetched`, `merging`, and `pushing` describe an active or recoverable transaction; `paused`, `offline`, and `error` require the indicated review or retry. Durable journal evidence takes precedence over stale terminal job state after restart.
- If a finite cycle reports `paused`, inspect `pause.reason`: `operation_in_progress`,
  `head_moved`, or `branch_paused`. Vulcan has already snapshotted current bytes but has not
  integrated the branch while that state is unsafe. Resolve the normal Git state yourself, then
  rerun sync; do not delete the retained journal or Vulcan refs. `branch_paused` means the branch
  diverged past fast-forward-only pulls, an interactive rebase was requested, or a merge/rebase
  conflict was aborted, leaving the checkout as it was; `branch.pause` names which (see
  `sync-workflow`).
- Run `vulcan sync doctor [<wiki>]` for a read-only installation, layout, hidden-ref/object, remote, lock, recovery-journal, ignore, filter/LFS, cache-coherence, and target-platform check. Use `--profile files-only` for an unregistered files-only path; a registration uses its stored profile. A registered wiki uses its recorded platform profile rather than the current host. Inspect `platform_preflight`: case-fold, canonical-Unicode, or Windows-reserved-name errors make the tree unsafe to materialize, while executable-bit, link-file symlink, and long-path warnings describe explicit target-filesystem limitations. `healthy: false` means at least one error-level invariant failed.
- Finite sync carries that same recorded profile through direct registered and daemon jobs. Successful JSON reports retain `local_platform_preflight`. Do not change the registration profile merely to bypass incompatible path diagnostics.
- Successful sync JSON also retains `requirements.required_filters` with clean, smudge, process, and executable readiness. A declared filter without a complete round-trip driver—or Git LFS without its executable—is a configuration error before capture or remote access. Install/configure the driver; do not remove `.gitattributes` merely to make synchronization proceed.
- Treat `refs.namespace_version` as the parser contract for every reported Vulcan-owned ref. Current writers emit version 3 and name device refs by the key-derived `vdev1_` device ID; older `devices/<profile>/<ULID>` heads are legacy recovery evidence only. Device snapshots, conflicts, and recovery refs remain local under `refs/vulcan/**`. Do not push or edit those local refs manually.
- `vulcan sync devices list` is a per-vault recovery inventory, not fleet or access status. When remote observation is unavailable, rely only on its local recovery refs and labels; `remote_observation.state` and its sanitized `error` explain that remote fields are unknown. A listed remote ref is unauthenticated recovery evidence, and a local recovery ref is not proof that its source device is online.
- To prune one integrated remote backup, fetch and review it first, then use `vulcan sync devices prune-backup <device-id> --dry-run` and rerun without `--dry-run` after review. The legacy `remove` spelling remains an alias. This deletes only that remote safety backup ref; it does not revoke device, Git, or vault access. The durable local recovery ref remains available.
- If doctor reports `state.apply-marker`, an older version's interrupted transaction left a marker. Preserve it and the transaction journal, avoid editing Vulcan-owned refs, and rerun a finite sync.
- `vulcan sync conflicts` lists conflict records retained from the removed whole-worktree mode, and `vulcan sync conflicts <id>` shows one (add `--wiki <id>` for a registered wiki; page large records with `--path-offset <n> --path-limit <1-256>` until `path_page.next_offset` is absent). Current sync creates no new records, and retained ones can be read and archived but no longer resolved. Recover any wanted bytes from their preserved refs with ordinary Git and commit them.
- Inspect JSON sync reports: `state.recovered_from` means Vulcan found an interruption-sensitive device-local transaction and recaptured before continuing; `state.retained` identifies the exact paused, cancelled, or failed phase and any captured object IDs available for follow-up.
- Use `vulcan sync run --dry-run` to inspect the selected remote and branch without creating objects, refs, or files; use `vulcan sync run` for one finite direct-mode cycle.
- Agent clients may request the MCP `sync` pack for `sync_status`, mutation-free `sync_plan`, `sync_doctor`, `sync_conflicts`, `sync_refresh`, and `sync_resume`. The pack deliberately has no resolver or generic Git shell and is visible only with Git plus full-vault read permission.
- `vulcan sync run --max-retries <n>` bounds rejected compare-and-swap push attempts. Retries re-fetch with capped exponential backoff and remain cancellable; do not replace them with unconditional force pushes.
- Use `vulcan sync run <wiki>`, `--group <name>`, or `--all` for registered selections. Group/all results are independent per-wiki transactions with aggregate counts, never one atomic cross-repository operation.
- Use `vulcan daemon status` to verify the authenticated background service before diagnosing automatic sync. Start it in the foreground with `vulcan daemon start` or explicitly choose `--detach`; use `vulcan daemon stop` for graceful cross-platform shutdown. A stopped or stale daemon does not prevent direct `vulcan sync run`, and runtime PID metadata alone is not proof that the service is live.
- For an explicit companion-client setup, inspect the running endpoint and credential identity with `vulcan daemon companion --output json`. Reveal bearer authority only with `vulcan daemon companion --reveal-token --output json`, transfer it directly into device-local client storage, and keep it out of vault files, synchronized Obsidian plugin settings, shell history, logs, and source control. The command intentionally refuses a stopped daemon or mismatched runtime credential.
- Install the embedded reference plugin with `vulcan daemon companion install [<wiki>] --dry-run` and then rerun without `--dry-run`. This installs only managed assets and first-install non-secret endpoint/wiki preferences; it deliberately does not put the bearer into the vault.
- The reference Obsidian companion is a thin daemon client. Manual sync requests the current editor save and enqueues the ordinary finite transaction; optional after-save triggers are debounced and skip busy or paused state. Do not use the plugin and another Git-sync plugin as independent writers for the same worktree, and never treat its UI as permission to bypass daemon validation.
- Use `vulcan sync pause [<wiki>]` and `vulcan sync resume [<wiki>]` to change device-local automatic behavior. Omitting the ID resolves the selected vault's registration; add `--dry-run` to preview the registry mutation.
- For a designated unattended resolver, configure `vulcan daemon config set-agent resolution`, then preview and apply `vulcan daemon config set-conflict-worker --wiki <id> --poll-seconds <n> --dry-run`. Restart and inspect `vulcan daemon conflict-status`. When the vault's `sync.branch_pull` is `guarded-rebase` or `rebase`, the worker replays a branch whose rebase stopped on a conflict, asking the agent per conflicted commit in a scratch worktree, and leaves publication to the next sync; `conflict-status` shows `replayed_branch` or `declined_replay` with the reason. It never touches shared commits, merges, deletions, non-text files, or uncommitted edits, so a declined replay still needs `git pull --rebase` by a person. Configure only one resolver node per vault.
- Use `vulcan vault clone <remote> <path> --dry-run` to validate a general new clone and registration. Prefer `vulcan sync clone <remote> <path> --dry-run` for sync setup: it selects the detached `android-shared` layout and a private Git directory automatically in Termux while retaining native defaults elsewhere.
- On Termux, the supported baseline is one-shot direct execution: run `vulcan sync status <wiki>`, `vulcan sync doctor <wiki>`, and `vulcan sync run <wiki>` from a shell or shortcut. These commands do not require or start the daemon. Keep the Git directory in Termux-private storage and the materialized vault in shared storage; do not move Git locks, indexes, refs, or objects into Android shared storage. For an OS-managed energy-aware safety net, preview `vulcan sync termux-install <wiki> --period-minutes 60 --dry-run`; it requires Termux:API, schedules the same finite command under Android constraints, and is removed with `sync termux-uninstall`.
- If a registered detached Git directory was lost but its materialized vault remains, preview `vulcan vault recover-git <wiki> <remote> --dry-run`, then rerun only after verifying the registered path, missing Git directory, stale-pointer target, redacted remote, and loss warning. Recovery snapshots the untouched vault under the returned `refs/vulcan/recovery/detached-git-loss/<ulid>` ref before fetching and never checks remote content out over it. Any reported local namespace—including device snapshots, conflicts, recovery refs, and legacy development roots—that existed only in the lost directory cannot be reconstructed, nor can unpushed commits.

## Guardrails

- Do not write a commit message before inspecting what actually changed.
- Treat unrelated dirty worktree state as a coordination issue, not something to silently overwrite.
- Prefer explicit commits over assuming auto-commit covers every workflow.
- With `[git] strategy = "batched"`, individual commands do not commit; the daemon commits accumulated changes on the vault's interval, and only while it runs. Check `vulcan daemon auto-commit-status` before assuming a change is already committed. Without a daemon, `vulcan git batch-commit --dry-run` previews the batch and `vulcan git batch-commit` commits it. Auto-commit runs the same way for vaults registered with Git sync, whose next cycle then publishes the commits.
- Never stage files just so a batch picks them up: batches leave paths you staged alone, and they wait out merges, rebases, and a detached `HEAD`. Agent-written batch messages need the vault's `batch_messages = "agent"` and this device's `vulcan daemon config set-auto-commit-agent --wiki <id>`; do not grant that consent unless the user wants this vault's changes sent to the configured provider.
- Do not reset or discard staged state to make synchronization proceed; commit it or leave it, and let the deferred pull retry.
- For a vault nested inside a code or documentation repository, use `vulcan git commit` for vault-only commits and ordinary Git for the rest of the repository. Do not run `vulcan sync` there unless the user wants that repository's branch pulled and pushed automatically.
- Sync defaults to remote `origin`; pass `--remote` only when the repository uses a different agreed remote.
- A clone that succeeds before registration fails is deliberately preserved. Report the partial state and register or remove it only with explicit user direction.
- Treat the Android shared-storage policy as a real capability constraint: executable bits are not representable, symlinks become link files, and case-only renames require an intermediate path. Do not silently substitute it for native Linux policy.
- Never replace or reinterpret a mismatched `.git` pointer during detached recovery. Do not claim that preserving materialized files also recovers unpushed commits, hidden refs, or objects from a deleted Termux-private Git directory.
- Pausing affects future automatic jobs only. Manual `sync status`, `sync run --dry-run`, and explicit `sync run` remain available and must not silently toggle the saved state.
- Do not delete a reported transaction journal to hide recovery state. It lives outside the vault and rebuildable cache; let a successful sync clear it or use a future explicit repair command.
- A remote/network failure is not a lost sync: local commits and the device snapshot remain, and the journal phase identifies where the finite cycle stopped. Do not replace the checkout with a fresh clone as an error-recovery shortcut.

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
