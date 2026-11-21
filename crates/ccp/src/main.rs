mod cli;
mod demo;
mod schema;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    match Cli::parse().command {
        Command::Serve => demo::serve(),
        Command::Demo => demo::run_demo(),
    }
}
