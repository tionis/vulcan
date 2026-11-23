# Audit: sync, device identity, and MCP-only agent work (2026-11-23)

Scope: the overall approach to device and vault identity, the file-tree sync design and its
recent additions (guarded rebase, agent replay, activity signal), and whether an agent can do all
of its vault work through MCP while Vulcan commits, syncs, and indexes in the background.
Judged against the real setup: normandy (desktop) and enterprise (always-on daemon, MCP remotes,
Telegram bot); Grimoire on Git sync in committed-history-only mode with `guarded-rebase`; Mimir
and Stories replicated by Syncthing. Phase 12 extensions are on hold for geth.

Three independent read-only reviews produced the findings; the load-bearing claims were then
checked in code or on the machines (marked **verified**).

## Verdict

- **Sync** has the right core for agent repositories (branch lane, leased push, `branch_pull`
  policies, guarded rebase) but carries a large whole-worktree machine (live ref, merge policy,
  conflict records, proposals, checkpoints, retention, semantic history, path scope: roughly
  15k of ~30k production lines) that no current wiki needs once Syncthing wikis stop using it.
  It is still running, by accident, on normandy's Syncthing wikis.
- **Identity** is cryptographically sound but keyed in the wrong places for one owner: OAuth keys,
  grants, tokens, and scan state hang off a device-local registration ULID that routine
  `vault remove` + `vault add` destroys, and path-hashed state silently diverges when a vault
  moves. That is exactly the 2026-11-23 enterprise outage.
- **MCP-only agents are not viable on Grimoire today.** MCP writes land in the worktree and
  nothing ever commits them. Reads, search, tasks, and daily notes are good; commit, history,
  move/rename, non-Markdown files, scripts, and optimistic concurrency are missing.

## Findings, ranked

### Data-loss shaped (fix first)

1. **MCP writes to Grimoire are never committed** (verified). `capture_owner_in_registry`
   (`vulcan-app/src/commit.rs`) and `sync_owns_capture` (`vulcan-daemon/src/auto_commit_worker.rs`)
   hand capture to sync for every Git registration without looking at `sync.worktree`; in `none`
   mode sync never commits, so neither per-write nor batched auto-commit runs. The MCP write report
   does not mention it (`warnings: []`). Enterprise's Grimoire checkout holds uncommitted
   `wiki/TODO.md` and an untracked `wiki/Architecture/Dotfiles Merge.md` right now. The 11.1
   capture rule predates worktree modes (12.23).
   *Fix:* auto-commit owns capture when the effective worktree mode is `none` (and for unscoped
   paths under `paths`); set Grimoire `[git] auto_commit = true, strategy = "batched"`; report the
   commit outcome (or the policy warning) in every MCP write result.
2. **A dirty, diverged checkout defers silently and never pushes** (reviewer-verified in code).
   `git rebase --no-autostash` refuses any unstaged change → lane `Deferred` every cycle; only
   healthy lanes push; alerts cover Paused/Failed/Conflicted only. With finding 1 leaving files
   dirty on enterprise, Grimoire there can stop advancing with no signal; the guide's "anything
   else pauses with an alert" is wrong for this case.
   *Fix:* escalate a lane deferred for N cycles/T minutes to Paused with an alert; push when
   strictly ahead even if the pull deferred.
3. **Two replicators on the same tree** (verified). On normandy `mimir`, `stories` (and
   `recipes`) are registered `sync_backend = "git"` with the default `worktree = "all"`, while
   `mimir` and `stories` are Syncthing folders. Live snapshots were still published on 2026-11-22.
   *Fix:* `vulcan vault set mimir --no-sync` and `stories` on normandy (and `recipes` if
   Syncthing-managed); later prune their remote `__vulcan-sync/*` refs; warn when Git sync is
   enabled on a directory containing `.stfolder`.

### Correctness in this week's code

4. **Replay leaves replayed bytes in the worktree when its compare-and-swap loses** (verified by
   reading; reviewer reproduced). On `Stale`, `advance_checked_out_branch` already restores the
   index to `expected`, so the follow-up `read-tree -m -u target expected` in
   `move_checked_out_branch` changes nothing on disk; a later `git commit -a` would sweep the
   replayed content in. *Fix:* do the two-way checkout and the CAS in one place, restore exactly
   once, and never restore if the branch moved somewhere else.
5. **Replay holds the vault's mutation permit across LLM calls and never takes
   `RepositoryLock`** (verified: `gate.enter()` before `replay_branch` in
   `vulcan-app/src/branch_replay.rs`). Hosted writes queue for up to 300 s; a poll-triggered sync
   can rebase in the main checkout concurrently. *Fix:* replay and call the agent ungated; take the
   gate and `RepositoryLock` only around the branch move.
6. **Replay hygiene:** hooks and `commit.gpgSign` apply in the scratch worktree
   (`-c core.hooksPath=/dev/null -c commit.gpgSign=false`; say when signatures would be dropped);
   no Drop guard removes the scratch worktree on panic; empty commits are kept on clean picks but
   skipped after resolution (pick one; `--keep-redundant-commits` is deprecated for
   `--empty=keep`); decline paths with a `filter` attribute (LFS).

### Identity and operability

