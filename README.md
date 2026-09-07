# Vulcan

**A local-first information hub that makes Markdown searchable, queryable, and programmable.**

Vulcan turns an Obsidian vault or plain Markdown directory into a knowledge base you can work with from the terminal, scripts, and agent tools. Search your notes, query structured records, follow links, manage tasks, make precise edits, and publish selected content—all while keeping ordinary files as the source of truth. Obsidian does not need to be installed or running.

> **Pre-alpha:** Vulcan is moving fast and contains a large amount of LLM-written code. Treat it as experimental, keep backups, and use Git or another versioning system before pointing it at important vaults.

[Getting started](docs/guide/getting-started.md) · [Installation](docs/installation.md) · [CLI reference](docs/cli.md) · [Architecture](docs/design_document.md) · [Roadmap](docs/ROADMAP.md)

## What you can do today

### Work with a knowledge base

Search note text with full-text and optional semantic search; explore backlinks, outgoing links, and the note graph; find unresolved links and other diagnostics. Query frontmatter, tags, inline fields, and file metadata using Vulcan's query language, Dataview DQL, or Obsidian Bases views.

Read and patch individual sections of long notes, rename files with link rewriting, and preview bulk property edits and refactors. Terminal browsers and interactive Bases views complement the scriptable commands.

Start with the [query guide](docs/guide/query-dsl.md), [filter reference](docs/guide/filters.md), and [workflow recipes](docs/examples/recipes.md).

### Use Markdown as a database

Keep tasks, projects, contacts, and other records in readable Markdown with structured metadata. Vulcan supports inline tasks, TaskNotes task files, recurring tasks, dependencies, Kanban boards, periodic notes, templates, and capture workflows.

The explicit `mdbase` commands add typed collection discovery, schema validation, record reads, CEL queries, and link semantics against a pinned mdbase v0.3 specification. Compatibility is scoped to tested profiles; collection writes and the optimized App backend remain planned. Ordinary vaults do not need mdbase schemas.

