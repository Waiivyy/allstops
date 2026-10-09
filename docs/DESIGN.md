# Design

Decisions taken in Stage 0, with the measurements behind them. Numbers are
from an Apple M5 (10 cores), release builds, on the MVV feed pinned in
`data/feeds.toml` (feed version 20261005) unless stated. Anything not measured
is labelled as a judgement.

## Language and layout

**Rust, as a Cargo workspace**, as proposed. One engine compiles to the native
CLI and, from Stage 4, to WebAssembly for the browser, so the web planner and
the CLI cannot drift apart. Search runs over hundreds of thousands of
connections thousands of times per solve, which favours a compiled language
with predictable memory layout.

Alternatives considered:

- **TypeScript only.** One language and no WebAssembly toolchain. The search is
  the hot path, and a typed-array implementation would be slower and harder to
  keep deterministic across engines (judgement, not measured).
- **Go.** Comparable speed natively, but larger WebAssembly output and a weaker
  story for running in a Web Worker (judgement).
- **Python with OR-Tools.** Strong solvers, but no way to run in the browser,
  which the product needs.

Crates (`crates/`):

| Crate | Role | May depend on |
|---|---|---|
| `allstops-gtfs` | feed loading with limits, calendars, station clustering, selection; with the `network` feature, building the routing network for a plan date | `allstops-core` only through `network` |
| `allstops-core` | network model, Connection Scan, profile scans, plans, bounds, itinerary schema, rules | no I/O |
| `allstops-verify` | independent verifier | `allstops-gtfs` without `network` |
| `allstops-cli` | the `allstops` binary | all |
| `xtask` | benchmarks, synthetic networks, experiments | all |

The verifier's dependency tree contains no solver code: `allstops-gtfs` is
used without its `network` feature, and a test fails if that changes.
`allstops-core` and `allstops-gtfs` take bytes, never paths, and use no
threads, files or network, so they compile unchanged for WebAssembly (see
WebAssembly below).

## GTFS loading

**Decision: keep the project's own streaming loader** (`allstops-gtfs`) and do
not adopt the `gtfs-structures` crate. Measured with `cargo xtask parse` on the
same in-memory MVV bytes, each loader in a fresh child process, median of 3:

| Loader | Load time | Peak memory over baseline | Rows loaded |
|---|---|---|---|
| allstops-gtfs | 572 ms | 207 MiB | identical |
| gtfs-structures 0.50.1, raw tables only | 1073 ms | 513 MiB | identical |
| gtfs-structures 0.50.1, full `Gtfs` | 1263 ms | 742 MiB | identical |

The machine was busy during these runs (load average 6 to 9), so the absolute
times are rough; the ratios (about 2.2x faster, 3.6x less memory) held in all
three sets. The deciding reasons are not speed:

- `gtfs-structures` has no input limits: no cap on compressed or uncompressed
  size, row count or line length, and the zip crate it uses decompresses a
  deflate bomb to the end. allstops must accept user-supplied feeds in the
  browser.
- It rejects a whole feed for one dangling reference, silently keeps the last
  of duplicate IDs, and does not fill in omitted times.
- allstops reads from bytes only, which is what a WebAssembly build needs,
  and compiles for WebAssembly (see below).

Neither loader expands `frequencies.txt` yet; MVV has none. That is Stage 1
work.

## Routing algorithm

**Decision: Connection Scan**, with RAPTOR kept as an independent cross-check
of the board labels. Measured with `cargo xtask routing` on the full MVV
network for 2026-11-12 with every mode routable (9,192 stations, 791,649
connections, 43,424 trips, 89,210 walk links), 300 seeded queries from random
stations at departure times between 04:30 and 18:30, single-threaded:

