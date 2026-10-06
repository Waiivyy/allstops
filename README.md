# allstops

A speedrun planner for metro networks: it computes the fastest timetable-feasible route that visits every station of a transit network, checks that route independently against the raw GTFS timetable, and reports how far it can be from the best possible route.

**Status:** early development (Stage 0: data profiling and feasibility spike). Nothing here is usable yet.

## Install

Requires a recent stable Rust toolchain.

```bash
git clone https://github.com/Waiivyy/allstops.git
cd allstops
cargo build --release
```

## Run

Commands are being added stage by stage. See `cargo run -p allstops-cli -- --help` once the CLI crate exists.

## Safety

Planned from the published timetable. Real trains run late. Check official sources and ride safely.

## Licence

Code: MIT or Apache-2.0, at your option. Timetable data belongs to its publishers and is used under their licences; see `NOTICE`.