7. **Routine maintenance destroys remote identity** (verified). `vault remove` deletes the
   registration ULID, the vault-backed remote's instance ID (which names the OAuth signing/issuer
   keys and grants), and the accepted IndieAuth identity; `vault add` mints a new ULID and has no
   `--serve-remote`; there is no in-place path move, and the `vault set --id` /
   `mcp remote set --id` renames that `docs/specs/declarative-device-config.md` promises do not
   exist. *Fix:* `vault set --path` and `--id`; make CLI `remove` keep identities as drift (like a
   hand edit) with `--forget` for the destructive case; print what an entry carries (remote bind,
   grants, tokens, schedules) before removing; key OAuth secrets and grants by remote id (the wiki
   id) rather than a ULID.
8. **Path-hashed device state diverges after a move.** The sync journal and the transport binding
   are keyed by a hash of the worktree path; after a move status reports the transport as not
   managed while plain `git` still uses the Vulcan-owned `core.sshCommand`. *Fix:* key by wiki id
   or a marker inside `.git/`; detect an orphaned Vulcan-owned `core.sshCommand`.
9. **Device safety heads in `none` mode cost more than they protect.** Every cycle (triggered by
   any worktree edit, since the watcher ignores the mode) snapshots the dirty worktree and pushes
   it, chaining bridge commits that retention refuses to prune in `none` mode (516 commits off
   HEAD on normandy's Grimoire). Uncommitted text leaves the machine despite "uncommitted edits
   never sync", and every local rewrite produces a bridge (the 81 warnings). *Fix:* in `none` mode
   watch refs only and keep the safety head local or opt-in.
10. **Alerts are imprecise.** The branch-paused text always says `git pull --rebase && git push`,
    which is wrong for a guarded-rebase hazard (it strands the agent worktree's base), merges,
    interactive rebase, and ff-only; the reason is derived from a string prefix. Background-policy
    holds (battery, metered network) alert as "sync paused". The foreign-bridge warning is a stderr
    line with no count or alert. *Fix:* typed reasons end to end; no alert for policy holds;
    count foreign bridges in status.
11. **Diagnosing transport** is confusing because the engine sets an exact `GIT_SSH_COMMAND` while
    plain git uses `core.sshCommand`, and the wrapper silently falls back to plain `ssh` when the
    binary moved. *Fix:* `vulcan sync transport test -v`.

### MCP coverage gaps

| Agent task | Today | Smallest fix |
|---|---|---|
| Read / outline / section get, search, query, tasks, daily | Full | — |
| Create / append / patch / set / delete notes | Full, but `note_set` has no precondition | `content_hash` from `note_get`, `expected_hash` on set/patch/delete |
| Know whether a write was committed | None | commit outcome in every write report |
| Commit, status, diff, log/history | None (profile allows git) | `git` pack gated on `git = allow`, reusing 11.2 REST history |
| Move/rename with link rewrite | None (profile allows refactor) | `note_move` over the existing refactor |
| Non-Markdown files, attachments | None (`note_create` forces `.md`) | bounded `file_get`; attachment upload |
| Grimoire scripts | None (`scripts/` unreachable) | expose selected scripts as custom tools |
| Sync now / resume / resolve a paused branch | Read-only status | `sync_refresh`, `sync_resume`; replay covers conflicts once enabled |
| Retried hosted append | Duplicates text | idempotency key on append |

Docs to fix alongside: Grimoire `AGENTS.md` (MCP-only clause), `docs/guide/mcp-remotes.md`,
`docs/assistant/skills/mcp-setup.md`, `AGENTS.template.md` (who commits MCP writes in each sync
mode); the `git-workflow` skill names an MCP `git_commit` tool that does not exist.

Syncthing-specific: Vulcan does not detect `*.sync-conflict-*` copies, so they index as ordinary
notes; and nothing commits on the Mimir hub unless the device enables batched auto-commit.

## Keep / simplify / freeze / remove

| Area | Decision |
|---|---|
| Branch lane, leased push, lock, `branch_pull` policies, guarded rebase, `rebase_hazard` | Keep; fix findings 2 and 10 |
| Agent replay | Keep; fix 4–6, then freeze |
| Supervisor, job ledger, adaptive polling, activity signal | Keep |
| Safety head and worktree watcher in `none` mode | Simplify (finding 9) |
| Registration ULID, OAuth key naming, path-keyed state | Simplify now (findings 7–8); bug-class, compatible with the freeze |
| Live ref / `all` mode, merge policy, structured merge, conflict records, `sync resolve`, proposals, checkpoints, retention/epochs, semantic history | Freeze hard; once no wiki uses `all`, deprecate and remove (≈15k lines) |
| `paths` mode | Freeze; first removal candidate (no user) |
| Device key transport, forge provisioning, registration refs, `device replace`/`revoke` | Freeze; mark 12.16–12.18 designs superseded by geth |
| Sync spec (103 KB in 121 lines) | Split into short sections; write `none` mode as its own short contract |

## Suggested order

1. Configuration today: Mimir and Stories `--no-sync` on normandy; commit enterprise's stranded
   Grimoire files.
2. Finding 1 (+ commit outcome in MCP reports) and finding 2.
3. Replay fixes 4–6.
4. Identity: `vault set --path/--id`, non-destructive `remove`, OAuth keyed by remote id.
5. `none`-mode safety head and watcher; alert accuracy.
6. MCP `git` pack, `note_move`, hashes, `file_get`; docs and skills.
7. Formal freeze of the live mode and a removal plan tied to geth.
