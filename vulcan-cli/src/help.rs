use crate::commands::docs::CliArgDescribe;
use serde::Serialize;
use std::fmt::{Display, Formatter, Write as _};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HelpTopicKind {
    Overview,
    Command,
    Concept,
    Guide,
}

impl Display for HelpTopicKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overview => formatter.write_str("overview"),
            Self::Command => formatter.write_str("command"),
            Self::Concept => formatter.write_str("concept"),
            Self::Guide => formatter.write_str("guide"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HelpTopicReport {
    pub(crate) name: String,
    pub(crate) kind: HelpTopicKind,
    pub(crate) summary: String,
    pub(crate) body: String,
    pub(crate) options: Vec<CliArgDescribe>,
    pub(crate) subcommands: Vec<String>,
    pub(crate) related: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HelpSearchReport {
    pub(crate) keyword: String,
    pub(crate) matches: Vec<HelpSearchMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HelpSearchMatch {
    pub(crate) name: String,
    pub(crate) kind: HelpTopicKind,
    pub(crate) summary: String,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn help_overview() -> HelpTopicReport {
    let concept_names = builtin_help_topics()
        .into_iter()
        .map(|topic| topic.name)
        .collect::<Vec<_>>();

    // Grouped command reference. Each tuple is (group_header, [(command, description)]).
    let groups: &[(&str, &[(&str, &str)])] = &[
        (
            "Notes",
            &[
                (
                    "note get",
                    "Open a note, resolve its path, or print frontmatter",
                ),
                ("note set", "Replace a note's content from stdin or a file"),
                (
                    "note create",
                    "Create a new note with optional template and frontmatter",
                ),
                ("note append", "Append text to a note or under a heading"),
                (
                    "note patch",
                    "Find-and-replace inside a note with match-count guard",
                ),
                (
                    "note delete",
                    "Delete a note and report inbound links that would break",
                ),
                (
                    "note rename",
                    "Rename a note in place and rewrite inbound links",
                ),
                ("note open", "Open a note in $EDITOR"),
                ("note links", "List outgoing links for a note"),
                ("note backlinks", "List notes that link to a note"),
                (
                    "note diff",
                    "Show a note's changes since git HEAD or a checkpoint",
                ),
                ("inbox", "Append a quick capture entry to the inbox note"),
                (
                    "template",
                    "Create notes from templates or insert templates into existing notes",
                ),
            ],
        ),
        (
            "Query & Search",
            &[
                (
                    "query",
                    "Run the shared query surface: DSL, Dataview DQL, or `--where` shortcuts",
                ),
                ("search", "Full-text and semantic search across the vault"),
                ("tags", "List indexed tags and counts across matching notes"),
                (
                    "ls",
                    "List notes filtered by tags, properties, or a path prefix",
                ),
                (
                    "properties",
                    "List indexed property keys with usage counts and observed types",
                ),
                ("backlinks", "List notes that link to the given note"),
                ("links", "List outgoing links from the given note"),
            ],
        ),
        (
            "History & Git",
            &[
                (
                    "changes",
                    "Report note/link/property changes since a baseline",
                ),
                (
                    "git",
                    "Git status, log, diff, blame, and commit within the vault",
                ),
            ],
        ),
        (
            "Refactor",
            &[
                (
                    "refactor",
                    "Vault-wide rename, retag, rewrite, and suggest passes",
                ),
                ("move", "Move a note and rewrite inbound links"),
                (
                    "rename-property",
                    "Rename a frontmatter property key across notes",
                ),
            ],
        ),
        (
            "Tasks",
            &[
                ("tasks", "Create, list, complete, and track TaskNotes tasks"),
                ("kanban", "Inspect and move cards on Kanban boards"),
            ],
        ),
        (
            "Periodic Notes",
            &[
                ("today", "Open today's daily note"),
                (
                    "daily",
                    "Open today's daily note; append text, list notes, or export as ICS",
                ),
                (
                    "periodic",
                    "List, gap-check, and open periodic notes of any cadence",
                ),
            ],
        ),
        (
            "Obsidian Plugin Views",
            &[
                ("bases", "Evaluate and interact with .base files"),
                ("dataview", "Evaluate Dataview inline fields and blocks"),
            ],
        ),
        (
            "Graph & Analysis",
            &[
                (
                    "graph",
                    "Shortest paths, hub notes, orphans, and vault analytics",
                ),
                (
                    "suggest",
                    "Surface plain-text mentions that could become links",
                ),
                ("doctor", "Inspect the vault for broken or suspicious state"),
            ],
        ),
        (
            "Index Maintenance",
            &[
                ("index", "Scan, rebuild, repair, watch, and serve the cache"),
                (
                    "vectors",
                    "Embed, cluster, query, and maintain the vector index",
                ),
                ("cache", "Inspect and maintain the SQLite cache"),
                ("repair", "Repair derived indexes and cache structures"),
            ],
        ),
        (
            "Interactive",
            &[
                ("browse", "Open the note browser TUI with live previews"),
                (
                    "edit",
                    "Open a note in $EDITOR and refresh the cache afterwards",
                ),
                ("open", "Open a note in the Obsidian desktop app"),
            ],
        ),
        (
            "Scripting & Tools",
            &[
                (
                    "run",
                    "Execute JavaScript in the Vulcan runtime sandbox; interactive REPL",
                ),
                (
                    "web",
                    "Fetch URLs and run web searches via configured backends",
                ),
                (
                    "render",
                    "Render markdown from a file or stdin with the terminal renderer",
                ),
                (
                    "plugin",
                    "List, enable, disable, and manually run JS lifecycle plugins",
                ),
                ("tool", "List and run exposed skill command tools"),
                (
                    "skill",
                    "List and read bundled or vault-defined assistant skills on demand",
                ),
            ],
        ),
        (
            "Assistant & Protocols",
            &[(
                "mcp",
                "Start a protocol-native MCP server over stdio or Streamable HTTP",
            )],
        ),
        (
            "Automation & Export",
            &[
                (
                    "saved",
                    "List, show, and run persisted query/report definitions",
                ),
                (
                    "automation",
                    "Run saved reports, checks, and repairs for CI workflows",
                ),
                ("export", "Write static export artifacts from the cache"),
                (
                    "site",
                    "Build, diagnose, and preview static websites from vault profiles",
                ),
                (
                    "checkpoint",
                    "Create and list named cache-state checkpoints",
                ),
            ],
        ),
        (
            "Setup & Configuration",
            &[
                ("init", "Initialize .vulcan/ state for a vault"),
                (
                    "config",
                    "Import Obsidian plugin settings into .vulcan/config.toml",
                ),
                (
                    "agent",
                    "Install harness files, print runtime snippets, and import external agent assets",
                ),
                (
                    "trust",
                    "Manage vault trust for startup scripts and plugins",
                ),
            ],
        ),
        (
            "Help & Info",
            &[
                ("status", "Show vault, cache, git, and config overview"),
                (
                    "help",
                    "Browse integrated docs (this page); `help <topic>` for details",
                ),
                (
                    "describe",
                    "Export machine-readable CLI, OpenAI tools, or MCP schemas",
                ),
                ("completions", "Generate shell completion scripts"),
                ("version", "Print the Vulcan version"),
            ],
        ),
    ];

    let mut body = String::from(
        "Vulcan is a headless CLI for Obsidian-style markdown vaults. \
        It indexes your notes into a local SQLite cache and exposes them \
        through DQL queries, full-text search, refactors, and a JavaScript runtime.\n\n\
        **Common workflows:**\n\
        - Query notes: `vulcan query 'FROM notes WHERE status = \"open\"'`\n\
        - Search full-text: `vulcan search \"meeting notes\"`\n\
        - Daily note: `vulcan daily`\n\
        - Create a note: `vulcan note create Projects/new-idea.md`\n\
        - List plugins: `vulcan plugin list`\n\
        - Run JS REPL: `vulcan run`\n\n\
        Run `vulcan help <command>` for details on any command.\n\
        Run `vulcan help --search <keyword>` to search all help topics.\n\
        Concept guides: ",
    );
    body.push_str(&concept_names.join(", "));
    body.push_str(
        "\n\nFor external runtimes and tool integrations, `vulcan describe` exports the full \
CLI schema as `json-schema` and the curated agent tool registry as `openai-tools` or `mcp` definitions.\n\n---\n",
    );

    for (group, commands) in groups {
        let _ = writeln!(body, "\n## {group}\n");
        for (cmd, desc) in *commands {
            let _ = writeln!(body, "- `{cmd}` — {desc}");
        }
    }
    HelpTopicReport {
        name: "help".to_string(),
        kind: HelpTopicKind::Overview,
        summary: "Integrated documentation for commands and core concepts.".to_string(),
        body,
        options: Vec::new(),
        subcommands: Vec::new(),
        related: concept_names,
    }
}

fn static_help_topic(
    name: &str,
    kind: HelpTopicKind,
    summary: &str,
    body: &str,
    related: &[&str],
) -> HelpTopicReport {
    HelpTopicReport {
        name: name.to_string(),
        kind,
        summary: summary.to_string(),
        body: body.trim().to_string(),
        options: Vec::new(),
        subcommands: Vec::new(),
        related: related.iter().map(|item| (*item).to_string()).collect(),
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn builtin_help_topics() -> Vec<HelpTopicReport> {
    vec![
        static_help_topic(
            "assistant-integration",
            HelpTopicKind::Guide,
            "External runtime contract for AGENTS.md, skill discovery, permissions, and wrappers.",
            include_str!("../../docs/assistant/pi_integration.md"),
            &["agent", "skill", "describe", "help"],
        ),
        static_help_topic(
            "chatgpt-mcp",
            HelpTopicKind::Guide,
            "Private ChatGPT Developer Mode MCP setup for daily wiki workflows.",
            include_str!("../../docs/guide/chatgpt-mcp.md"),
            &["mcp", "permissions", "daily", "tasks", "agent"],
        ),
        static_help_topic(
            "getting-started",
            HelpTopicKind::Guide,
            "Quick orientation for the CLI and its main workflows.",
            include_str!("../../docs/guide/getting-started.md"),
            &["query", "search", "note get", "note create"],
        ),
        static_help_topic(
            "examples",
            HelpTopicKind::Guide,
            "Representative command patterns for common vault workflows.",
            include_str!("../../docs/examples/recipes.md"),
            &["filters", "query-dsl", "note get", "refactor"],
        ),
        static_help_topic(
            "filters",
            HelpTopicKind::Concept,
            "Typed `--where` filter grammar shared across query, search, and mutations.",
            include_str!("../../docs/guide/filters.md"),
            &["query", "search", "note update"],
        ),
        static_help_topic(
            "query-dsl",
            HelpTopicKind::Concept,
            "The shared query DSL used by `vulcan query` and related tooling.",
            include_str!("../../docs/guide/query-dsl.md"),
            &["query", "ls", "search"],
        ),
        static_help_topic(
            "scripting",
            HelpTopicKind::Concept,
            "Current scripting-oriented surfaces and the path to the standalone JS runtime.",
            include_str!("../../docs/guide/scripting.md"),
            &["sandbox", "js", "describe"],
        ),
        static_help_topic(
            "skill-commands",
            HelpTopicKind::Concept,
            "Declare executable, schema-validated commands inside skills and expose them through CLI, MCP, describe, and JS.",
            include_str!("../../docs/guide/skill_command.md"),
            &[
                "skill command",
                "commands",
                "scripts",
                "scripts/",
                "mcp",
                "tool",
                "tools",
                "schema",
                "json schema",
                "permissions",
                "sandbox",
                "skill run",
            ],
        ),
        static_help_topic(
            "custom-tools",
            HelpTopicKind::Guide,
            "Author, inspect, lint, test, and expose skill-backed custom tools.",
            include_str!("../../docs/assistant/custom_tools.md"),
            &[
                "tool",
                "tool init",
                "tool lint",
                "tool test",
                "tool compat",
                "tool run",
                "skill commands",
            ],
        ),
        static_help_topic(
            "static-sites",
            HelpTopicKind::Guide,
            "Profile-driven static publishing, diagnostics, and local preview workflow.",
            include_str!("../../docs/guide/static-sites.md"),
            &["site", "render", "note get", "export"],
        ),
        static_help_topic(
            "sandbox",
            HelpTopicKind::Concept,
            "Sandbox guarantees and execution limits for JavaScript-backed features.",
            include_str!("../../docs/guide/sandbox.md"),
            &["scripting", "js.vault", "web"],
        ),
        static_help_topic(
            "js",
            HelpTopicKind::Concept,
            "Overview of the JS runtime surface, including current and planned namespaces.",
            include_str!("../../docs/reference/js-api/index.md"),
            &["js.contract", "js.vault", "js.vault.graph", "js.vault.note"],
        ),
        static_help_topic(
            "js.contract",
            HelpTopicKind::Concept,
            "Versioned contract for Vulcan's JavaScript runtime namespaces.",
            include_str!("../../docs/reference/js-api/contract.md"),
            &["js", "js.vault", "js.tools", "js.skills", "js.host"],
        ),
        static_help_topic(
            "js.vault",
            HelpTopicKind::Concept,
            "Primary JS namespace for vault-oriented reads, queries, and periodic helpers.",
            include_str!("../../docs/reference/js-api/vault.md"),
            &["js", "js.vault.graph", "js.vault.note"],
        ),
        static_help_topic(
            "js.vault.graph",
            HelpTopicKind::Concept,
            "Planned graph traversal and relationship inspection surface for the JS runtime.",
            include_str!("../../docs/reference/js-api/graph.md"),
            &["js.vault", "graph", "graph path"],
        ),
        static_help_topic(
            "js.vault.note",
            HelpTopicKind::Concept,
            "Shape and usage guidance for the planned JS Note object.",
            include_str!("../../docs/reference/js-api/note-object.md"),
            &["js.vault", "note get", "query"],
        ),
        static_help_topic(
            "js.plugins",
            HelpTopicKind::Concept,
            "Lifecycle plugin registration, hook names, payloads, and trust requirements.",
            include_str!("../../docs/reference/js-api/plugins.md"),
            &["plugin", "run", "trust"],
        ),
        static_help_topic(
            "js.tools",
            HelpTopicKind::Concept,
            "Registry-backed skill command tool discovery, invocation, and runtime context.",
            include_str!("../../docs/reference/js-api/tools.md"),
            &["tool", "js.host", "automation-surfaces"],
        ),
        static_help_topic(
            "js.skills",
            HelpTopicKind::Concept,
            "Agent Skills-compatible command discovery and invocation from the JS runtime.",
            include_str!("../../docs/reference/js-api/skills.md"),
            &["skill", "skill-command", "js.tools", "tools.call"],
        ),
        static_help_topic(
            "js.host",
            HelpTopicKind::Concept,
            "Permission-gated host process execution from the JS runtime.",
            include_str!("../../docs/reference/js-api/host.md"),
            &["tool", "js.tools", "sandbox"],
        ),
        static_help_topic(
            "automation-surfaces",
            HelpTopicKind::Concept,
            "How skills, skill command tools, plugins, and `vulcan run` differ.",
            include_str!("../../docs/guide/automation-surfaces.md"),
            &["tool", "plugin", "scripting"],
        ),
        static_help_topic(
            "reports",
            HelpTopicKind::Concept,
            "Saved report definitions and the commands that create, run, and schedule them.",
            "\
# Vulcan Report System

A **saved report** is a persisted query or check stored as a YAML file in `.vulcan/reports/`.
Reports capture the parameters of a `search`, `query`, or `bases` command so they
can be re-run by name without repeating the flags. `saved create notes` is the
shortcut form for a note-property query built from `query --where/--sort`.

## Creating reports

  vulcan saved create search <name> --where <filter>  # full-text search report
  vulcan saved create notes  <name> --where <filter>  # property query report
  vulcan saved create bases  <name> <file>            # Bases view report

## Running reports

  vulcan saved run <name>              # run one report with full export options
  vulcan automation run <name>         # run one or more reports sequentially
  vulcan automation run --all          # run every report in .vulcan/reports
  vulcan automation run <name> --scan  # run reports + scan + health checks (CI)
  vulcan automation list               # list the saved reports automation can run

## Command roles

| Command            | Scan | Doctor | Exit codes | Best for        |
|--------------------|------|--------|------------|-----------------|
| `saved run`        | no   | no     | 0/1        | one-off runs    |
| `automation run`   | opt  | opt    | 0/1/2      | batches + CI    |
| `automation list`  | no   | no     | 0          | discoverability |

## Report file format

Reports are TOML files in `.vulcan/reports/<name>.toml`:

  kind: search
  filters: [\"status = done\"]
  description: completed notes

## Tip

Use `--fail-on-issues` with `automation run` to get exit code 2 when checks
complete but still report problems — useful for CI gates.",
            &["saved", "automation", "query"],
        ),
    ]
}

pub(crate) fn builtin_help_topic(name: &str) -> Option<HelpTopicReport> {
    builtin_help_topics()
        .into_iter()
        .find(|topic| topic.name.eq_ignore_ascii_case(name))
}
