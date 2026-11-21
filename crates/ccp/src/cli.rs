use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ccp", about = "CCP: a compact, GraphQL-style query protocol (demo)")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the demo CCP server over stdio
    Serve,
    /// Run the built-in demo query + byte-size comparison
    Demo,
}
