use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

const COMMAND_GROUPS_HELP: &str = "\
Command Groups:
  Indexing: init, scan, rebuild, repair, watch, serve
  Graph and Query: links, backlinks, graph, search, notes, bases, suggest
  Semantic: vectors, cluster, related
  Reports and Automation: saved, checkpoint, changes, batch
  Maintenance: move, doctor, cache, link-mentions, rewrite, rename-property, merge-tags, rename-alias, rename-heading, rename-block-ref, describe, completions";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    Csv,
    Jsonl,
}

#[derive(Debug, Clone, PartialEq, Eq, Args, Default)]
pub struct ExportArgs {
    #[arg(
        long,
        value_enum,
        requires = "export_path",
        help = "Write query rows to a CSV or JSONL file"
    )]
    pub export: Option<ExportFormat>,

    #[arg(
        long = "export-path",
        requires = "export",
        help = "Destination file for CSV or JSONL exports"
    )]
    pub export_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum BasesCommand {
    #[command(about = "Evaluate a .base file against the indexed vault state")]
    Eval {
        #[arg(help = "Vault-relative path to the .base file to evaluate")]
        file: String,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Open a read-only interactive TUI for a .base file")]
    Tui {
        #[arg(help = "Vault-relative path to the .base file to inspect")]
        file: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchMode {
    Keyword,
    Hybrid,
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum VectorQueueCommand {
    #[command(about = "Report pending vector indexing work")]
    Status,
    #[command(about = "Run the pending vector indexing queue")]
    Run {
        #[arg(long, help = "Report the pending queue without embedding chunks")]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum VectorsCommand {
    #[command(about = "Embed pending chunks and update the vector index")]
    Index {
        #[arg(long, help = "Report pending vector work without writing embeddings")]
        dry_run: bool,
    },
    #[command(about = "Repair stale, missing, or mismatched vector rows")]
    Repair {
        #[arg(
            long,
            help = "Report the repair scope without mutating the vector index"
        )]
        dry_run: bool,
    },
    #[command(about = "Rebuild the vector index from scratch")]
    Rebuild {
        #[arg(
            long,
            help = "Report the rebuild scope without mutating the vector index"
        )]
        dry_run: bool,
    },
    #[command(about = "Inspect or run the explicit vector indexing queue")]
    Queue {
        #[command(subcommand)]
        command: VectorQueueCommand,
    },
    #[command(about = "Find nearest indexed chunks for text or a note")]
    Neighbors {
        #[arg(help = "Ad hoc text query to embed and search")]
        query: Option<String>,
        #[arg(long, help = "Existing note identifier to use as the similarity query")]
        note: Option<String>,
    },
    #[command(about = "Recommend semantically related notes for one note")]
    Related {
        #[arg(help = "Note path, filename, or alias to use as the seed note")]
        note: String,
    },
    #[command(about = "Report highly similar chunk pairs from the vector index")]
    Duplicates {
        #[arg(
            long,
            default_value_t = 0.95,
            help = "Minimum cosine similarity threshold for duplicate candidates"
        )]
        threshold: f32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum RepairCommand {
    #[command(about = "Rebuild the full-text search index from cached chunks")]
    Fts {
        #[arg(long, help = "Report the repair scope without mutating the cache")]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum GraphCommand {
    #[command(about = "Find the shortest resolved-link path between two notes")]
    Path {
        #[arg(help = "Starting note path, filename, or alias")]
        from: String,
        #[arg(help = "Destination note path, filename, or alias")]
        to: String,
    },
    #[command(about = "List notes with the highest combined link degree")]
    Hubs,
    #[command(about = "Report candidate map-of-content style notes")]
    Moc,
    #[command(about = "List notes without outbound resolved note links")]
    DeadEnds,
    #[command(about = "Report weakly connected components of the note graph")]
    Components,
    #[command(about = "Summarize note-graph and vault analytics")]
    Stats,
    #[command(about = "Show note-count, orphan, stale, and link trends over saved scans")]
    Trends {
        #[arg(
            long,
            default_value_t = 10,
            help = "Maximum number of checkpoints to include"
        )]
        limit: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum CacheCommand {
    #[command(about = "Inspect cache sizes and row counts")]
    Inspect,
    #[command(about = "Verify cache invariants against derived indexes")]
    Verify,
    #[command(about = "Run SQLite VACUUM on the cache database")]
    Vacuum {
        #[arg(long, help = "Report the vacuum scope without mutating the cache")]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SuggestCommand {
    #[command(about = "Report plain-text note mentions that could become links")]
    Mentions {
        #[arg(help = "Optional note path, filename, or alias to inspect")]
        note: Option<String>,
    },
    #[command(about = "Report duplicate titles, alias collisions, and merge candidates")]
    Duplicates,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SavedCommand {
    #[command(about = "List saved query and report definitions")]
    List,
    #[command(about = "Show one saved query or report definition")]
    Show {
        #[arg(help = "Saved report name")]
        name: String,
    },
    #[command(about = "Persist a saved search definition in .vulcan/reports")]
    Search {
        #[arg(help = "Saved report name")]
        name: String,
        #[arg(help = "Full-text query string")]
        query: String,
        #[arg(
            long = "where",
            help = "Typed property filter, repeatable and combined with AND"
        )]
        filters: Vec<String>,
        #[arg(
            long,
            value_enum,
            default_value_t = SearchMode::Keyword,
            help = "Search strategy to store"
        )]
        mode: SearchMode,
        #[arg(long, help = "Restrict matches to notes carrying the given tag")]
        tag: Option<String>,
        #[arg(
            long = "path-prefix",
            help = "Restrict matches to paths under this prefix"
        )]
        path_prefix: Option<String>,
        #[arg(long = "has-property", help = "Require a property key to be present")]
        has_property: Option<String>,
        #[arg(
            long = "context-size",
            default_value_t = 18,
            help = "Approximate snippet context size for each search hit"
        )]
        context_size: usize,
        #[arg(long, help = "Persist the query string as raw FTS5 syntax")]
        raw_query: bool,
        #[arg(
            long,
            help = "Enable typo-tolerant fallback when the saved search runs"
        )]
        fuzzy: bool,
        #[arg(long, help = "Optional saved report description")]
        description: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Persist a saved property query definition in .vulcan/reports")]
    Notes {
        #[arg(help = "Saved report name")]
        name: String,
        #[arg(long = "where", help = "Filter expression, repeatable")]
        filters: Vec<String>,
        #[arg(long, help = "Property or field to sort by")]
        sort: Option<String>,
        #[arg(long, help = "Sort descending instead of ascending")]
        desc: bool,
        #[arg(long, help = "Optional saved report description")]
        description: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Persist a saved Bases evaluation definition in .vulcan/reports")]
    Bases {
        #[arg(help = "Saved report name")]
        name: String,
        #[arg(help = "Vault-relative path to the .base file to evaluate")]
        file: String,
        #[arg(long, help = "Optional saved report description")]
        description: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Run one saved query or report definition")]
    Run {
        #[arg(help = "Saved report name")]
        name: String,
        #[command(flatten)]
        export: ExportArgs,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum CheckpointCommand {
    #[command(about = "Create or replace a named checkpoint from the current cache state")]
    Create {
        #[arg(help = "Checkpoint name")]
        name: String,
    },
    #[command(about = "List saved scan and manual checkpoints")]
    List,
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum Command {
    #[command(about = "Initialize .vulcan/ state for a vault")]
    Init,
    #[command(about = "Rebuild the cache from disk")]
    Rebuild {
        #[arg(long, help = "Report rebuild scope without mutating the cache")]
        dry_run: bool,
    },
    #[command(about = "Repair derived indexes and cache structures")]
    Repair {
        #[command(subcommand)]
        command: RepairCommand,
    },
    #[command(about = "Watch the vault for filesystem changes and keep the cache fresh")]
    Watch {
        #[arg(
            long,
            default_value_t = 250,
            help = "Event coalescing window in milliseconds"
        )]
        debounce_ms: u64,
    },
    #[command(about = "Serve local cache-backed HTTP APIs for repeated queries")]
    Serve {
        #[arg(
            long,
            default_value = "127.0.0.1:3210",
            help = "Bind address for the local HTTP server"
        )]
        bind: String,
        #[arg(long, help = "Disable the background watcher refresh loop")]
        no_watch: bool,
        #[arg(
            long,
            default_value_t = 250,
            help = "Watcher debounce window in milliseconds when serve watch mode is enabled"
        )]
        debounce_ms: u64,
        #[arg(
            long,
            help = "Optional shared secret required in the X-Vulcan-Token header"
        )]
        auth_token: Option<String>,
    },
    #[command(about = "Scan the vault and update the cache")]
    Scan {
        #[arg(long, help = "Force a full scan instead of incremental reconciliation")]
        full: bool,
    },
    #[command(about = "List outgoing links for a note")]
    Links {
        #[arg(help = "Note path, filename, or alias to inspect")]
        note: String,
    },
    #[command(about = "List inbound links pointing at a note")]
    Backlinks {
        #[arg(help = "Note path, filename, or alias to inspect")]
        note: String,
    },
    #[command(about = "Analyze the resolved note graph")]
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },
    #[command(about = "Search indexed note content")]
    Search {
        #[arg(help = "Full-text query string")]
        query: String,
        #[arg(
            long = "where",
            help = "Typed property filter, repeatable and combined with AND"
        )]
        filters: Vec<String>,
        #[arg(
            long,
            value_enum,
            default_value_t = SearchMode::Keyword,
            help = "Search strategy to use"
        )]
        mode: SearchMode,
        #[arg(long, help = "Restrict matches to notes carrying the given tag")]
        tag: Option<String>,
        #[arg(
            long = "path-prefix",
            help = "Restrict matches to paths under this prefix"
        )]
        path_prefix: Option<String>,
        #[arg(long = "has-property", help = "Require a property key to be present")]
        has_property: Option<String>,
        #[arg(
            long = "context-size",
            default_value_t = 18,
            help = "Approximate snippet context size for each search hit"
        )]
        context_size: usize,
        #[arg(long, help = "Treat the query string as raw FTS5 syntax")]
        raw_query: bool,
        #[arg(long, help = "Retry empty searches with typo-tolerant term expansion")]
        fuzzy: bool,
        #[arg(long, help = "Include the parsed search plan and score details")]
        explain: bool,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Query notes by typed properties")]
    Notes {
        #[arg(long = "where", help = "Filter expression, repeatable")]
        filters: Vec<String>,
        #[arg(long, help = "Property or field to sort by")]
        sort: Option<String>,
        #[arg(long, help = "Sort descending instead of ascending")]
        desc: bool,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Evaluate read-only Bases views")]
    Bases {
        #[command(subcommand)]
        command: BasesCommand,
    },
    #[command(about = "Suggest link and merge opportunities from indexed notes")]
    Suggest {
        #[command(subcommand)]
        command: SuggestCommand,
    },
    #[command(about = "Persist and run saved reports from .vulcan/reports")]
    Saved {
        #[command(subcommand)]
        command: SavedCommand,
    },
    #[command(about = "Capture and inspect named cache-state checkpoints")]
    Checkpoint {
        #[command(subcommand)]
        command: CheckpointCommand,
    },
    #[command(about = "Report note, link, property, and embedding changes since a baseline")]
    Changes {
        #[arg(
            long,
            help = "Compare against a named checkpoint instead of the previous scan"
        )]
        checkpoint: Option<String>,
    },
    #[command(about = "Run multiple saved reports for automation and scheduled jobs")]
    Batch {
        #[arg(help = "Saved report names to run")]
        names: Vec<String>,
        #[arg(long, help = "Run every saved report definition in .vulcan/reports")]
        all: bool,
    },
    #[command(about = "Cluster indexed vectors into topical groups")]
    Cluster {
        #[arg(long, default_value_t = 8, help = "Requested cluster count")]
        clusters: usize,
        #[arg(long, help = "Report cluster assignments without persisting them")]
        dry_run: bool,
    },
    #[command(about = "Recommend semantically related notes for one note")]
    Related {
        #[arg(help = "Note path, filename, or alias to use as the seed note")]
        note: String,
    },
    #[command(about = "Run vector indexing and similarity commands")]
    Vectors {
        #[command(subcommand)]
        command: VectorsCommand,
    },
    #[command(about = "Move a note or attachment and safely rewrite inbound links")]
    Move {
        #[arg(help = "Existing source note or attachment path")]
        source: String,
        #[arg(help = "Destination note or attachment path")]
        dest: String,
        #[arg(long, help = "Report rewrite changes without moving files")]
        dry_run: bool,
    },
    #[command(about = "Convert unambiguous plain-text note mentions into links")]
    LinkMentions {
        #[arg(help = "Optional note path, filename, or alias to update")]
        note: Option<String>,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
    },
    #[command(about = "Apply a literal find/replace across notes selected by filters")]
    Rewrite {
        #[arg(long = "where", help = "Typed property filter, repeatable")]
        filters: Vec<String>,
        #[arg(long, help = "Literal text to find")]
        find: String,
        #[arg(long, help = "Replacement text")]
        replace: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
    },
    #[command(about = "Inspect the vault for broken or suspicious state")]
    Doctor {
        #[arg(long, help = "Apply deterministic local repairs")]
        fix: bool,
        #[arg(
            long,
            help = "Report planned repairs without mutating the vault or cache"
        )]
        dry_run: bool,
    },
    #[command(about = "Rename a frontmatter property key across notes")]
    RenameProperty {
        #[arg(help = "Existing property key")]
        old: String,
        #[arg(help = "Replacement property key")]
        new: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
    },
    #[command(about = "Merge one tag into another across frontmatter and note bodies")]
    MergeTags {
        #[arg(help = "Source tag to replace")]
        source: String,
        #[arg(help = "Destination tag to write")]
        dest: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
    },
    #[command(about = "Rename an alias inside one note's frontmatter")]
    RenameAlias {
        #[arg(help = "Note path, filename, or alias to update")]
        note: String,
        #[arg(help = "Existing alias text")]
        old: String,
        #[arg(help = "Replacement alias text")]
        new: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
    },
    #[command(about = "Rename a heading and rewrite inbound heading links")]
    RenameHeading {
        #[arg(help = "Note path, filename, or alias containing the heading")]
        note: String,
        #[arg(help = "Existing heading text")]
        old: String,
        #[arg(help = "Replacement heading text")]
        new: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
    },
    #[command(about = "Rename a block reference and rewrite inbound block links")]
    RenameBlockRef {
        #[arg(help = "Note path, filename, or alias containing the block reference")]
        note: String,
        #[arg(help = "Existing block reference id without the ^ prefix")]
        old: String,
        #[arg(help = "Replacement block reference id without the ^ prefix")]
        new: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
    },
    #[command(about = "Inspect and maintain the SQLite cache")]
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    #[command(about = "Describe the CLI schema and command surface")]
    Describe,
    #[command(about = "Generate shell completion scripts")]
    Completions {
        #[arg(help = "Shell to generate completions for")]
        shell: Shell,
    },
}

#[derive(Debug, Clone, Parser)]
#[command(
    author,
    version,
    about = "Headless CLI for Obsidian-style vaults and Markdown directories",
    long_about = None,
    after_help = COMMAND_GROUPS_HELP
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        default_value = ".",
        help = "Vault root directory"
    )]
    pub vault: PathBuf,

    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = OutputFormat::Human,
        help = "Output format"
    )]
    pub output: OutputFormat,

    #[arg(
        long,
        global = true,
        value_delimiter = ',',
        help = "Comma-separated field selection for list output"
    )]
    pub fields: Option<Vec<String>>,

    #[arg(
        long,
        global = true,
        help = "Embedding provider override for vector commands"
    )]
    pub provider: Option<String>,

    #[arg(long, global = true, help = "Maximum number of rows to return")]
    pub limit: Option<usize>,

    #[arg(
        long,
        global = true,
        default_value_t = 0,
        help = "Row offset for pagination"
    )]
    pub offset: usize,

    #[arg(
        long,
        global = true,
        action = ArgAction::SetTrue,
        help = "Enable extra diagnostic output"
    )]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}
