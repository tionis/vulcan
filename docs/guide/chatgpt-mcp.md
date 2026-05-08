# ChatGPT MCP Wiki Setup

Use this setup for a private ChatGPT Developer Mode connection to a personal wiki.

## Recommended Server Shape

Do not expose `vulcan mcp --auth-token` directly to the public internet. Run Vulcan on loopback or a private network, then place an HTTPS front door in front of it.

Preferred production shape:

1. Run Vulcan on a private bind:

   ```sh
   vulcan --permissions daily-wiki-agent mcp \
     --transport http \
     --bind 127.0.0.1:8765 \
     --endpoint /mcp \
     --tool-pack notes-read,notes-write,notes-manage,search,status,daily,tasks,custom
   ```

2. Terminate HTTPS and OAuth at a reverse proxy or thin adapter.
3. Forward only authenticated requests to `http://127.0.0.1:8765/mcp`.
4. Keep shell, host execution, git mutation, unrestricted network, broad refactor, and config writes out of the selected permission profile.

`daily-wiki-agent` is the built-in pilot profile for this shape. It allows full vault note/task edits, config reads, and no shell, host execution, git mutation, refactor, network, or explicit index maintenance.

## ChatGPT Developer Mode

In ChatGPT Developer Mode, add the HTTPS MCP URL exposed by your front door. Refresh tools after changing Vulcan's `--tool-pack` flags or after enabling packs in adaptive mode.

Use static packs for hosts that do not react to `notifications/tools/list_changed`. Use adaptive mode only when the host reliably refreshes `tools/list` after pack mutations:

```sh
vulcan --permissions daily-wiki-agent mcp \
  --transport http \
  --tool-pack-mode adaptive \
  --tool-pack notes-read,search,status,daily,tasks
```

## Tool Selection Guidance

For daily workflow questions, prefer:

- `daily_show` before `note_get` for today's daily note.
- `daily_list` for week or month summaries.
- `task_list` or `task_query` for task summaries.
- `task_create`, `task_complete`, and `task_reschedule` for task changes.
- `note_append --periodic daily` as a low-risk log fallback.

Use generic note edits only when the task/daily tools do not model the requested change. MCP `note_set` requires `confirm: true`, and `note_delete` requires either `dry_run: true` or `confirm: true`.

## Vault Preparation

Run:

```sh
vulcan agent install --overwrite
```

Then edit the vault `AGENTS.md` with your routine conventions, inbox paths, daily-note headings, task conventions, and edit rules. Add a Daily Review or Routine skill under `.agents/skills/` when the routine has stable steps.
