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
threads, files or network, so they are meant to compile unchanged for
WebAssembly. That has not been tested yet (see WebAssembly below).

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
- allstops reads from bytes only, which is what a WebAssembly build needs
  (not yet compiled for WebAssembly; see below).

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

The papers assume transitively closed footpaths; allstops does not need them
because the rules forbid chained walks (see `docs/ALGORITHMS.md`).

The `vulture` RAPTOR crate was not adopted: first published in May 2026 with
under 300 downloads, it pins an older `gtfs-structures` and returns only board
and alight stops, not the intermediate calls the visit tracking needs.

## Pack format

**Decision: postcard**, behind a pack header carrying a format version, the
feed SHA-256, the selection and rules hashes, the validity range and the
attribution text (Stage 1). `bincode` was excluded: the project is
discontinued, 3.0.0 is a tombstone release and RUSTSEC-2025-0141 marks it
unmaintained.

Measured with `cargo xtask pack` on the Munich network for 2026-11-12 (8,987
stations, 42,258 trips, 778,688 connections, 88,842 walk links), median of 5
after a warm-up, on a busy machine (load average 3.3 to 3.8):

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

The content matters more than the format: connections are 64% of the postcard
bytes and 95% of them belong to tram and bus connector trips; `trip_conns`
(16% of the pack) can be rebuilt at load time instead of shipped. Both are
Stage 1 work.

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

The core crates read only bytes and use no threads, files or network, but the
WebAssembly build has not been compiled yet: the Homebrew Rust toolchain on the
development machine ships only the host standard library and no `wasm-ld`, and
`rustup` cannot add targets to it. Adding a rustup-managed toolchain is a
prerequisite for Stage 4.

## Licences and attribution

- Code: MIT or Apache-2.0.
- Each feed's licence, attribution template and redistribution status is in
  `data/feeds.toml`; `NOTICE` lists the attribution for every registered feed.
  Every itinerary carries the rendered attribution in `feed.attribution`.
- MVV: CC BY 4.0 (see `docs/DATA.md`). Packs built from it are adapted
  material and may be redistributed with attribution and a note that they
  were changed.
- Dependencies are permissively licensed (MIT, Apache-2.0, Unlicense, BSD).
