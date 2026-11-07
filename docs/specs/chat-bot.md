# Chat bot: capture and reminder delivery

**Status:** Draft implementation contract, 2026-11-07 (Roadmap 10.11). Telegram is the first and
only adapter.

## Purpose

A small, deterministic chat bot that the daemon hosts, giving one person (or a few allowlisted
people) a phone-friendly way into their vaults:

- **capture**: text, links, photos, and files into the vault, through QuickAdd capture choices, the
  inbox, or the daily note;
- **tasks**: create tasks from natural language, list what is due, and complete them;
- **reminders**: receive due reminders with buttons (done, snooze) that write back into Markdown;
- **lookups**: search, and read a note.

It replaces the deferred "Reminder delivery channels" item. Each command is a thin adapter over an
existing finite `vulcan-app` workflow, so the bot adds no write semantics of its own.

## Non-goals

- **No LLM chat.** The embedded assistant (Phase 9.21) was built and then retired because external
  runtimes over MCP gave a better experience. The bot never forwards free text to a model;
  free text is a capture.
- **No multi-platform identity framework.** The retired 9.21.12 transport contract (principals,
  chat spaces, identity bindings) is not revived. The bot command layer is transport-neutral so a
  Matrix or Signal adapter can reuse it later, but v1 models only "allowlisted user → permission
  profile and vaults".
- **No external relay.** No webhook endpoint, n8n, or message broker is required. The bot polls
  Telegram with `getUpdates`, so it works behind NAT and on a laptop.
- **No group chats in v1.** Only private chats with allowlisted users are served.

## Placement

| Layer | Owns |
| --- | --- |
| `vulcan-core` | Inline reminder syntax and vault `[bot]` / `[tasks.reminders]` config types |
| `vulcan-app` | The reminder occurrence planner (`tasks reminders`, TaskNotes and inline tasks). `vulcan-app::chat`: transport-neutral commands: parse a chat message into a `ChatCommand`, run it against a vault with a permission guard, return a structured `ChatReply` (text blocks, buttons, attachments wanted). Reminder dispatch, resolution, and action workflows |
| `vulcan-daemon` | The `telegram-bot` optional service: Bot API client, long-poll loop, rendering `ChatReply` into Telegram messages, the reminder timer, and device-local state |
| `vulcan-cli` | `vulcan daemon config set-telegram-bot|set-bot-user|…` setup, `vulcan daemon bot-status`; `vulcan tasks reminders` lists planned occurrences |

The command layer never sees Telegram types, and the Telegram client never touches the vault.

## Configuration

### Device (daemon registry, `~/.config/vulcan/`)

The bot token is a device secret and the bot is device infrastructure, like the webhook alert
sinks. Telegram permits only one `getUpdates` poller per token, so configuring the bot on a second
device fails visibly (HTTP 409 from Telegram), which the service reports as a conflict instead of
retrying silently.

```toml
[bot.telegram]
token_env = "VULCAN_TELEGRAM_BOT_TOKEN" # variable holding the token; never the token itself
# api_base = "https://api.telegram.org" # HTTPS, or loopback HTTP (a local Bot API server, tests)

[[bot.telegram.users]]
telegram_id = 123456789                 # numeric user id; usernames are mutable and not trusted
vaults = ["notes", "rpg"]               # registered vault names this user may reach
default_vault = "notes"
profile = "bot-capture"                 # permission profile, capped by each registration's ceiling
reminders = true                        # this user receives reminders for these vaults
```

- The service `worker.telegram-bot` is enabled when `[bot.telegram]` exists and the token
  variable is set, from the daemon's environment or `~/.config/vulcan/daemon.env`. There is no
  token file: `daemon.env` already is the device's private secret file. With the section but no
  token, the service is listed as disabled with the variable's name as the reason.
- Bot users may name vaults that were later unregistered; the bot skips them at runtime, so
  `vulcan vault remove` is never blocked by the allowlist. Allowlist edits apply without a restart.
- Unknown users get no reply at all, so the bot does not confirm that it exists. Their attempts are
  counted in status with the user id.
- The effective authority for a command is the intersection of the user's profile and the vault
  registration's ceiling, as for schedules. The bot can never widen what the device allows.
- `bot-capture` is a suggested profile, not a built-in: read access plus writes limited to the
  inbox, daily notes, the TaskNotes folder, and attachments. The default when `profile` is
  omitted is `readonly`, so a forgotten setting cannot grant write access.