See the [CLI reference](docs/cli.md) for task and metadata workflows, and the [mdbase roadmap](docs/ROADMAP.md#mdb-mdbase-typed-markdown-collection-interoperability-formerly-932) for implemented profiles and remaining work.

### Automate with scripts and agents

Use the CLI directly from shell or Python scripts, request JSON output, or run JavaScript against Vulcan's vault API. The same local workflows support agent tool discovery, MCP clients, reusable skills, and typed skill commands. Direct CLI operation does not require a daemon.

| Surface | Best suited to |
| --- | --- |
| CLI with `--output json` | Shell scripts, CI, and programs invoking Vulcan |
| `vulcan run` | JavaScript scripts using the vault API |
| Skills and skill commands | Reusable agent guidance and typed callable workflows |
| Plugins | Event-driven lifecycle hooks |
| MCP | Tool clients using permission profiles and selected tool packs |
| Rust crates | Native integration with shared semantics and application workflows |

Read the [automation overview](docs/guide/automation-surfaces.md), [scripting guide](docs/guide/scripting.md), [JavaScript API](docs/reference/js-api/index.md), and [sandbox guide](docs/guide/sandbox.md). For remote agent access, see the [ChatGPT MCP setup guide](docs/guide/chatgpt-mcp.md).

### Publish and synchronize

Export selected notes as documents, datasets, books, or archives; build static sites; or publish to Outline. Outline integration includes explicit pull and publication workflows, exact note/document bindings, and named subtree routes with conflict handling.

Git-backed synchronization replicates vault files across devices. An optional multi-vault daemon schedules synchronization, and the Obsidian companion exposes status and sync controls. Device synchronization and external wiki publication have separate responsibilities: external documents pass through an inspectable local vault.

Follow the guides for [static sites](docs/guide/static-sites.md), [Outline publishing](docs/guide/outline-publishing.md), [Git synchronization](docs/guide/git-sync.md), and the [Obsidian companion](integrations/obsidian-vulcan/README.md).

## Quick start

Install a release using the [installation guide](docs/installation.md), which covers Linux, macOS, Windows, and Android/Termux, plus upgrades and optional daemon services. Place `vulcan` on your `PATH`. Git is a separate dependency for synchronization.

To build from this checkout instead, use the toolchain selected by `rust-toolchain.toml` (Rust 1.88 minimum):

```sh
cargo build --release --locked -p vulcan-cli --bin vulcan
export PATH="$PWD/target/release:$PATH"
```

Initialize an existing Markdown directory and build its local index:

```sh
vulcan --vault ~/notes index init
vulcan --vault ~/notes index scan
```

Search, query, and inspect it:

```sh
# Search note contents.
vulcan --vault ~/notes search 'meeting notes'

# Select records by metadata and return JSON for a script.
vulcan --vault ~/notes query \
  'from notes where status = "open" order by file.path asc limit 20' \
  --output json

# Browse interactively, or inspect indexing and link diagnostics.
vulcan --vault ~/notes browse
vulcan --vault ~/notes doctor
```

For an existing note, use its vault-relative path to read or preview an edit:

```sh
vulcan --vault ~/notes note get 'Projects/Alpha.md' --output json
vulcan --vault ~/notes note patch 'Projects/Alpha.md' \
  --find 'TODO' --replace 'DONE' --dry-run
```

For an external agent runtime or MCP client:

```sh
vulcan --vault ~/notes agent install
vulcan --vault ~/notes describe --format mcp
vulcan --vault ~/notes mcp --transport stdio --tool-pack notes-read,search,status
```

Explore commands with `vulcan --help` and topic guides with `vulcan help`. The [getting-started guide](docs/guide/getting-started.md) and [CLI reference](docs/cli.md) cover additional workflows and configuration prerequisites.

## How your data is stored

Vulcan follows three layers:

```text
Markdown, attachments, and collection definitions   ← canonical vault files
                      ↓ parse and index
              Rebuildable SQLite cache             ← metadata and link graph
                      ↓ derive
             Full-text and vector indexes          ← search and retrieval
```

You can edit the files with your existing editor and rebuild the cache from disk. `.obsidian/` is optional. Compatibility adapters interpret supported plugin formats and settings; they do not require the plugins to run or promise complete desktop behavior parity.

Vault-local configuration and state live under `.vulcan/`:

| Path | Purpose |
| --- | --- |
| `config.toml` | Shared vault configuration, usually committed with the vault |
| `config.local.toml` | Device-local overrides, ignored by default |
| `cache.db` | Rebuildable SQLite cache |
| `publish/` | Durable publisher identity mappings |
| `integrations/` | Durable import identities, reconciliation snapshots, conflict journals, and route state |

The cache is disposable; remote identities and recovery state are not. Keep durable workflow state when backing up or moving an integration. See the [configuration reference](docs/reference/config.md) and [information-hub guide](docs/guide/information-hub.md) for the boundaries. Supported Obsidian settings can be imported explicitly with `vulcan config import --all`.

## Where the project is going

The [roadmap](docs/ROADMAP.md) tracks implementation separately from design targets. Major directions include a web wiki, broader external knowledge connectors, and **Vulcan Apps**: installable applications with browser views, typed commands, and explicitly granted access to vault data.

For wiki-native structured data, the accepted direction is **one portable mdbase model with one Vulcan execution and mutation engine**. Markdown and mdbase definitions remain canonical; SQLite supplies rebuildable, indexed query projections. Equivalent mdbase and collection-bound native queries should share optimized execution, while managed writes should share schema validation, lifecycle rules, and Markdown persistence.

That integration is planned. The current mdbase query path still performs collection-wide work, and the documented latency targets are acceptance criteria, not measured guarantees. The target profile includes warm App reads below 50 ms at p95 and direct CLI queries below 100 ms at p95 under defined workloads. Standalone scripts remain useful independently of the App platform.

- [mdbase performance and native integration](docs/specs/mdb/PERFORMANCE_AND_NATIVE_INTEGRATION.md): execution strategy, shared writes, benchmark workloads, and readiness gates.
- [Vulcan App specification](docs/specs/vulcan-app/v1/SPEC.md) and [example App designs](docs/specs/vulcan-app/v1/EXAMPLE_APPS.md): planned package, API, storage, and application model.
- [Local information hub](docs/guide/information-hub.md): current Outline integration and planned routes to additional knowledge systems.
- [Performance notes](docs/performance.md): optimization evidence and remaining work.

## Documentation map

| If you want to… | Read |
| --- | --- |
| Install, upgrade, or run a service | [Installation](docs/installation.md) |
| Learn commands and selection syntax | [CLI reference](docs/cli.md), [queries](docs/guide/query-dsl.md), [filters](docs/guide/filters.md) |
| Write scripts or extend automation | [Scripting](docs/guide/scripting.md), [JS API](docs/reference/js-api/index.md), [custom tools](docs/assistant/custom_tools.md), [skill commands](docs/assistant/skill_commands.md) |
| Configure permissions and script access | [Configuration](docs/reference/config.md), [sandboxing](docs/guide/sandbox.md), [MCP setup](docs/guide/chatgpt-mcp.md) |
| Publish or exchange knowledge | [Static sites](docs/guide/static-sites.md), [Outline](docs/guide/outline-publishing.md), [external wikis](docs/guide/information-hub.md) |
| Sync devices and use Obsidian alongside Vulcan | [Git sync](docs/guide/git-sync.md), [companion plugin](integrations/obsidian-vulcan/README.md) |
| Understand or contribute to the implementation | [Design document](docs/design_document.md), [roadmap](docs/ROADMAP.md), [hardening and verification](docs/hardening.md) |

## Development

The Rust workspace separates reusable synchronous semantics and workflows from UI and async transports:

| Crate | Responsibility |
| --- | --- |
| `vulcan-core` | Parsing, indexing, SQLite cache, queries, graph, tasks, permissions, and compatibility semantics |
| `vulcan-app` | Reusable application workflows, including mutations, publication, configuration, and plugin dispatch |
| `vulcan-cli` | CLI, terminal UIs, output formatting, and MCP transports |
| `vulcan-daemon` | Async service, multi-vault registry, and background scheduling |
| `vulcan-sync` | Synchronous Git synchronization engine and backend boundaries |
| `vulcan-embed` | Embedding providers and vector-store abstractions |

The `vulcan-app` crate is the shared workflow layer used today; the planned **Vulcan Apps** platform is a separate product capability.

Run the required checks before committing:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace --no-default-features
```

See [hardening](docs/hardening.md) for feature combinations and architecture boundary checks.
