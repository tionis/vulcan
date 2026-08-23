# Getting Started

Vulcan works against an Obsidian vault or any plain Markdown directory. Start by initializing `.vulcan/` state, then scan the vault into the rebuildable SQLite cache:

```sh
vulcan --vault ~/notes index init
vulcan --vault ~/notes index scan
```

Core workflows:

- `vulcan search <query>` for content-oriented lookup.
- `vulcan query '<dsl>'` for metadata and graph-aware filtering.
- `vulcan note get|create|append|patch` for precise note edits.
- `vulcan tasks list|create|complete|reschedule` for inline tasks and TaskNotes workflows.
- `vulcan export ...` for publication output; `markdown`, `json`, `epub`, and `zip` can apply publication transforms without modifying source notes.
- `vulcan export outline-zip ...` and `vulcan publish outline ...` for one-way Outline export and publication.
- `vulcan site build` for static site output from configured site profiles.
- `vulcan doctor` to surface unresolved links and parser diagnostics.
- `vulcan mcp ...`, `vulcan agent install`, `vulcan describe`, and `vulcan skill ...` for external assistant runtimes, MCP clients, and workflow skills.

Automation conventions:

- Prefer `--output json` for scripts and external harnesses.
- Use `--dry-run` before bulk or destructive mutations.
- Note names may be ambiguous; pass a full relative path when precision matters.
- When you need repeatable public exports, prefer `export profile create` for the profile-wide settings and `export profile rule ...` for the ordered transform rules stored in `.vulcan/config.toml`.
- For ChatGPT or another remote MCP client, expose Vulcan behind HTTPS with OAuth/IndieAuth and a narrow permission profile. Do not publish a no-auth private vault endpoint.

See also: `vulcan help examples`, `vulcan help filters`, `vulcan help query-dsl`, `vulcan help assistant-integration`, `vulcan export --help`, and [ChatGPT MCP setup](./chatgpt-mcp.md).

## Local information hub direction

Today, Vulcan already provides publication/export workflows and one-way Outline reconciliation while keeping Markdown canonical. The roadmap generalizes this into a local hub with two deliberately separate mechanisms:

- device and file-tree sync backends replicate the vault across devices or storage providers;
- external knowledge routes pull selected remote documents into the vault or publish selected local notes through system-specific connectors.

The generic route and binding commands are planned, not yet available. See [Local information hub and external knowledge routes](./information-hub.md) for the supported boundary, safety rules, and first connector wave.