### Vault (`.vulcan/config.toml`, synced)

```toml
[bot]
default_capture = "inbox"           # "inbox" | "daily"
task_target = "tasknotes"           # "tasknotes" | "inline": what /task creates
attachment_folder = "Attachments"   # where sent files are saved
# deferred: QuickAdd capture choices by name, link_capture

[tasks.reminders]                   # implemented (slice 1)
inline = true                       # parse ⏰ / (@…) / [remind:: …] on inline tasks
due_dates = "ignore"                # "ignore" | "notify": 📅 alone fires at default_time
default_time = "09:00"
timezone = "Europe/Berlin"          # IANA name; the device's local timezone when unset

[bot.reminders]                     # implemented (slices 4 and 5)
catch_up_window = "24h"             # older missed reminders are not sent
snooze_options = ["1h", "tomorrow"] # buttons, at most 3 besides "done"
privacy = "full"                    # "full" (title and note path) | "title_only"

[[bot.reminders.privacy_overrides]]
glob = "GM/**"
privacy = "title_only"
```

## Commands

| Input | Effect | Workflow |
| --- | --- | --- |
| free text | capture to `bot.default_capture` | inbox capture / QuickAdd capture / daily append |
| a message that is only a URL | deferred: captured as text for now; planned capture to `bot.link_capture` with the title fetched only if the profile allows network access | as above |
| photo, document, voice, audio, video | save under `bot.attachment_folder` (sanitized name, never overwriting) and capture `![[path]]` with the caption as text; at most 20 MB, the Bot API's download limit | `vulcan-app::chat::run_chat_attachment` |
| `/c <text>`, `/capture <text>` | capture to the default target (QuickAdd choices by name are planned; Vulcan has no reusable QuickAdd capture workflow yet) | as free text |
| `/daily <text>` | append to today's daily note | periodic append |
| `/task <natural language>` | create a TaskNotes task (date, time, priority, and tags parsed as in `vulcan tasks add`), or an inline task in the inbox note with `bot.task_target = "inline"` | TaskNotes create / inline task create |
| `/today`, `/due [7]` | open inline and TaskNotes tasks due or scheduled up to today (or N days ahead), each with a Done button | Tasks query |
| `/reminders [24h]` | upcoming reminders | tasks reminders |
| `/find <query>` | top 8 search hits, each with a button that sends the note | search |
| `/note <path or title>` | send the note as text (split at 4096 characters) or as a file when it is long | note get |
| `/vault [name]` | show or switch this chat's active vault | bot state |
| `/undo` | revert the last capture, task, or attachment made from this chat within 7 days, all or nothing, if every file is byte-for-byte as the capture left it | bot state + `run_chat_undo` |
| `/help` | list commands (also registered with `setMyCommands`) | — |

Every write goes through the ordinary write path, so the mutation scheduler, auto-commit, and Git
sync apply. Replies confirm the target note path. Errors are sanitized: no stack traces, tokens, or
absolute paths.

## Reminders

### Syntax

| Source | Syntax |
| --- | --- |
| TaskNotes | frontmatter `reminders:` (already parsed and evaluated) |
| Inline, canonical | `- [ ] Call X ⏰ 2026-11-07 10:00` |
| Inline, also parsed | `(@2026-11-07 10:00)` (obsidian-reminder), `[remind:: 2026-11-07T10:00]` |
| Due-date fallback | `📅 2026-11-07` fires at `default_time` when `due_dates = "notify"` |

Writes from Vulcan (snooze) use the canonical `⏰` form. Unparseable reminder values produce
diagnostics.

TaskNotes dates and times without a zone (`scheduled: 2026-11-08T12:00`) are wall-clock times in
`tasks.reminders.timezone`, as TaskNotes reads them in the device's timezone; an explicit offset is
honored. A relative reminder whose anchor is a bare date fires relative to `default_time`, not
midnight. `/task` adds `tasknotes.task_creation_defaults.default_reminders`, so a default reminder
`0 minutes before scheduled` makes `/task call Anna tomorrow 10:00` remind at 10:00.

### Occurrence identity

