mod check;
mod cmd_fetch;
mod cmd_inspect;
mod cmd_pack;
mod cmd_solve;
mod cmd_stations;
mod cmd_verify;
mod plan_input;
mod registry;
#[cfg(test)]
mod stay_aboard_tests;
mod style;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Plan the fastest timetable-feasible route that visits every station of a
/// transit network.
#[derive(Parser)]
#[command(name = "allstops", version, about)]
struct Cli {
    /// Print machine-readable JSON on stdout instead of text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download a registered feed over HTTPS and check it against its pinned hash.
    Fetch(cmd_fetch::Args),
    /// Print a profile of a GTFS zip, or the header of a network pack.
    Inspect(cmd_inspect::Args),
    /// Cluster stops into stations and optionally list a selection's targets.
    Stations(cmd_stations::Args),
    /// Write a network pack: the part of a feed that plans for one selection use.
    Pack(cmd_pack::Args),
    /// Plan a route that visits every target station.
    Solve(cmd_solve::Args),
    /// Check an itinerary against the raw feed with the independent verifier.
    Verify(cmd_verify::Args),
}

/// Exit codes: 0 success; 1 no feasible route or itinerary rejected by the
/// verifier; 2 usage, data or runtime errors.
pub enum Outcome {
    Ok,
    Rejected,
}

pub fn default_registry() -> PathBuf {
    PathBuf::from("data/feeds.toml")
}

pub fn default_cache() -> PathBuf {
    PathBuf::from("data/cache")
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let code = if e.use_stderr() { 2 } else { 0 };
            let _ = e.print();
            return ExitCode::from(code);
        }
    };
    let result = match cli.command {
        Command::Fetch(a) => cmd_fetch::run(a, cli.json),
        Command::Inspect(a) => cmd_inspect::run(a, cli.json),
        Command::Stations(a) => cmd_stations::run(a, cli.json),
        Command::Pack(a) => cmd_pack::run(a, cli.json),
        Command::Solve(a) => cmd_solve::run(a, cli.json),
        Command::Verify(a) => cmd_verify::run(a, cli.json),
    };
    match result {
        Ok(Outcome::Ok) => ExitCode::SUCCESS,
        Ok(Outcome::Rejected) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{} {e:#}", style::bad("error:"));
            ExitCode::from(2)
        }
    }
}
