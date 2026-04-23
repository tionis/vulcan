# AGENTS.md for Vulcan Vaults

Use Vulcan as the primary automation surface for this vault.

Core conventions:

- Prefer `vulcan --output json ...` for all tool-driven workflows.
- Use `--dry-run` before bulk or destructive mutations.
- Note names can be ambiguous; prefer vault-relative paths when precision matters.
- `note patch` fails on multiple matches by design. Treat that as a safety guard, not a bug, and narrow the edit with `--section`, `--heading`, `--block-ref`, or `--lines`.

Useful command groups:

- Notes: `note outline`, `note get`, `note create`, `note set`, `note append`, `note patch`
- Querying: `search`, `query`, `ls`, `backlinks`, `links`, `graph ...`
- Refactors: `refactor rename-alias`, `rename-heading`, `rename-property`, `merge-tags`, `move`
- Web and git: `web search`, `web fetch`, `git status`, `git diff`, `git log`, `git commit`
- Periodic notes: `daily ...`, `periodic weekly`, `periodic monthly`, `periodic ...`

Documentation workflow:

- Read `.agents/skills/*/SKILL.md` for task-specific usage patterns.
- Read `.agents/tools/*/TOOL.md` when a vault-native custom tool exists for the workflow.
- Use `vulcan help <topic>` for integrated documentation.
- Use `vulcan describe --format openai-tools` or `--format mcp` to export machine-readable tool schemas.
- Run `vulcan agent install --overwrite` after upgrading Vulcan if the bundled harness files need a refresh.

Common pitfalls:

- Search is for note text. Query is for structured metadata.
- Skills teach workflows. Tools perform callable request/response work. Plugins react to events.
- Search JSON hits include `section_id` and `line_spans`; use them to follow a search hit with `note get` or `note patch` instead of reopening the full note.
- Property typing is lenient and may need validation through `doctor`.
- Some runtime-oriented JS APIs are still rolling out; prefer stable CLI commands when available.
