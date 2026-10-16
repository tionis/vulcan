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
- A job never overlaps itself. If it is still running when it comes due again, one follow-up runs
  after it finishes.
- Daylight saving time: a cron time that does not exist (spring forward) runs at the first moment
  after the gap. A time that occurs twice (fall back) runs once, at its first occurrence.
- A failed run is retried after 30 seconds, as many times as `retries` allows. A run interrupted
  by a crash is retried at startup if retries remain, and otherwise recorded as interrupted.
- Disabling, removing, reassigning, or changing a running job asks that run to stop. A run past
  `max_runtime_seconds` is also asked to stop. Both targets finish their current step first.

## Who may run what

Every run, scheduled or manual, is authorized first. Under v1's repository trust, the vault's job
definitions are accepted as written, but a job's `authority` profile must stay within this
device's ceiling for the vault: the registration's permission profile (`vulcan vault set <id>
--permissions-profile <name>`). A job asking for more than the ceiling fails. A synced change can
therefore narrow what a job may do, never widen it. Signed job definitions, which only an
administrator can authorize, are planned as a later trust mode.

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
```

All commands take `--output json`. Run history is kept on each device, outside the vault and
its cache: the newest 200 runs per job, for up to 30 days.
