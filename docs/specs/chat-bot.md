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
| `vulcan-cli` | `vulcan bot …` setup and diagnostics; `vulcan tasks reminders` lists planned occurrences |

The command layer never sees Telegram types, and the Telegram client never touches the vault.

## Configuration

### Device (daemon registry, `~/.config/vulcan/`)

The bot token is a device secret and the bot is device infrastructure, like the webhook alert
sinks. Telegram permits only one `getUpdates` poller per token, so configuring the bot on a second
device fails visibly (HTTP 409 from Telegram), which the service reports as a conflict instead of
retrying silently.

```toml
[bot.telegram]
enabled = true
token_env = "VULCAN_TELEGRAM_TOKEN"     # or token_file = "~/.config/vulcan/telegram-token"
reminders = true                        # deliver reminders for the vaults below

[[bot.telegram.users]]
telegram_id = 123456789                 # numeric user id; usernames are mutable and not trusted
vaults = ["notes", "rpg"]               # registered vault names this user may reach
default_vault = "notes"
profile = "bot-capture"                 # permission profile, capped by each registration's ceiling
reminders = true                        # this user receives reminders for these vaults
```

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
default_capture = "inbox"           # "inbox" | "daily" | "<QuickAdd capture choice name>"
link_capture = "inbox"              # target for messages that are only a URL
attachments_folder = "Attachments/Inbox"   # default: the vault's attachment folder

[tasks.reminders]                   # implemented (slice 1)
inline = true                       # parse ⏰ / (@…) / [remind:: …] on inline tasks
due_dates = "ignore"                # "ignore" | "notify": 📅 alone fires at default_time
default_time = "09:00"
timezone = "Europe/Berlin"          # IANA name; the device's local timezone when unset

[bot.reminders]                     # planned (slice 4)
catch_up_window = "24h"             # older missed reminders are not sent
snooze_options = ["1h", "tomorrow"] # buttons, at most 3 besides "done"
privacy = "full"                    # "full" | "title_only"

[[bot.reminders.privacy_overrides]]
glob = "GM/**"
privacy = "title_only"
```

## Commands

| Input | Effect | Workflow |
| --- | --- | --- |
| free text | capture to `bot.default_capture` | inbox capture / QuickAdd capture / daily append |
| a message that is only a URL | capture to `bot.link_capture`; the title is fetched only if the profile allows network access | as above |
| photo, document, voice | save under `attachments_folder` and capture an embed link, with the caption as text | attachment write plus capture |
| `/c <choice> <text>` | capture with a named QuickAdd choice | QuickAdd capture |
| `/daily <text>` | append to today's daily note | periodic append |
| `/task <natural language>` | create a TaskNotes task (date, time, priority, and tags parsed as in `vulcan tasks add`) | TaskNotes create |
| `/today`, `/due [7d]` | tasks due or scheduled in the window, each with a Done button | tasks due |
| `/reminders [24h]` | upcoming reminders | tasks reminders |
| `/find <query>` | top 8 search hits, each with a button that sends the note | search |
| `/note <path or title>` | send the note as text (split at 4096 characters) or as a file when it is long | note get |
| `/vault [name]` | show or switch this chat's active vault | bot state |
| `/undo` | revert the last capture made from this chat, if the file is unchanged since | bot state + note write |
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

### Occurrence identity

`occurrence_id = blake3(domain ‖ task key ‖ reminder id ‖ notify instant in UTC)`, reported by
`tasks reminders`; the bot ledger keys it by vault. The task key is `tasknote:<path>` for TaskNotes,
otherwise `id:<🆔>`, `block:<path>#<block id>`, or `text:<path>:<hash of the task title>` (the text
without Tasks annotations, Dataview fields, reminder forms, and block id). Completing a recurring
inline task moves its `⏰` with its other dates. Editing the text of a task that has no ID therefore makes a new
occurrence. Vulcan never writes IDs only to make reminders work.

### Delivery

