# allstops

Plans the fastest timetable-feasible route that visits every station of a
transit network, from any GTFS feed. Every route is checked by an independent
verifier against the raw timetable before it is shown, and every result comes
with a lower bound, so you can see how far it could be from the best possible
route.

**Status:** early development (Stage 0: data profiling and feasibility spike).
The command-line tool can fetch and inspect a feed, cluster stops into
stations, plan a first route with a greedy heuristic, verify it and report a
lower bound. The web planner and live run mode come later.

## Install

Requires a recent stable Rust toolchain (tested with 1.93).

```bash
git clone https://github.com/Waiivyy/allstops.git
cd allstops
cargo build --release
```

The binary is `target/release/allstops`.

## Quick start (Munich U-Bahn)

```bash
allstops fetch mvv
allstops solve data/cache/mvv.gtfs.zip --rules data/rules/mvv-ubahn.toml --date 2026-11-12 --out route.json
allstops verify data/cache/mvv.gtfs.zip route.json --selection data/selections/mvv-ubahn.toml
```

`fetch` downloads the MVV feed and checks it against the pinned SHA-256 in
`data/feeds.toml`. The plan date must lie inside the feed's validity range
(`allstops inspect data/cache/mvv.gtfs.zip` prints it).

## Commands

| Command | What it does |
|---|---|
| `fetch <id>` | Download a registered feed over HTTPS and check its pinned hash |
| `inspect <zip>` | Feed profile: validity, structure, warnings, date coverage |
| `stations <zip>` | Cluster stops into stations; list a selection's targets |
| `solve <zip> --rules <toml>` | Plan a route, verify it, report the lower bound and gap |
| `verify <zip> <itinerary.json> --selection <toml>` | Check any itinerary against the raw feed |

Every command accepts `--json`. Exit codes: 0 success, 1 no feasible route or
an itinerary rejected by the verifier, 2 usage, data or runtime errors.

## Rules

What counts as a visit, how transfers and walks work, and how the defaults
compare with the published record rules: see [docs/RULES.md](docs/RULES.md).
Walking speed, detour factor and transfer buffers are assumptions, not
measurements.

## Safety

Planned from the published timetable. Real trains run late. Check official
sources and ride safely. No route requires running; follow station rules, do
not run on platforms, stairs or escalators, and travel with a valid ticket.

A planned time is not a record. Comparisons with published records are
"planned versus achieved" and list the differences in rules and data.

## Data and attribution

No timetable data is stored in this repository. Feeds are downloaded from
their publishers and used under their licences; see [NOTICE](NOTICE) and
[docs/DATA.md](docs/DATA.md).

Munich: Timetable data: Münchner Verkehrs- und Tarifverbund GmbH (MVV), CC BY
4.0.

## Privacy

The command-line tool makes network requests only when you run `fetch`. It
collects no telemetry.

## Licence

Code: MIT or Apache-2.0, at your option.
