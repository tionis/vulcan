# External Harness Integration Sketch

Current recommendation for Phase 9.12: use an external agent runtime instead of building an in-process Rust assistant. `pi` is a useful reference implementation, but it is not the architectural center of the design.

This document is specifically about **subprocess-style runtimes** that can preload vault files such as `AGENTS.md`, the configured prompts folder, and `.agents/skills/`. It is not the MCP integration design by itself. Generic MCP clients do not get that same out-of-band bootstrap, so MCP needs its own server-native discovery surface (`resources`, `prompts`, filtered tool exposure, and later HTTP transport) rather than assuming the exact same pattern.

The older native assistant and chat-runtime steering was not discarded; it was moved to [`native_runtime_deferred.md`](./native_runtime_deferred.md) so the runtime-agnostic decision does not erase those ideas.

Vulcan remains responsible for:

- vault semantics
- command contracts
- JSON output stability
- prompts and skills stored in the vault
- permissions and safety checks

The external runtime remains responsible for:

- model inference
- chat/session UX
- context compaction
- runtime orchestration
- any non-vault-native conversation state

Integrated help for this contract is available inside the CLI:

- `vulcan help assistant-integration`
- `vulcan --output json help assistant-integration`

## Why this boundary

The expensive part to get right is not chat UX. It is note semantics, safe mutations, deterministic query behavior, and a tool surface that an LLM can use without bypassing vault rules.

Vulcan already has the right shape for this:

- `describe --format openai-tools|mcp|json-schema`
- `help --output json <command>`
- single-note CRUD
- search/query/web/git tools
- default `AGENTS.md`
- vault-native skill bundles in `.agents/skills/`

That means the runtime can stay outside Vulcan without losing the important guarantees.

## Non-goals

Phase 9.12 should not:

- add a second vault API just for one runtime
- let an external runtime mutate notes through direct filesystem writes
- duplicate parser or cache logic in a JS package
- implement vault-native chat transcripts or memory notes up front
- build Telegram/Discord/Signal adapters yet

## Recommended Runtime Model

### 1. Startup context

At session start, the runtime should load:

- vault `AGENTS.md` if present
- the compact command map from `vulcan describe`
- the default skill directory summary from `vulcan skill list`

It should not preload every tool schema or every skill body. Discovery stays on demand.

### 2. Tool discovery

Recommended sequence:

1. Call `vulcan describe --format openai-tools`
2. Register the core tools directly
3. Use `vulcan help --output json <command>` only when the model needs an unfamiliar command
4. Call `vulcan skill list` up front, then `vulcan skill get <name>` only when a workflow-specific guide is needed

This keeps context small while preserving full surface area.

Run `vulcan agent install` once per vault to scaffold `AGENTS.md`, the bundled `.agents/skills/`
directory, and the default prompt files. Add `--example-tool` to also write a starter custom tool
under `.agents/tools/`. Re-run with `--overwrite` after upgrading Vulcan if the bundled files
should be refreshed.

### 3. Tool execution

Every vault operation should shell out to Vulcan in JSON mode:

```text
vulcan <command...> --output json
```

Wrapper behavior:

1. validate arguments
2. spawn `vulcan`
3. parse JSON or line-delimited JSON output
4. treat stdout as the machine-readable success payload, stderr as diagnostic text, and non-zero exits as structured tool failures
5. surface timeout failures distinctly from normal non-zero command exits

The wrapper must not inspect SQLite directly or rewrite notes itself.

Every invocation should include an explicit permission profile:

```text
vulcan --vault /path/to/vault --permissions agent --output json <command...>
```

Recommended defaults:

- write-capable sessions: `--permissions agent`
- browse-only sessions: `--permissions readonly`

Vulcan exposes a helper to print the current vault's contract and ready-to-paste wrapper examples:

```text
vulcan agent print-config --runtime generic
vulcan agent print-config --runtime pi
vulcan agent print-config --runtime codex
```

## Recommended Tool Shape In A Reference Integration

Use two layers.

### Layer A: pinned core wrappers

Always register stable wrappers for:

- `note_get`
- `note_create`
- `note_set`
- `note_append`
- `note_patch`
- `search`
- `query`
- `update_property`
- `unset_property`
- `inbox`
- `describe`
- `help`
- `skill_list`
- `skill_get`

