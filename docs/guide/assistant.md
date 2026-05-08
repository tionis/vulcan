# Embedded Assistant Host

`vulcan assistant` is an optional managed-engine host mode. Vulcan owns vault
context, permission-profile selection, MCP tool exposure, and process lifecycle.
The managed engine is a JSONL RPC subprocess that owns model inference and its
own session format.

This design keeps the same boundary that proved important for the MCP server:
tool exposure and policy are Vulcan responsibilities. The engine should not
directly rewrite notes, bypass permission profiles, or become the source of
truth for vault state.

## Prerequisites

The initial runtime is `pi` in RPC mode:

```sh
vulcan assistant --doctor
```

Set `[assistant].pi_binary` when `pi` is not on `PATH`.

## Configuration

Assistant settings live in `.vulcan/config.toml`:

```toml
[assistant]
runtime = "pi"
pi_binary = "pi"
provider = ""
model = ""
thinking_level = "medium"
permissions = "readonly"
sessions_dir = "AI/Sessions"
session_export = "on_exit"
session_exports_dir = "AI/Assistant Sessions"
```

Empty `provider` and `model` values let the managed engine use its own defaults.
An empty `sessions_dir` or `--ephemeral` starts the engine without session
persistence.

`session_export` controls the Markdown archive layer:

- `manual`: keep only the managed engine session files.
- `on_exit`: export the latest managed session to an Obsidian-readable Markdown
  note when `vulcan assistant` exits. This is the default.
- `always`: currently behaves like `on_exit` in the CLI host; daemon mode may
  use it for after-each-turn exports later.

Exports go to `session_exports_dir` as normal notes with YAML frontmatter and
`[!user]`, `[!assistant]`, and `[!tool]` callouts. The raw pi session remains
the source used for resume/continue.

## Usage

Inspect the launch configuration:

```sh
vulcan assistant --doctor
```

Inspect the context payload sent to the engine:

```sh
vulcan assistant --print-context --tool-pack notes-read,search,status
```

Run a one-shot prompt:

```sh
vulcan assistant "Summarize today's routine"
```

Pipe a prompt in non-interactive mode:

```sh
printf '%s\n' "Find open tasks for this week" | vulcan assistant
```

List locally persisted session files:

```sh
vulcan assistant --list-sessions
```

Start an interactive chat:

```sh
vulcan assistant --chat
```

Resume the newest persisted session:

```sh
vulcan assistant --chat --continue
```

Chat slash commands include `/model`, `/models`, `/thinking`, `/compact`,
`/new`, `/stats`, `/state`, `/steer <text>`, `/follow-up <text>`,
`/set-model <provider> <model>`, `/help`, and `/quit`.

## Permissions And Tools

Use `--assistant-permissions <profile>` to bind a run to a permission profile.
The default is `[assistant].permissions`, which defaults to `readonly`.

Use `--tool-pack` to expose the same curated tool packs used by MCP:

```sh
vulcan assistant \
  --assistant-permissions daily-wiki-agent \
  --tool-pack notes-read,notes-write,notes-manage,search,status,daily,tasks,custom \
  "Review my daily note"
```

For exploration, prefer `readonly`. For note edits, use a profile that grants
the smallest write surface needed for the workflow.

`vulcan init` creates `AI/Sessions/`, `AI/Assistant Sessions/`, and writes the
bundled pi extension under `.vulcan/assistant/extension/vulcan-tools/` when
`[assistant].runtime = "pi"`. The extension passes the active permission profile
to nested `vulcan` calls and blocks pi built-in shell/edit/write tools in
readonly mode.

## Integration Models

Vulcan supports two assistant integration models:

- External runtime contract: the runtime hosts the conversation and shells out
  to `vulcan --output json` for tools. See `vulcan help assistant-integration`.
- Embedded host mode: `vulcan assistant` starts a managed RPC engine and sends
  it vault context plus filtered tools.

Both models share `AGENTS.md`, skills, permission profiles, MCP-style tool
packs, and the rule that durable artifacts are normal vault notes.

## Current Limits

The embedded host uses a synchronous JSONL RPC client in the CLI process. Rich
extension UI prompts, a session picker for choosing anything other than the
newest session, tab completion in chat mode, and always-on real-pi CI smoke
tests remain deferred until the daemon/WebUI shape needs them.
