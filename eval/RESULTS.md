# Benchmark results

Written by `cargo xtask bench`; rerun the command to refresh this file. Per-instance rows go to `eval/out/bench.json`, which is not committed.

| Run | |
|---|---|
| Command | `cargo xtask bench` |
| Date (UTC) | 2026-10-06 |
| Commit | `b0daf4f15da1` |
| Machine | Apple M5, 10 cores; every run single-threaded, release build |
| Load average (1 min) at start / end | 6.28 / 5.35 |
| Greedy starts | every target station at 12 start times, 10 min apart from earliest_start |

## Caveats

- Walking speed (4.5 km/h), walk detour factor (1.3) and transfer buffers (60 s to change at a station, at least 120 s for a walk link) are assumptions from the rules, not measurements.
- Planned times are plans from the published timetable, not records. Nobody has ridden these routes, and real trains run late.
- Synthetic networks are generated. They exercise the pipeline and the bounds; they say nothing about any real network.
- Times are wall-clock milliseconds on a machine that may have been running other work (see the load average); treat them as rough.
- Time to first verified route: the first greedy run that visits every target, its itinerary and the verifier. Time to best: every greedy run, then itinerary and verifier for the shortest. Neither includes building the network.

## Synthetic networks

Plan date 2026-11-12 (a Thursday), default rules. Tiny: 3 to 7 target stations, with the brute-force optimum. Small: 15 to 30 target stations, no optimum. Seeds count up from 0.

| | tiny | small |
|---|---|---|
| instances | 200 | 50 |
| target stations, mean | 5.0 | 22.6 |
| connections, mean | 1308 | 6042 |
| metro lines, mean | 1.67 | 3.78 |
| with a branch / ring / connector bus | 72 / 24 / 101 | 38 / 25 / 27 |
| with a line reached only on foot | 7 | 17 |
| greedy found a route | 200 / 200 (100.0%) | 50 / 50 (100.0%) |
| verifier passed the first and best route | 188 / 200 (94.0%) | 32 / 50 (64.0%) |
| greedy equals the optimum | 125 / 200 (62.5%) | n/a |
| gap greedy to optimum, mean / max | 8.9% / 77.3% | n/a |
| lower bound <= optimum (asserted) | 200 / 200 (100.0%) | n/a |
| gap greedy to lower bound, mean / max | 27.8% / 153.8% | 37.5% / 82.4% |
| time to first verified route, ms, median / max | 0.03 / 0.20 | 0.07 / 0.11 |
| time to best route, ms, median / max | 0.08 / 0.38 | 2.18 / 4.69 |
| brute-force optimum, ms, median / max | 10.3 / 1143 | n/a |
| transfers per route, mean | 0.83 | 5.36 |
| tight transfers (slack under 120 s), total | 92 | 120 |
| smallest transfer slack, s | 0 | 0 |
| walking per route, m, mean | 120 | 510 |

## Real networks

- Munich U-Bahn: feed sha256 `13e9b6db7681c3849f57bc8afb97c3182b9994dbb512dd6d242af0fe6961b21c`, feed version 20261005, parsed in 585 ms; rules `data/rules/mvv-ubahn.toml`.

| network | date | targets | connections | greedy runs with a route | best route | lower bound | gap | transfers | tight | min slack s | walk m | verified | build ms | solve ms | first route ms | best route ms | bounds ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Munich U-Bahn | 2026-11-09 | 96 | 778244 | 1152 / 1152 | 4:26:40 | 2:46:30 | 60.2% | 14 | 9 | 0 | 142 | yes | 74.4 | 1651 | 3.28 | 1652 | 10612 |
| Munich U-Bahn | 2026-11-12 | 96 | 778688 | 1152 / 1152 | 4:29:10 | 2:46:30 | 61.7% | 17 | 10 | 0 | 190 | yes | 73.2 | 1680 | 3.20 | 1682 | 10564 |
| Munich U-Bahn | 2026-11-14 | 96 | 542282 | 1152 / 1152 | 4:45:50 | 2:44:45 | 73.5% | 15 | 8 | 0 | 296 | yes | 53.4 | 1201 | 2.75 | 1203 | 6926 |

## Problems

Broken invariants and verifier rejections. The command exits with an error while any remain; every message is in `eval/out/bench.json`.

| problem | instances | first instances |
|---|---|---|
| verifier rejected the first route: STATIONS_MISMATCH | 24 | tiny seed 24, tiny seed 34, tiny seed 52, tiny seed 96, tiny seed 128, tiny seed 146, tiny seed 153, tiny seed 156, ... |
| verifier rejected the best route: STATIONS_MISMATCH | 12 | tiny seed 128, tiny seed 192, small seed 2, small seed 3, small seed 6, small seed 17, small seed 18, small seed 24, ... |
| verifier rejected the first route: WALK_CHAINED | 1 | tiny seed 186 |
