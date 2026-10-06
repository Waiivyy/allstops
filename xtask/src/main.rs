//! Project tasks: `cargo xtask <command>`.

mod parse;
mod pack;
mod real;
mod routing;
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
    /// Time Connection Scan against RAPTOR on the full network.
    Routing(routing::Args),
    /// Compare GTFS loaders on one feed: load time, peak memory, row counts.
    Parse(parse::Args),
    /// Compare pack formats (postcard, rkyv) on a real network.
    Pack(pack::Args),
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::BoundTuning(a) => tuning::run(a),
        Command::Routing(a) => routing::run(a),
        Command::Parse(a) => parse::run(a),
        Command::Pack(a) => pack::run(a),
    }
}