These are the tools the model should see immediately.

### Layer B: dynamic wrappers

Generate wrappers for the rest of the command surface from `describe` plus `help`.

Recommended rule:

- keep the CLI command name as the canonical tool name when practical
- do not invent aliases unless the CLI name is unusable in the runtime
- route every wrapper back to the same CLI handler

This preserves the CLI-to-tool 1:1 mapping and avoids drift.

## Trust And Permission Model

The runtime should be configured so generic write/edit/shell tools are disabled or deprioritized for vault work. Vulcan should be the only path for note mutations.

Recommended operating modes:

- read-only: `note get`, `search`, `query`, graph/daily/git/web reads
- edit: add note CRUD and property mutation tools
- refactor: add `move`, `rewrite`, `merge-tags`, and other high-impact commands, preferably with `--dry-run` first

Rules:

- use Vulcan permissions as the enforcement point
- preserve `--dry-run` for high-impact operations
- prefer explicit git commits or batched auto-commit over silent edits
- never let the runtime bypass note safety checks such as `note patch` single-match semantics

Recommended profiles:

- `readonly`: exploration, search, graph inspection, skills, and help only
- `agent`: normal note editing and property mutation through Vulcan
- higher-trust custom profiles: bulk refactors, git mutation, host execution, or network-heavy flows

## Session and persistence boundary

Default assumption: session history lives in the external runtime, not in the vault.

That means Vulcan does not initially need:

- `vulcan assistant --chat`
- gemini-scribe transcript files
- assistant-specific memory notes
- transcript compaction logic

If the user wants durable output, the agent should write a normal note through Vulcan tools.

Later, if needed, Vulcan can add optional export commands such as:

- export current session summary to note
- save selected turns to meeting log
- materialize runtime memory into a vault note

Those should be explicit exports, not the default storage model.

## Suggested package structure

This is a sketch for a reference integration, not a committed implementation:

```text
packages/
  runtime-vulcan/
    README.md
    src/
      index.ts          # runtime entrypoint
      vulcan.ts         # process spawning + JSON parsing
      tools.ts          # pinned core wrappers
      discovery.ts      # describe/help -> dynamic wrappers
      context.ts        # AGENTS.md + skill summary loading
      permissions.ts    # read-only / edit / refactor profiles
```

The critical point is ownership:

- `vulcan.ts` owns subprocess execution and error normalization
- `tools.ts` owns only wrapper registration
- no module owns vault semantics except the `vulcan` binary itself

## Suggested rollout

### Milestone 1

- document the contract
- prove a reference runtime session can read notes, search, query, and patch notes only through Vulcan

### Milestone 2

- add dynamic discovery for non-core commands
- validate skill-driven workflows such as daily review, refactoring, and web research

### Milestone 3

- tighten permission profiles
- decide whether generated runtime-specific config helpers from Vulcan are worth adding

That helper now exists as `vulcan agent print-config --runtime <name>`.

## Asset import helper

Vulcan can preview or import common external-harness layouts into its own vault-native structure:

```text
vulcan agent import
vulcan agent import --apply
vulcan agent import --apply --symlink
```

Detected sources currently include:

- `CLAUDE.md`, `CODEX.md`, `GEMINI.md` -> `AGENTS.md`
- `.claude/commands/*.md`, `.codex/prompts/*.md`, `.gemini/prompts/*.md` -> configured prompts folder
- `.claude/skills/*/SKILL.md`, `.codex/skills/*/SKILL.md`, `.gemini/skills/*/SKILL.md` -> configured skills folder

The default mode is preview-only. Conflicting source files that would target the same Vulcan path are reported and must be resolved before `--apply`.

## Revisit Criteria For A Native Runtime

Re-open the embedded assistant only if one of these remains painful after the external-runtime contract lands:

- vault-native transcripts are essential
- the runtime cannot express the required permission model
- confirmation UX must be enforced inside Vulcan, not in the host runtime
- mobile or chat transports need tighter control than an external runtime can provide
- the cost of keeping runtime logic outside Vulcan becomes higher than owning it

Until then, keep Vulcan opinionated about tools and permissive about runtimes.
