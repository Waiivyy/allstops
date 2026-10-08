# allstops

**Plan the fastest way to visit every station of a metro network, from any
GTFS timetable, and know how good the plan is.**

allstops computes timetable-feasible routes for the *station challenge*:
travelling to every station of a transit network as fast as possible. It is the
sport behind the London Tube Challenge, with recorded attempts for Munich,
Berlin, New York and many other networks. Every route is replayed against the
raw timetable by an independent verifier before it is shown, and every result
comes with a lower bound, a time that no route under the same rules can beat.

> **Status: early development. Stage 1 of 6 (data layer) is complete.** The
> command-line tool can fetch and profile a feed, build stations with manual
> corrections, write a compact network pack, plan a first route, verify it and
> bound it. Route optimisation, exports, the browser planner and the live run
> mode are on the [roadmap](#roadmap).

## Highlights

- **Any GTFS feed.** Feeds are loaded from memory with no network or file
  access in the core, so the same engine can later run in the browser.
- **Independently verified.** The verifier shares no code with the solver
  except feed loading (a test enforces it). It re-derives every boarding,
  transfer, walk and visit from the raw `stop_times.txt` rows and rejects the
  route with a coded violation if anything is off. Nothing unverified is
  shown.
- **Honest about quality.** Each result reports its lower bound and the gap.
  The word "optimal" is reserved for a gap of zero.
- **Safe with untrusted input.** Hard limits on archive size, decompressed
  size, entries, rows and line length stop zip bombs, and malformed data is
  reported as an error with the file and line. Fuzz-style tests push mutated
  feeds through the whole data pipeline to check that nothing crashes.
- **Reproducible.** Feeds are pinned by SHA-256, rules are saved with every
  result, ties are broken deterministically, and network packs build to the
  same bytes every time.
- **Compact packs.** `allstops pack` stores what plans for one network need in
  a versioned, checksummed file: 3.1 MB for Munich with every tram, bus and
  rail connector trip (the feed is 17.9 MB), loading in about 60 ms instead of
  about 640 ms.
- **Private.** No accounts, no telemetry. The CLI only goes online when you run
  `fetch`.

## Example: Munich U-Bahn

The Munich U-Bahn has 96 stations (MVG counts 100 by listing its four two-level
interchanges twice; see [docs/RULES.md](docs/RULES.md#the-munich-station-count-96-or-100)).

```console
$ allstops solve data/cache/mvv.gtfs.zip --rules data/rules/mvv-ubahn.toml --date 2026-11-12 --out route.json
feed loaded in 647 ms; network for 2026-11-12 built in 81 ms: 43424 trips, 791649 connections, 9192 stations, 89210 walk links, 96 targets
wrote route.json
Greedy route, 2026-11-12
  total time       4:29:10
  first visit      Oberwiesenfeld at 06:10
  last visit       Garching, Forschungszentrum at 10:39
  legs             18 rides, 2 walks
  changes          17 (10 tight: under 120 s to spare beyond the minimum change; least 0 s)
    tight: 72 s spare at Klinikum Großhadern after a walk
    tight: 80 s spare at Sendlinger Tor
    tight: 0 s spare at Neuperlach Süd
    tight: 80 s spare at Laimer Platz
    tight: 50 s spare at Hauptbahnhof (U, Tram)
    tight: 40 s spare at Olympia-Einkaufszentrum
    tight: 20 s spare at Moosach
    tight: 110 s spare at Hauptbahnhof (U, Tram)
    tight: 20 s spare at Mangfallplatz
    tight: 40 s spare at Dietlindenstraße
  lower bound      2:46:30 (gap 61.7%)
    static         2:16:25 (110 ms)
    profile        2:46:30 (10688 ms)
  greedy runs      1152 (1152 covered every target) in 330 ms
  verified against the raw timetable
Times are service-day clock times (may exceed 24:00).
Timetable data: Münchner Verkehrs- und Tarifverbund GmbH (MVV), CC BY 4.0, retrieved 2026-10-06, feed version 20261005
Planned from the published timetable. Real trains run late. Check official sources and ride safely.
```

The first steps of that route, from `route.json`:

| Step | Time | From | Line | Until | New stations | Visited |
|---|---|---|---|---|---|---|
| 1 | 06:10 | Oberwiesenfeld | U3 towards Fürstenried West | 06:41 Fürstenried West | +22 | 22 |
| 2 | 06:47 | Fürstenried West | 56 towards Schloss Blutenburg | 06:53 Max-Lebsche-Platz |  | 22 |
| 3 | 06:53 | Max-Lebsche-Platz | walk 142 m | 06:55 Klinikum Großhadern |  | 22 |
| 4 | 06:57 | Klinikum Großhadern | U6 towards Garching, Forschungszentrum | 07:12 Sendlinger Tor | +7 | 29 |
| 5 | 07:14 | Sendlinger Tor | U2 towards Messestadt Ost | 07:34 Messestadt Ost | +13 | 42 |
| 6 | 07:40 | Messestadt Ost | U2 towards Feldmoching | 07:50 Innsbrucker Ring |  | 42 |
| 7 | 07:54 | Innsbrucker Ring | U5 towards Neuperlach Süd | 08:02 Neuperlach Süd | +5 | 47 |
| 8 | 08:03 | Neuperlach Süd | U5 towards Laimer Platz | 08:29 Laimer Platz | +11 | 58 |

The full itinerary has 20 steps and ends at 10:39; every step lists the stations it ticks off.

<sub>Timetable data: Münchner Verkehrs- und Tarifverbund GmbH (MVV), CC BY 4.0,
feed version 20261005. Planned from the published timetable; real trains run
late.</sub>

## Quick start

Requires Rust 1.88 or newer (developed with 1.93).

```bash
git clone https://github.com/Waiivyy/allstops.git
cd allstops
cargo build --release
```

Then fetch the Munich feed, plan a route and check it independently:

```bash
./target/release/allstops fetch mvv
./target/release/allstops inspect data/cache/mvv.gtfs.zip
./target/release/allstops solve data/cache/mvv.gtfs.zip --rules data/rules/mvv-ubahn.toml --date 2026-11-12 --out route.json
./target/release/allstops verify data/cache/mvv.gtfs.zip route.json --rules data/rules/mvv-ubahn.toml
```

To plan several times, pack the network once and plan from the pack. It holds
every date of the feed and the rules it was built with; `--date` and `--rules`
still apply:

```bash
./target/release/allstops pack data/cache/mvv.gtfs.zip --rules data/rules/mvv-ubahn.toml
./target/release/allstops solve data/cache/mvv-ubahn.pack --date 2026-11-12 --out route.json
```

The plan date must lie inside the feed's validity range, which `inspect`
prints. MVV replaces its feed every few weeks. When the published file no
longer matches the pinned hash, `fetch` stops and shows the old and new
validity dates; rerun it with `--accept-new-hash` to adopt the new file and
choose a date inside its range.

## How it works

```mermaid
flowchart TD
    A[GTFS zip] --> B[Load with input limits]
    B --> C[Cluster stops into stations, apply overrides]
    C --> D[Select target stations]
    D --> E[Build the network for the plan date]
    D -.-> P[(Network pack)]
    P -.-> E
    E --> F[Search: Connection Scan and greedy]
    E --> K[Lower bounds]
    F --> G[Itinerary JSON]
    G --> H{Independent verifier}
    H -- passes --> I[Route, lower bound and gap]
    K --> I
    H -- rejects --> J[Error: nothing is shown]
```

1. **Stations.** GTFS stops are clustered into the stations a passenger would
   name, using `parent_station`, German DHID station IDs, and names within a
   distance; an overrides file can merge, split or rename stations. Every
   count and visit works on stations.
2. **Network.** Trips of every service day that reaches into the time window
   are placed on one time line, with exact offsets across daylight-saving
   changes. Walk links join nearby stations under the walking rules (or take
   measured times from a walks file), and `transfers.txt` minimums lengthen
   changes and walks where they apply. A network pack stores the part of the
   feed that the selection and its connector modes can use, so networks for
   any date are built without reading the zip again.
3. **Search.** A [Connection Scan](https://arxiv.org/abs/1703.05997) engine
   finds the earliest time each station can be visited, aboard a train that
   stops there or by boarding one. A greedy heuristic repeatedly travels to the
   unvisited target that can be visited first, from every target station and
   several start times, and keeps the shortest route.
4. **Verification.** The route is written as a versioned JSON itinerary that
   names trips and stops by their GTFS IDs, then checked by `allstops-verify`.
5. **Bounds.** Two relaxations of the visiting order, solved with Held-Karp
   1-trees: one on fastest static travel times, one on the least time from any
   visit of a station to the earliest reachable visit of another, computed with
   backward profile scans. Both are checked against an exhaustive search on
   small networks: 800 in every test run and 10,000 in a long run.

Details and proof sketches: [docs/ALGORITHMS.md](docs/ALGORITHMS.md).
Design decisions and the measurements behind them:
[docs/DESIGN.md](docs/DESIGN.md).

## Results

### Munich U-Bahn (96 stations)

Measured with `cargo xtask bench` (Apple M5, 10 cores; single-threaded release build; MVV feed version 20261005; default rules):

| Plan date | Best planned route | Lower bound | Gap | Verified | Changes | Walking |
|---|---|---|---|---|---|---|
| Mon 2026-11-09 | 4:26:40 | 2:46:30 | 60.2% | yes | 14 | 142 m |
| Thu 2026-11-12 | 4:29:10 | 2:46:30 | 61.7% | yes | 17 | 190 m |
| Sat 2026-11-14 | 4:45:50 | 2:44:45 | 73.5% | yes | 15 | 296 m |

These are first routes from the greedy heuristic, and the bound is still loose: it relaxes the order in which stations are visited and lets each pair of consecutive stations use its best-aligned connection of the day (see [docs/ALGORITHMS.md](docs/ALGORITHMS.md#lower-bounds)). Route optimisation and stronger bounds are Stage 3.

**Planned versus achieved.** The Guinness World Records time for visiting all Munich U-Bahn stations is 4 h 19 min 21 s, achieved by Lorenz Wünsch and Till Rasche on 21 April 2022 from Garching-Forschungszentrum to Messestadt Ost. The times above are plans on the November 2026 timetable under the default rules: scheduled times without delays, walking and never running, a 60-second minimum change, and trams, buses, the S-Bahn and regional trains as connectors. A plan is not a record, and the two numbers are not directly comparable.

### Synthetic networks

The benchmark also generates seeded networks with branches, rings, shared trunks, walk links and connector buses: 200 tiny ones (3 to 7 stations, solved exactly by exhaustive search) and 50 small ones (15 to 30 stations).

| | Tiny | Small |
|---|---|---|
| Routes passing the verifier | 200 / 200 | 50 / 50 |
| Greedy route equals the exact optimum | 125 / 200 | n/a |
| Lower bound at most the exact optimum | 200 / 200 | n/a |
| Mean gap, greedy to exact optimum | 8.9% | n/a |

Full tables, timings and caveats: [eval/RESULTS.md](eval/RESULTS.md).

## Commands

| Command | What it does |
|---|---|
| `allstops fetch <id>` | Download a registered feed over HTTPS and check its pinned SHA-256 |
| `allstops inspect <zip-or-pack>` | Feed profile (validity, structure, warnings, date coverage), or a pack's header |
| `allstops stations <zip>` | Cluster stops into stations, apply overrides, list a selection's targets |
| `allstops pack <zip> --rules <toml>` | Write a network pack and report its size, build time and load time |
| `allstops solve <zip-or-pack> --rules <toml>` | Plan a route, verify it, report the lower bound and gap (`--rules` is optional with a pack) |
| `allstops verify <zip-or-pack> <itinerary.json> --rules <toml>` | Check any itinerary against the timetable and the given rules |

Every command accepts `--json` for machine-readable output. Exit codes: `0`
success; `1` no feasible route, or an itinerary rejected by the verifier; `2`
usage, data or runtime error.

## Rules

A run is planned and checked under explicit rules, saved with every result.
The defaults for Munich (`data/rules/mvv-ubahn.toml`):

- A station counts as visited when a U-Bahn train you are on stops there, or
  when you board one there. You do not need to get off. Passing through without
  a scheduled stop does not count, and neither does walking past.
- Trams, buses, the S-Bahn and regional trains may be used to move between
  stations but never count as visits.
- Walks of up to 1,200 m straight-line distance, at an assumed 4.5 km/h with a
  detour factor of 1.3 and at least 2 minutes each; never two walks in a row.
- At least 60 seconds to change trains at a station. Changes with less than
  2 minutes to spare beyond that are flagged as tight.
- `transfers.txt` is honoured as the GTFS reference defines it: minimum times
  are kept, forbidden transfers are never used.
- Per network, an overrides file can correct the station list and a walks file
  can replace estimated walk times with measured ones or forbid a walk
  ([docs/RULES.md](docs/RULES.md#override-files)). Munich uses neither.
- Time runs from the first visit to the last, between 04:30 and 26:00 of the
  plan date's service day.

Walking speed, detour factor and transfer buffers are assumptions, not
measurements. [docs/RULES.md](docs/RULES.md) defines every option and compares
them with the published Guinness World Records guidelines.

## Testing and benchmarks

```bash
cargo test                                   # unit, property and mutation tests
cargo test --release -p allstops-gtfs -- --ignored   # Munich station snapshot and pack determinism
cargo xtask bench                            # synthetic and real networks, writes eval/RESULTS.md
```

- **Routing** is checked against a separate brute-force oracle (Dijkstra over
  explicit states) on random networks, including hops that take no time; a
  long run of 100,000 networks agrees, deliberately broken variants are caught,
  and an independent RAPTOR implementation gives identical labels.
- **The verifier** has mutation tests: a valid itinerary passes, and each
  single mutation (a departure too soon after an arrival, a wrong service date,
  a trip that does not call at the alighting stop, boarding where pickup is not
  allowed, a walk faster than the rules allow, a forbidden transfer, a missing
  station, overlapping legs) is rejected with the expected code.
- **Bounds** are asserted to be at most the exact optimum, found by exhaustive
  search, on every small synthetic network in the benchmark.
- **Packs** build to identical bytes twice, a pack of another format version
  is refused with a clear message, and a network built from a pack is
  byte-identical to one built from the feed, on test feeds and on Munich.
  Packs with crafted bodies and valid checksums end in an error, never a
  crash.
- **Untrusted input** tests cover truncated archives, zip bombs, archives with
  too many entries, oversized lines, invalid UTF-8, missing columns, malformed
  times, skipped calendar dates, mutated feeds and crafted packs (run in debug
  builds, so integer overflows would also fail).

Also available: `cargo xtask routing` (Connection Scan against RAPTOR),
`cargo xtask parse` (GTFS loaders), `cargo xtask pack` (serialisation formats
compared in Stage 0)
and `cargo xtask bound-tuning`.

## Project layout

| Path | Contents |
|---|---|
| `crates/allstops-gtfs` | Feed loading with limits, calendars, station clustering and overrides, selection, walks, network building, packs |
| `crates/allstops-core` | Network model, Connection Scan, profile scans, plans, lower bounds, itinerary schema, rules |
| `crates/allstops-verify` | The independent verifier |
| `crates/allstops-cli` | The `allstops` command-line tool |
| `xtask` | Benchmarks, synthetic network generator, experiments |
| `data` | Feed registry, rules, selections and overrides (no timetable data) |
| `docs` | Rules, data notes, algorithms, design decisions |
| `eval` | Benchmark specification and latest results |

## Roadmap

| Stage | Scope | Status |
|---|---|---|
| 0 | Data profiling, feasibility spike, benchmark harness, design, review | done |
| 1 | Data layer: overrides, footpaths with measured walks, deterministic network packs | done |
| 2 | Routing and verifier: profile queries, larger property suites against a time-expanded oracle | planned |
| 3 | Solver: corridor decomposition, stronger bounds, local search, robustness, replanning, exports | planned |
| 4 | Web planner: the engine in WebAssembly, map replay, prebuilt packs | planned |
| 5 | Live run mode: offline phone view, "missed it" replanning, more cities | planned |
| 6 | Releases, CI, documentation | planned |

## FAQ

**Is this the optimal route?** Not yet. The planner still uses a greedy
heuristic (search comes in Stage 3), and the reported gap to the lower bound
is large. A route is only called
optimal when its gap is zero.

**Why was my real run slower than the plan?** The plan assumes every train runs
exactly to the published timetable, that each change takes the assumed buffer
and that you walk at the assumed speed. Real trains run late, platforms can be
far apart, and crowds slow you down.

**Can I use my own city?** Any GTFS feed within the input limits loads and can
be inspected and clustered today. A city needs a selection file that names its
target mode (for example `route_types = [1]` for a metro) and a rules file.
Adding a feed to the registry needs its licence checked first; see
[docs/DATA.md](docs/DATA.md).

**Does it use live delays?** No. Plans come from the published timetable only.
Live data is a stretch goal.

## Related work

- Beckenbach, Borndörfer, Knoben, Kretz and Uetz,
  [*The S-Bahn Challenge in Berlin*](https://opus4.kobv.de/opus4-zib/frontdoor/index/index/docId/5379),
  ZIB-Report 15-13, 2015: a periodic space-time network turned into a
  travelling-salesman problem.
- E. Rappos, [*Tubechallenges: Can OR help break records?*](https://optimization-online.org/2005/04/1112/),
  2005: an integer-programming model of the London challenge.
- G. Laporte, *The Tube Challenge*, INFOR 52(1), 2014: formulations as
  travelling-salesman and postman problems.
- [vats5](https://github.com/marcia-pedals/vats5) plans exact or bounded routes
  from GTFS for several configured systems, including Munich.
- Many single-city projects exist, mostly for New York and London.

## Safety

Planned from the published timetable. Real trains run late. Check official
sources and ride safely. No route requires running. Follow station rules, do
not run on platforms, stairs or escalators, and travel with a valid ticket.

A planned time is not a record and does not predict one. Comparisons with
records here are always planned versus achieved.

## Data and attribution

No timetable data is stored in this repository. Feeds are downloaded from
their publishers and used under their licences, listed in
[data/feeds.toml](data/feeds.toml), [NOTICE](NOTICE) and
[docs/DATA.md](docs/DATA.md). Every itinerary carries its feed's attribution.

Munich: Timetable data: Münchner Verkehrs- und Tarifverbund GmbH (MVV), CC BY
4.0.

No operator or record-keeping organisation is affiliated with or endorses this
project.

## Licence

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT licence](LICENSE-MIT), at your option.