`occurrence_id = blake3(domain ‖ task key ‖ reminder id ‖ notify instant in UTC)`, reported by
`tasks reminders`; the bot ledger keys it by vault. The task key is `tasknote:<path>` for TaskNotes,
otherwise `id:<🆔>`, `block:<path>#<block id>`, or `text:<path>:<hash of the task title>` (the text
without Tasks annotations, Dataview fields, reminder forms, and block id). Completing a recurring
inline task moves its `⏰` with its other dates. Editing the text of a task that has no ID therefore makes a new
occurrence. Vulcan never writes IDs only to make reminders work.

### Delivery

- The `telegram-bot` service plans, once a minute, the open reminders of each vault for each
  allowlisted user with `reminders = true`, through that user's permission guard, so a user is
  never told about a note they may not read. Planning first proves the cache fresh or scans
  incrementally. Due occurrences are checked on every poll cycle (at most about 8 s apart), so a
  reminder arrives within seconds of its time and edits in the vault take effect within a minute.
  A plain timer loop was chosen over a wakeable sleep: the poll loop already wakes that often and
  this keeps the Telegram client and ledger on one thread.
- Reminders are delivered only after a successful `getUpdates`: a device in conflict with another
  poller of the same bot sends nothing, so two devices never deliver the same reminder.
- The device-local ledger (`daemon/bot.sqlite`, table `reminders`) is keyed by chat, vault, and
  occurrence id and stores the task key, task reference, path, message text, notify instant,
  state (`sending`, `open`, `expired`, `resolved`), and message id. An entry is written as
  `sending` before each send; a send that failed or was interrupted is retried after a minute, so
  a duplicate message is preferred over a lost reminder. A 403 (the user blocked the bot) records
  the entry as expired instead of retrying. Expired and resolved entries are pruned after 60 days;
  open ones are kept.
- Missed occurrences inside `catch_up_window` are sent, marked `(late)` when more than 5 minutes
  behind. Older ones are recorded as expired and not sent.
- Message: `⏰ <task title>`, the note path (unless `title_only` applies, by the first matching
  `privacy_overrides` glob or else `privacy`), and the notify time in the vault's reminder
  timezone, with buttons `Done`, then one per snooze option (`Snooze 1h`, `Tomorrow`). Buttons are
  offered only when the user's profile may write the task's note.

### Resolution

Every cycle compares open ledger entries with the latest plan. An entry whose occurrence is no
longer planned, because its task was completed, cancelled, rescheduled, or deleted by any other
means, has its message edited (losing any buttons) to add "Next reminder: <time>." when the task
still has a later reminder, or "No longer open: completed, cancelled, or removed." otherwise. A
vault whose plan fails is skipped, so a broken config never resolves its messages.

### Actions

A button carries `callback_data = "r:<ledger row id>:<index>"` (≤ 64 bytes), where the index
names one of the actions recorded with the ledger entry when it was sent. The path, vault, task,
and action come only from the ledger, never from the callback. A press is accepted only from the
chat the reminder was sent to, only for an open entry, and only for an offered action; anything
else answers "This reminder is no longer open." It is acknowledged with `answerCallbackQuery`
either way. The action re-plans the vault through the user's guard and acts on the task of the
recorded occurrence id, so a moved line or an edited vault never redirects it.

| Action | Effect |
| --- | --- |
| `done` | the existing completion path, including recurrence (a recurring task completes the occurrence the reminder belongs to) |
| `<duration>` | set the remind time to now + duration, rounded up to the minute (TaskNotes: add an absolute reminder described "Snoozed"; inline: replace `⏰`, `(@…)`, or `remind::` with `⏰`, or add it) |
| `tomorrow` | next day at `default_time` in the vault timezone |

Snooze is the reusable `vulcan-app` workflow `apply_task_snooze_with_guard`, under the same
mutation permit, cache refresh, and auto-commit as other bot writes. The message is then edited to
show the outcome and loses its buttons, the entry is resolved, and the vault is re-planned at once
so a snoozed reminder arrives on time. A task that already changed results in a no-op that says
so.

## Telegram transport

- A minimal first-party client (`vulcan-daemon::telegram_api`) over blocking reqwest, since the
  service runs on its own thread like the other workers. Redirects are disabled. Methods: `getMe`,
  `getUpdates` (long poll, 8 s so shutdown is prompt, `allowed_updates = ["message",
  "callback_query"]`), `sendMessage`, `editMessageText`, `answerCallbackQuery`, `sendDocument`,
  and `setMyCommands`; `getFile` arrives with attachments (slice 6). No bot framework dependency.
