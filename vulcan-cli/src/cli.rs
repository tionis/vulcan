use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    Init,
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
        #[arg(long)]
        tag: Option<String>,
        #[arg(long = "path-prefix")]
        path_prefix: Option<String>,
        #[arg(long = "context-size", default_value_t = 18)]
        context_size: usize,
    },
    Move {
        source: String,
        dest: String,
        #[arg(long)]
        dry_run: bool,
    },
    Doctor,
    Describe,
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
    pub limit: Option<usize>,

    #[arg(long, global = true, default_value_t = 0)]
    pub offset: usize,

    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}
