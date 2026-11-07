# Telegram chat bot

The daemon can host a small Telegram bot for you and the people you allow. From a phone you can
capture notes, photos, and voice messages, create and complete tasks, search and read notes, and
receive reminders with Done and Snooze buttons. Everything the bot writes goes through the same
workflows, permission checks, and auto-commit as the CLI, so it syncs like any other edit.

The bot is deterministic: it runs commands, it does not chat. It runs on one device, the one whose
daemon holds the bot token, and needs no server or relay besides Telegram itself.

The full contract is in [`docs/specs/chat-bot.md`](../specs/chat-bot.md).

## Set it up

1. Create a bot with [@BotFather](https://t.me/BotFather) (`/newbot`) and copy its token.
2. Store the token where only the daemon reads it, in `~/.config/vulcan/daemon.env` (mode `0600`):

   ```sh
   echo 'VULCAN_TELEGRAM_BOT_TOKEN=123456:ABC...' >> ~/.config/vulcan/daemon.env
   chmod 600 ~/.config/vulcan/daemon.env
   ```

   The token never goes into configuration; Vulcan stores only the variable's name.
3. Enable the bot and restart the daemon:

   ```sh
   vulcan daemon config set-telegram-bot --dry-run
   vulcan daemon config set-telegram-bot
   vulcan daemon stop && vulcan daemon start --detach
   ```

4. Send your bot any message, then look up your numeric Telegram id. The bot ignores unknown users
   without replying, but counts them:

   ```sh
   vulcan daemon bot-status
   ```

5. Allow yourself, naming the registered vaults you may use:

   ```sh
   vulcan daemon config set-bot-user 123456789 --wiki notes --wiki rpg \
     --default-wiki notes --profile bot-capture --reminders
   ```

   Allowlist changes take effect without a restart.

Only one device may poll a given bot. If two devices are configured with the same token,
`bot-status` shows `conflict` on one of them and that device delivers nothing.

## Permissions

Each user's commands run with the permission profile named by `--profile`, which defaults to
`readonly`. It is resolved from the built-in and device-local profiles only, and it must stay
within each vault registration's `permissions_profile`; a wider profile is refused with an error.

A narrow profile for capture is a good choice. In the vault's device-local `config.local.toml`:

```toml
[permissions.profiles.bot-capture]
read = "all"
write = { allow = ["note:Inbox.md", "folder:Journal/**", "folder:TaskNotes/**", "folder:Attachments/**"] }
```

Reminders only mention notes the user may read, and reminder buttons appear only when the user may
write the task's note.

## Commands

| Send | What happens |
| --- | --- |
| any text | captured to the inbox, or to today's daily note with `bot.default_capture = "daily"` |
| a photo, file, voice message, audio, or video | saved under `bot.attachment_folder` and captured as an embed, with the caption as text (files up to 20 MB) |
| `/task <text>` | a TaskNotes task with natural-language dates (`/task call Anna tomorrow 10:00`), or an inline task in the inbox with `bot.task_target = "inline"` |
| `/daily <text>` | appended to today's daily note |
| `/today`, `/due [days]` | open tasks due or scheduled by then, each with a Done button |
| `/reminders [24h]` | upcoming reminders |
| `/find <words>` | search results, each with a button that sends the note |
| `/note <path or title>` | the note's text, or the file when it is long |
| `/undo` | revert the last capture, task, or attachment from this chat, if nothing changed it since; replies to captures also carry an Undo button |
| `/vault [name]` | show or switch the vault this chat works in |
| `/help` | the command list |

Vault settings live in `.vulcan/config.toml`:

```toml
[bot]
default_capture = "inbox"           # inbox | daily
task_target = "tasknotes"           # tasknotes | inline
attachment_folder = "Attachments"
```

## Reminders

Users set up with `--reminders` receive the reminders of their vaults: TaskNotes reminders and
inline reminders such as `- [ ] Call the bank ⏰ 2026-11-07 10:00` (see `vulcan tasks reminders`).
They arrive within seconds of their time. A reminder whose task is completed or rescheduled
elsewhere has its message updated.

The buttons write back to the vault:

- **Done** completes the task, including the right occurrence of a recurring task.
- **Snooze 1h** and **Tomorrow** move the reminder: an inline task's `⏰` is rewritten, a TaskNotes
  task gains a reminder described "Snoozed".

```toml
[bot.reminders]
catch_up_window = "24h"               # reminders missed for longer, e.g. while the device was off, are skipped
snooze_options = ["1h", "tomorrow"]   # at most three
privacy = "full"                      # full (title and path) | title_only

[[bot.reminders.privacy_overrides]]
glob = "GM/**"
privacy = "title_only"
```

Reminder times are read in `tasks.reminders.timezone`, or the device's timezone when unset.

A task created with `/task` has a date, not a reminder. To be reminded at a task's time, give new
tasks a default reminder in the vault configuration:

```toml
[[tasknotes.task_creation_defaults.default_reminders]]
id = "at-time"
type = "relative"
related_to = "scheduled"   # or "due", whichever your natural-language dates fill
offset = 0
unit = "minutes"
direction = "before"
```

`/task call Anna tomorrow 10:00` then reminds at 10:00, and the reply shows the date and reminder
it understood. A task with a date but no time is reminded at `tasks.reminders.default_time` (09:00
by default). A reminder that was already due when its task was created, such as a 09:00 default
reminder on a task created at noon and scheduled for today, is not sent. Reminders are added when a task is created,
so existing tasks keep theirs. Inline tasks use `⏰` instead (see above).

## Alerts and the daily digest

Two more things the bot can send unprompted, set per user (repeat every flag you want to keep,
since `set-bot-user` replaces the user's entry):

```sh
vulcan daemon config set-bot-user 123456789 --wiki notes --profile bot-capture \
  --reminders --alerts --digest 07:30
```

- `--alerts` sends the daemon's attention alerts for the user's vaults: synchronization that
  failed, needs conflict resolution, or paused. They use the same durable, retried delivery as
  desktop and webhook alerts. Restart the daemon after giving the first user alerts.
- `--digest 07:30` sends a morning summary per vault: open tasks due or scheduled up to today,
  with Done buttons, and the reminders still to come that day. It is sent once a day at or after
  that time in the vault's reminder timezone, and skipped when there is nothing to report.

## Status and troubleshooting

`vulcan daemon bot-status` shows whether the token variable is set, the allowed users, the service
state, the last error, ignored senders, and reminder counts. `vulcan daemon status` lists the
`worker.telegram-bot` service; without the token it is disabled and names the missing variable.

- **No reply at all**: the sender is not allowlisted (check ignored senders), or the chat is a group.
  The bot answers private chats only.
- **`failed` with "token was rejected"**: the token was revoked or mistyped. Fix `daemon.env` and
  restart the daemon.
- **`conflict`**: another device polls the same bot. Remove it there with
  `vulcan daemon config clear-telegram-bot`.
- **"exceeds this device's limit"**: the user's profile is wider than the vault registration allows.

Bot state (the update position, each chat's vault, button actions, the reminder ledger, and the
last capture for `/undo`) is device-local in the daemon state directory and never syncs. Losing it
is harmless: old buttons stop working and missed reminders inside the catch-up window are sent.