- The `telegram-bot` service plans occurrences for every vault where at least one user has
  `reminders = true`, sleeps until the next notify instant (wakeable), and replans after each scan,
  config change, or clock jump.
- The device-local ledger (`daemon/bot.sqlite`) records `sent(occurrence_id, vault, task key, path,
  notify_utc, chat_id, message_id, state, actions)` before each send. After a crash, an occurrence
  without a recorded `message_id` is sent again; a duplicate message is preferred over a lost
  reminder.
- Missed occurrences inside `catch_up_window` are sent and marked late. Older ones are recorded as
  expired and not sent.
- Message: the task text, the note path (unless `title_only`), due time, and buttons
  `Done · Snooze 1h · Tomorrow`.

### Resolution

After each scan the service re-checks open ledger entries. A task that was completed, cancelled,
rescheduled, or deleted by any other means has its message edited to say so, and its buttons
removed.

### Actions

A button carries `callback_data = "r:<ledger row id>:<action>"` (≤ 64 bytes). The path, vault, and
task come only from the ledger, never from the callback. A press is accepted only from the chat the
reminder was sent to, only for an open entry, and only for an offered action. It is acknowledged
with `answerCallbackQuery` either way.

| Action | Effect |
| --- | --- |
| `done` | the existing completion path, including recurrence |
| `snooze:<dur>` | set the remind time to now + duration (TaskNotes: add an absolute reminder; inline: replace or add `⏰`) |
| `snooze:tomorrow` | next day at `default_time` in the vault timezone |

The message is then edited to show the outcome and loses its buttons. A task that already changed
results in a no-op that says so.

## Telegram transport

- A minimal first-party client over the daemon's async reqwest with these methods: `getMe`,
  `getUpdates` (long poll, 50 s, `allowed_updates = ["message", "callback_query"]`),
  `sendMessage`, `editMessageText`, `editMessageReplyMarkup`, `answerCallbackQuery`, `getFile` plus
  file download, `sendDocument`, and `setMyCommands`. No bot framework dependency.
- Plain-text messages with entities, or HTML parse mode with escaping in one function. No
  MarkdownV2.
- The update offset is persisted after each update is handled. Handling is idempotent on
  `update_id` for the rare redelivery after a crash.
- Rate limits: honor `retry_after` on 429. Outgoing messages are serialized per chat.
- The token never appears in logs, status, or errors; request URLs are redacted to
  `api.telegram.org/bot<redacted>/<method>`.
- The permission check for outbound network access to `api.telegram.org` uses the device
  profile, like other daemon network clients.
- Backoff with jitter on network errors. A 401 (revoked token) stops the service with a visible
  failure instead of retrying forever.

## State

All bot state is device-local, outside `cache.db` and outside sync, in `daemon/bot.sqlite`:
the update offset, per-chat active vault, the last capture for `/undo`, the reminder ledger, and
handled update ids (pruned after 7 days).

## CLI

- `vulcan bot status`: service state, bot identity (`getMe`), last poll, users, ledger counts, and
  rejected users.
- `vulcan bot test [--user ID]`: send a test message (needs the token, not the daemon).
- `vulcan tasks reminders [--upcoming 24h]`: the planner, with `source`, `task`, `task_key`, and
  `occurrence_id` per row (slice 1).
- `vulcan bot reminders [--open]`: inspect the sent-reminder ledger.

All of them support `--output json`. Bot configuration is edited with the existing daemon config
commands.

## Slices

1. **Inline reminder syntax and planner** in core and app; inline reminders in `vulcan tasks reminders`.
2. **Chat command layer** in `vulcan-app::chat`: parser, capture, `/task`, `/today`, `/find`,
   `/note`, and `/vault`, tested without Telegram.
3. **Telegram service**: client, polling, allowlist, rendering, and state, tested against a fake
   Bot API server; `vulcan bot status|test`.
4. **Reminder delivery**: timer, ledger, catch-up, and resolution edits.
5. **Reminder actions**: done and snooze, ledger-gated.
6. **Attachments and `/undo`**, skills and user guide.

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
