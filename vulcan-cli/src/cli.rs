use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum BasesCommand {
    Eval { file: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchMode {
    Keyword,
    Hybrid,
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum VectorsCommand {
    Index,
    Neighbors {
        query: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    Duplicates {
        #[arg(long, default_value_t = 0.95)]
        threshold: f32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum RepairCommand {
    Fts {
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum Command {
    Init,
    Rebuild {
        #[arg(long)]
        dry_run: bool,
    },
    Repair {
        #[command(subcommand)]
        command: RepairCommand,
    },
    Watch {
        #[arg(long, default_value_t = 250)]
        debounce_ms: u64,
    },
    Scan {
        #[arg(long)]
        full: bool,
    },
    Links {
        note: String,
    },
    Backlinks {
        note: String,
    },
    Search {
        query: String,
        #[arg(long, value_enum, default_value_t = SearchMode::Keyword)]
        mode: SearchMode,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long = "path-prefix")]
        path_prefix: Option<String>,
        #[arg(long = "has-property")]
        has_property: Option<String>,
        #[arg(long = "context-size", default_value_t = 18)]
        context_size: usize,
    },
    Notes {
        #[arg(long = "where")]
        filters: Vec<String>,
        #[arg(long)]
        sort: Option<String>,
        #[arg(long)]
        desc: bool,
    },
    Bases {
        #[command(subcommand)]
        command: BasesCommand,
    },
    Cluster {
        #[arg(long, default_value_t = 8)]
        clusters: usize,
    },
    Vectors {
        #[command(subcommand)]
        command: VectorsCommand,
    },
    Move {
        source: String,
        dest: String,
        #[arg(long)]
        dry_run: bool,
    },
    Doctor,
    Describe,
    Completions {
        shell: Shell,
    },
}

#[derive(Debug, Clone, Parser)]
#[command(
    author,
    version,
    about = "Headless CLI for Obsidian-style vaults and Markdown directories"
)]
pub struct Cli {
    #[arg(long, global = true, default_value = ".")]
    pub vault: PathBuf,

    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Human)]
    pub output: OutputFormat,

    #[arg(long, global = true, value_delimiter = ',')]
    pub fields: Option<Vec<String>>,

    #[arg(long, global = true)]
    pub provider: Option<String>,

    #[arg(long, global = true)]
    pub limit: Option<usize>,

    #[arg(long, global = true, default_value_t = 0)]
    pub offset: usize,

    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}
