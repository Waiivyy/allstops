//! Project tasks: `cargo xtask <command>`.

mod real;
mod tuning;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compare Held-Karp step rules on a real network's distance matrices.
    BoundTuning(tuning::Args),
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::BoundTuning(a) => tuning::run(a),
    }
}