| Query | Mean | p50 | p95 | Max |
|---|---|---|---|---|
| Connection Scan, one-to-all | 1.78 ms | 1.77 ms | 2.99 ms | 3.15 ms |
| RAPTOR, one-to-all (5,893 FIFO route patterns, built in 11 ms) | 1.29 ms | 1.43 ms | 1.54 ms | 1.78 ms |
| Connection Scan, stop at the first target visit (the greedy's query) | 0.14 ms | 0.10 ms | 0.41 ms | 0.77 ms |

Both engines produce identical earliest-board labels at every station for all
300 queries (and 900 of 900 over three seeds before the scan was optimised).
The Stage 0 target of 20 ms per earliest-arrival query is met by both.

RAPTOR is somewhat faster for a full one-to-all query. The scan wins where
this planner spends its time: the query that stops at the first unvisited
target, which RAPTOR would need target pruning and visit labels to answer. The
scan also already provides visit labels, three kinds of origin (at a station,
aboard a train, boarding a train), journey extraction and the backward profile
scan behind the timetable-aware bound. Before an exact pruning of walk
relaxations (walks start from a station only after its earliest alighting),
the full scan took 6.99 ms mean.

**Point-to-point and profile queries** (Stage 2), measured the same way on
2026-10-09 (load average about 5):

| Query | Mean | p50 | p95 | Max |
|---|---|---|---|---|
| Connection Scan to one target, with the journey | 0.19 ms | 0.15 ms | 0.47 ms | 0.92 ms |
| Profile: one backward scan for a destination (20 target stations) | 114 ms | 112 ms | n/a | 122 ms |
| Profile lookup: earliest visit from a station at a time | 0.11 µs | | | |

A profile holds a mean of 53 (ready time, earliest visit) pairs per station.
All 6,000 lookups (20 destinations, 300 origins and times each) equal a
forward scan from the same place and time. A profile pays off once a
destination is asked about more than about 600 times (114 ms against
0.19 ms per forward query), which is the solver's situation when it tries
many departure times between decision stations.

The papers assume transitively closed footpaths; allstops does not need them
because the rules forbid chained walks (see `docs/ALGORITHMS.md`).

The `vulture` RAPTOR crate was not adopted: first published in May 2026 with
under 300 downloads, it pins an older `gtfs-structures` and returns only board
and alight stops, not the intermediate calls the visit tracking needs.

## Pack format

**Decision: postcard**, compressed with deflate, behind a header carrying a
format version, the feed SHA-256, the selection, rules and override hashes,
the validity range and the attribution text. `bincode` was excluded: the
project is discontinued, 3.0.0 is a tombstone release and RUSTSEC-2025-0141
marks it unmaintained.

### Serialisation format (Stage 0)

Measured with `cargo xtask pack` on the Munich network for 2026-11-12 (8,987
stations, 42,258 trips, 778,688 connections, 88,842 walk links, the Stage 0
rules with tram and bus connectors), median of 5 after a warm-up, on a busy
machine (load average 3.3 to 3.8):

| Format | Bytes | Deflated (-6) | Encode | Validated access | Decode to `Network` | Deterministic |
|---|---|---|---|---|---|---|
| postcard 1.1.3 | 22,603,400 | 13,600,424 | 16.2 to 16.6 ms | n/a | 15.7 to 16.1 ms | yes, also after rebuilding from the feed |
| rkyv 0.8.18 | 35,861,472 | 15,073,288 | 7.7 to 9.4 ms | 0.8 ms | 4.5 to 4.6 ms | yes, also after rebuilding from the feed |
| serde_json (reference) | 120,166,645 | 19,719,933 | 81 to 85 ms | n/a | 134 to 138 ms | yes |

postcard is 37% smaller raw and 10.8% smaller after compression, has a
documented stable wire format since 1.0, needs no new types (the network
already derives serde) and is plain serde, so it has no alignment or
pointer-width concerns in WebAssembly. rkyv decodes about 3.5 times faster
natively, but its zero-copy access only pays off if the solver worked on the
archived types directly; it would need a 211-line mirror of the network types
or 11 more crates in the core. Its archive compatibility is tied to rkyv's
semver, and its CI does not test wasm32. Decode time in WebAssembly is not
measured yet (no wasm target installed).

The content mattered more than the format: connections were 64% of those
bytes, and 95% of them belonged to tram and bus connector trips.

### What a pack holds (Stage 1)

A pack is not a network for one date. The date and most rules (walking
speed, detour factor, maximum walk, transfer minimums, time window, start
station) change from one plan to the next, and a network bakes all of them
in. A pack holds what stays fixed: the part of the feed a selection can use
with its connector modes, the stations after clustering and overrides, the
targets and the walk overrides. A network for any date in the validity range,
under any rules that keep the pack's selection, overrides and connector
modes, is built from it in about 60 ms on the Munich pack, by the same code
that builds it from a zip.

How it is built (`allstops-gtfs/src/pack.rs`):

1. Keep the routes of the target and connector route types with their
   trips, stop times and frequencies, every stop of every station a kept
   trip calls at (and of their parents' stations), the transfers between
   kept stops, routes and trips, every service calendar (so the accepted plan
   dates are the feed's) and every walks entry. Rows are copied from the GTFS
   files field for field into a subset, and the normal loader reads the
   subset, so time interpolation, frequency expansion and every check apply
   exactly as for a zip. Transfer rows that name a route or trip outside the
   pack can match no change in the network, and the network builder skips
   them for a zip as well.
2. Store the loaded tables with postcard. Stop times, nearly all of the data,
   are split into deduplicated stop patterns (stops, sequence numbers, pickup
   and drop-off types) and timing patterns (times relative to the trip's first
   departure); each trip keeps one (stop pattern, timing pattern, first
   departure) triple. Deflate the result at level 9.
3. Write the header and a SHA-256 trailer, then read the pack back and refuse
   to finish unless it gives exactly the loaded subset.

Reading checks the magic bytes, the format version (a pack of any other
version is refused with a message to rebuild it), the checksum, the size of
the file and of the inflated body (256 MiB by default), the number of stop
times the patterns expand to (the same row limit a zip gets, checked before
anything is allocated), every index and time in the decoded tables, and that
the header's counts match the contents. A crafted pack ends in an error,
never a panic, and costs at most a small multiple of the body limit in
memory, comparable to what the row limits allow a zip. The same input gives the same bytes: fixed file order,
no timestamps, first-seen pattern order and a fixed compression level.

The hashes in the header are SHA-256 of the canonical JSON (fields in
declaration order) of the parsed selection, rules, station overrides and
walks; an override hash is absent when its file is absent or empty. `solve`
and `verify` accept rules with a pack only when these match and the rules
name no connector mode whose route types the pack lacks. Without `--rules`,
`verify` checks the itinerary's own rules against the pack the same way.
The feed's identity and attribution come from the `data/feeds.toml` found
from the feed file's location, so a pack does not depend on the directory
it is built from.

Measured on Munich (details in `docs/DATA.md`): 3,171,750 bytes, 17.7% of
the zip, loading in a median of 51.5 ms against 556 ms for the zip. Two
alternatives were measured and rejected: the first row is the Stage 0
measurement above, under the rules of that time; the second was measured on
an earlier build of this stage, at a load average of 15 to 18.

| Pack content | Bytes | Load |
|---|---|---|
| The network for one date (Stage 0 table above, postcard plus deflate) | 13,600,424 | 15.7 to 16.1 ms to decode (inflating not measured), but one pack per date and per rules |
| The subset as a GTFS zip (CSV rows, deflate level 6) | 18,045,310 | 533 ms, about the same as the full zip |
| The subset as loaded tables with stop and timing patterns (chosen) | 3,171,750 | 51.5 ms |

The pack lives in `allstops-gtfs`, not in `allstops-core` as first sketched,
because what it stores is feed tables and stations, which are that crate's
types; the core stays free of GTFS.

**The verifier and packs.** Checking against a pack reads the decoded tables,
the same structures the verifier reads from a zip, so the verifier's logic is
unchanged; the round trip in step 3 guards the decoding. The strongest check
stays available: an itinerary records the full feed's SHA-256, so it can
always be verified against the original zip with
`allstops verify <zip> <itinerary> --rules <rules>`.

## Itinerary format

Every route is written as a versioned JSON itinerary
(`allstops-itinerary/0`), specified by a JSON Schema in
`docs/schema/itinerary-0.schema.json`. It names trips and stops by their
GTFS IDs and service dates, so it can be checked against the raw feed by the
verifier, which reads it with its own types and shares no code with the
planner. Ride times are GTFS times of the ride's own service date, exactly
as in stop_times.txt; walk, wait and summary times count from the plan
date's service day. Both may pass 24:00:00. The embedded rules make a run
reproducible.

Tests validate planner output against the schema on 200 synthetic networks
and on a ride that stays aboard through a terminus, and check that the
schema rejects broken documents; the Munich itinerary for 2026-11-12 also
validates. Optional fields may be added within version 0 when older readers
can ignore them (`stay_aboard` was added this way, written only when true);
anything else needs version 1.

## Lower bound

Two bounds, both from relaxing the visiting order to a Hamiltonian path over
the targets and bounding that path with Held-Karp 1-trees (see
`docs/ALGORITHMS.md`): a static one on fastest travel times and a
timetable-aware one on profile gaps. Measured on Munich for 2026-11-12:

| Bound | Value | Time |
|---|---|---|
| static | 2:18:25 | about 0.1 s |
| profile | 2:46:30 | about 10 s (96 backward scans over 778,688 connections) |

The Held-Karp subgradient converges: six step rules from 840 to 11,415
iterations give identical values (`cargo xtask bound-tuning`). The remaining
gap is the relaxation, not the optimiser. Both bounds run in pure Rust with no
LP solver, so they also work in WebAssembly. An exact or LP-based bound on a
decision graph (Stage 3) is where `microlp` (pure Rust, integer variables,
WebAssembly) or `good_lp` with HiGHS (needs a C++ toolchain and cmake, no
WebAssembly) come in; that comparison is deferred until there is a decision
graph to solve.

## WebAssembly

`allstops-core`, `allstops-gtfs` and `allstops-verify` compile for
`wasm32-unknown-unknown` without changes or warnings. Checked with a
rustup-managed stable toolchain (Rust 1.99.0) next to the Homebrew one, which
cannot add targets:

```bash
PATH="$(brew --prefix rustup)/bin:$PATH" cargo +stable check --target wasm32-unknown-unknown --target-dir target/wasm-check -p allstops-core -p allstops-gtfs -p allstops-verify
```

Linking, running and measuring in a browser come with the bindings in
Stage 4.

## Licences and attribution

- Code: MIT or Apache-2.0.
- Each feed's licence, attribution template and redistribution status is in
  `data/feeds.toml`; `NOTICE` lists the attribution for every registered feed.
  Every itinerary carries the rendered attribution in `feed.attribution`.
- MVV: CC BY 4.0 (see `docs/DATA.md`). Packs built from it are adapted
  material and may be redistributed with attribution and a note that they
  were changed.
- Dependencies are permissively licensed. In the `allstops` binary's
  dependency tree (`cargo tree -e normal`) most crates are MIT or
  Apache-2.0; the exceptions are ISC, BSD-3-Clause and Apache-2.0 AND ISC
  (`ring`) in the HTTPS stack that `fetch` uses, Zlib (`zlib-rs`,
  decompression), Unicode-3.0 together with MIT or Apache-2.0
  (`unicode-ident`), and CDLA-Permissive-2.0 for the bundled CA root
  certificates (`webpki-roots`).