- Plain-text messages only, clipped to Telegram's 4096 characters. No parse mode, so vault text
  can never be misread as markup.
- Only private chats are served; messages in groups and from bots are ignored.
- Buttons carry `a:<token>`, an opaque id of an action stored in `bot.sqlite` for 30 days and
  bound to the chat it was sent to. A token pressed in another chat, or expired, answers "This
  button has expired." A Done button's token is removed once it ran.
- Errors shown to users have the vault's absolute path removed.
- The update offset is persisted after each update is handled, so a crash re-delivers at most
  the update in flight.
- Rate limits: honor `retry_after` on 429. Outgoing messages are serialized per chat.
- The token never appears in logs, status, or errors: request errors drop their URL, and the
  client's `Debug` output redacts it.
- Outbound access to the Bot API is consented to by the device configuration itself, like webhook
  alert sinks; vault permission profiles govern what users' commands may do in vaults, not the
  daemon's own transport.
- Exponential backoff (to 60 s) on network errors, `retry_after` on 429, 30 s on a 409 conflict.
  A 401 (revoked token) stops the service with a visible failure instead of retrying forever; the
  host's bounded restart policy then gives up.

## State

All bot state is device-local, outside `cache.db` and outside sync, in `daemon/bot.sqlite`:
the update offset, per-chat active vault, and button actions (slice 3); the reminder ledger
(slice 4); and the last capture for `/undo` (slice 6): each written file's path, its content
before (absent when the capture created it; notes over 1 MB are not kept), and a hash of its
content after. The service also writes
`daemon/telegram-bot.json`: state (`starting`, `polling`, `backoff`, `rate_limited`, `conflict`,
`failed`, `stopped`), bot username, last poll, last error, handled updates, reminders sent, and
ignored senders.

## CLI

Bot configuration lives with the other device-local daemon settings:

- `vulcan daemon config set-telegram-bot [--token-env NAME] [--api-base URL]`: enable the bot;
  existing users are kept. `clear-telegram-bot` removes the section and its users.
- `vulcan daemon config set-bot-user ID --wiki NAME… [--default-wiki NAME] [--profile NAME]
  [--reminders]`: add or replace a user. Wikis must be registered. `remove-bot-user ID`.
- `vulcan daemon bot-status`: configuration, whether the token variable is set (reading
  `daemon.env`), users, the service's last state, including ignored sender ids, which is how a
  new user finds their numeric id, and reminder ledger counts by state.
- `vulcan tasks reminders [--upcoming 24h]`: the planner, with `source`, `task`, `task_key`, and
  `occurrence_id` per row (slice 1).
- A separate ledger listing command was dropped: `bot-status` shows the counts and
  `vulcan tasks reminders` shows what is planned.

All of them support `--output json` and the config commands `--dry-run`. A separate `bot test`
command was dropped: messaging the bot and checking `bot-status` covers it.

## Slices

1. **Inline reminder syntax and planner** in core and app; inline reminders in `vulcan tasks reminders`.
2. **Chat command layer** in `vulcan-app::chat`: parser, capture, `/task`, `/today`, `/find`,
   `/note`, and `/vault`, tested without Telegram.
3. **Telegram service**: client, polling, allowlist, rendering, and state, tested against a fake
   Bot API server; `vulcan daemon config` bot commands and `vulcan daemon bot-status`.
4. **Reminder delivery**: timer, ledger, catch-up, and resolution edits.
5. **Reminder actions**: done and snooze, ledger-gated.
6. **Attachments and `/undo`**, skills and user guide (`docs/guide/chat-bot.md`).

## Tests

- Planner: TaskNotes, inline `⏰`, `(@…)`, `remind::`, due-date fallback on and off, DST gap,
  timezone, recurrence, and identity stability across line moves.
- Command layer: each command against fixture vaults, permission denial for writes outside the
  profile, and unknown commands.
- Telegram service against a local fake Bot API: allowlist (silent drop), 409 conflict, 401 stop,
  429 `retry_after`, offset persistence across restart, and callback validation (wrong chat,
  stale entry, unoffered action).
- Reminder lifecycle: send → done via button → Markdown updated; send → completed elsewhere →
  message edited; crash before `message_id` is recorded → resend; catch-up window.
