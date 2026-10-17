# Scheduled jobs

A vault can schedule its own recurring jobs, such as a nightly batched commit or a regular full
rescan. The jobs are part of the vault's shared configuration, so they travel with Git and Vulcan
sync. Each job names the devices that run it, and the daemon on each of those devices runs it on
its trigger.

## Define a job

Jobs live in `.vulcan/config.toml`:

```toml
[schedules]
version = 1

[[schedules.jobs]]
id = "nightly-commit"
target = { builtin = "batch-commit" }
nodes = ["vdev1_..."]                    # from `vulcan device show`
trigger = { cron = "30 2 * * *", timezone = "Europe/Berlin" }
authority = "unrestricted"

[[schedules.jobs]]
id = "weekly-rescan"
target = { builtin = "scan" }
input = { full = true }
nodes = ["vdev1_..."]
trigger = { every = "7d" }
authority = "unrestricted"
limits = { max_runtime_seconds = 1800, retries = 1 }
```

- **`id`**: 1 to 64 of `a-z`, `0-9`, `_`, and `-`. Keep it stable; it names the job's history.
- **`target`**: `scan` (incremental, or full with `input = { full = true }`) or `batch-commit`
  (the vault's [batched commit](../cli.md#batched-auto-commits)).
- **`nodes`**: the device IDs that run the job. `vulcan device show` prints this device's ID. A job
  listing several devices runs on each of them independently.
- **`trigger`**: `every` takes a whole number with `m`, `h`, or `d` (1 minute to 31 days), counted
  from the Unix epoch or from `anchor = 2026-01-01T00:00:00Z`. `cron` takes the classic five fields
  (minute, hour, day of month, month, day of week) with lists, ranges, and steps, plus a required
  IANA `timezone`.
- **`authority`**: the permission profile the job runs under.
- **`enabled`** (default `true`) and **`limits`**: `max_runtime_seconds` (default 3600) and
  `retries` (0 to 5, default 0).

Jobs in `.vulcan/config.local.toml` are ignored with a warning: whether a job runs is the vault's
decision, and a single device cannot add one for itself.

## When jobs run

- The daemon runs a job when its trigger comes due, on the devices its `nodes` list. Without a
  running daemon nothing runs on a schedule, but `vulcan schedule run <id>` runs a job once, now.
- A job added or changed starts with its next occurrence. Nothing it "missed" before then runs.
- Occurrences missed while the daemon was stopped, the machine slept, or the clock jumped
  forward run once, as a single catch-up. A clock that jumps back repeats nothing.
- A job never overlaps itself on a device. If it is still running when it comes due again, one
  follow-up runs after it finishes. This includes `vulcan schedule run`: while the daemon runs the
  job, a manual run is refused, and a run that comes due during a manual one waits for it.
- Daylight saving time: a cron time that does not exist (spring forward) runs at the first moment
  after the gap. A time that occurs twice (fall back) runs once, at its first occurrence.
- A failed run is retried after 30 seconds, as many times as `retries` allows. A run interrupted
  by a crash is retried at startup if retries remain, and otherwise recorded as interrupted.
- The daemon rereads every vault's jobs and its own registrations at least every 30 seconds, so an
  edit, a synced change, or `vulcan vault set` takes effect without a restart.
- Disabling, removing, reassigning, or changing a running job asks that run to stop. A run past
  `max_runtime_seconds` is also asked to stop. Both targets finish their current step first. A
  run still waiting for another write to the vault stops at once, without starting. A stopped run
  is recorded as cancelled and is not retried.
- A `config.toml` that does not parse stops every job in the vault, and `vulcan schedule list`
  says why. Once it is fixed, unchanged jobs catch up once, as after downtime.

## Who may run what

Every run, scheduled or manual, is authorized first, under one of two policies a device chooses for
each registered vault.

**Repository trust** (the default) accepts the vault's job definitions as written, but a job's
`authority` profile must stay within this device's ceiling for the vault: the registration's
permission profile (`vulcan vault set <id> --permissions-profile <name>`). A job asking for more
than the ceiling fails. A synced change can therefore narrow what a job may do, never widen it.
The ceiling must be a built-in profile or one defined under `[permissions.profiles.<name>]` in
this device's `.vulcan/config.local.toml`, which is never synced. Profiles in the
synced `.vulcan/config.toml` are ignored for the ceiling, even ones that redefine a built-in name,
because anyone who can push to the vault could edit them.

**Trusted-commit** runs a job only when an allowed signer signed its definition. Use it on a device,
such as a home server, that pulls vault changes from other devices: then a device that can push to
the vault still cannot change what the server runs.

- **Signers** are listed in an OpenSSH allowed-signers file kept outside the vault, owned by you,
  and not writable by anyone else. Use keys only for this purpose: Vulcan refuses device keys, so
  automatic sync commits can never authorize a job. Ed25519 keys work, including keys on a hardware
  token through your SSH agent.
- **Signing** is plain Git with SSH signatures: set `gpg.format = ssh` and `user.signingkey` in the
  vault repository, then `git commit -S` (or `git merge -S`). OpenPGP signatures are not accepted.
- **What a signature covers** is each job separately: its `[[schedules.jobs]]` entry, the permission
  profile it names, and its target's configuration (all of `[git]` for `batch-commit`). Jobs are
  read from the signed commit, not the working tree, so uncommitted edits show as pending and do
  nothing.
- **Unsigned commits** that leave job definitions alone, such as note edits and auto-commits, never
  block anything. An unsigned change to a job blocks just that job until someone signs it, and so
  does an old definition copied back in.
- **Rollback protection:** each device remembers the last signed commit it accepted and moves it
  forward as new signatures arrive. History that no longer contains it, after a force-push or a
  reset, blocks the vault's jobs until you run `vulcan schedule trust accept`.
- Under trusted-commit, `scan` runs the scan alone (no scan-triggered auto-commit or plugins), and
  `batch-commit` uses the signed `[git]` table and runs no commit plugins, because neither may run
  configuration or code nobody signed.

```bash
# On the server, once: an allowed-signers file with your signing key.
echo "admin $(cat ~/.ssh/id_ed25519.pub)" > ~/.config/vulcan/execution-signers
chmod 600 ~/.config/vulcan/execution-signers

vulcan schedule trust enable --allowed-signers ~/.config/vulcan/execution-signers \
  --principal admin --dry-run           # shows the signed commit it would accept
vulcan schedule trust enable --allowed-signers ~/.config/vulcan/execution-signers \
  --principal admin
vulcan schedule trust status            # checkpoint, signed commits, each job's state
vulcan schedule trust accept --dry-run  # after a rollback or restore: what would change
vulcan schedule trust disable           # back to repository trust
```

`trust enable` needs at least one signed commit and records it as the starting point. If that
record is lost (a new state directory, or a backup restored from before), jobs stay blocked until
you run `trust accept`; Vulcan never re-trusts silently. After restoring device state from a backup,
run `trust accept` so the checkpoint is current again. A plain directory without Git cannot use
trusted-commit.

## Setups

### One device

List only this device's ID in `nodes` and keep repository trust. This works on a plain directory
too; only trusted-commit needs Git. Without a daemon, `vulcan schedule run <id>` from your own
timer (cron, a systemd timer, launchd) runs a job once and records it as a manual run.

### A home server that runs jobs for every device

List only the server's ID in `nodes`, so laptops that sync the same vault never run the jobs
themselves. On the server, enable trusted-commit so that a push from another device cannot change
what the server runs, and give the vault a ceiling: define a profile in the server's
`.vulcan/config.local.toml` and select it with `vulcan vault set <id> --permissions-profile <name>`.
Sign job changes from a machine that holds the signing key, push them, and the server picks them up
on its next sync.

### Moving a job to another device

Edit `nodes` and sync. Each device acts on its own copy of the vault: the old device stops the job
when it receives the change, and the new device starts it with its first occurrence after it
receives the change. Nothing missed in between runs, and each device keeps its own history.

A device that is offline keeps running the job as it last saw it, because a synced change cannot
reach it. During a handover both devices may therefore run the job until the old one syncs. When
that overlap matters, make the change while the old device is online and wait until
`vulcan schedule list` on it shows the job no longer runs there. For a device that is gone for
good, stop its daemon or remove its registration with `vulcan vault remove` if it comes back
before it syncs.

### Repair

- `vulcan schedule list` and `vulcan schedule show <id>` say why a job does not run: not this
  device, disabled, a configuration error, its ceiling, or (under trusted-commit) its trust state.
- A run interrupted by a crash needs nothing: at startup it is retried if retries remain and
  otherwise recorded as interrupted.
- Run history, the last run of each job, and the trusted-commit checkpoint live in
  `daemon/schedules.sqlite` under the device state directory. If that file is lost or restored
  from an older backup, history restarts and each job begins with its next occurrence; under
  trusted-commit, jobs stay blocked until you run `vulcan schedule trust accept`.

## Commands

```bash
vulcan schedule list                 # every job: where it runs, authorization, next and last runs
vulcan schedule show nightly-commit
vulcan schedule next nightly-commit --count 5
vulcan schedule history nightly-commit
vulcan schedule run nightly-commit --dry-run
vulcan schedule run nightly-commit   # once, now, without the daemon
vulcan schedule disable nightly-commit --dry-run
vulcan schedule enable nightly-commit
vulcan schedule trust status         # trusted-commit only: see "Who may run what"
```

All commands take `--output json`. Run history is kept on each device, outside the vault and
its cache: the newest 200 runs per job, for up to 30 days.
