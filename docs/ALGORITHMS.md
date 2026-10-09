# Algorithms

This page explains how allstops plans a route, written for a curious engineer.
It covers what exists after Stage 1; later stages extend it.

## The network for one plan

A plan works on one **network**, built for the plan date from the feed or
from a network pack, which holds the same tables (`allstops-gtfs/src/network.rs`,
`docs/DESIGN.md`):

- **Stations** are clusters of GTFS stops (see `docs/DATA.md`). Routing,
  visits and every output work on stations.
- **Connections** are single hops of a trip between two consecutive stops: a
  departure station and time, an arrival station and time, the trip, the hop's
  position in the trip and four flags: boarding allowed, alighting allowed, and
  whether the departure and the arrival count as visits. All connections are
  sorted by departure time.
- **Time** is one line: seconds after noon minus 12 hours of the plan date's
  service day, the GTFS definition. Trips of the previous and next service days
  are shifted onto this line with the exact offset between the service-day
  origins in the feed's time zone (25 or 23 hours across daylight-saving
  changes, 24 otherwise).
- **Walk links** join stations within `max_walk_m`; their duration already
  includes the walk-link minimum (`docs/RULES.md`). A walks file can replace a
  link's estimated time with a measured one or remove the link.

Only trips of the target mode and of the allowed connector modes enter the
network, and only those that overlap the time window. When the rules let
runners stay aboard through a terminus, a trip and the trip its vehicle runs
next (`docs/RULES.md`) become one network trip, joined by a hop that allows
neither boarding nor alighting; every algorithm below then handles staying
aboard without knowing about it, and the itinerary splits such a ride back
into its GTFS trips.

## Earliest arrival and earliest visit: Connection Scan

The routing engine is the Connection Scan Algorithm (Dibbelt, Pajor, Strasser,
Wagner, *Connection Scan Algorithm*, ACM Journal of Experimental Algorithmics
23, 2018; arXiv:1703.05997). It scans the sorted connections once. A trip
becomes reachable at the first connection whose departure can be boarded; every
later connection of a reachable trip is then reachable too. Each station keeps
two labels:

- **board**: the earliest time a departure from the station can be caught.
  Arriving by train adds the station's change time; arriving on foot does not;
- **visit**: the earliest time the station is visited, either aboard a visiting
  trip with a scheduled stop there or by boarding one there.

Searches can start at a station, aboard a train (just arrived at a stop) or
while boarding a train. A back-pointer per label rebuilds the journey: rides
(trip, first hop, last hop) and walks.

**Footpaths.** The CSA paper assumes transitively closed footpaths, so one
footpath edge always suffices. allstops does not close its footpaths, and does
not need to: the rules forbid chaining walks, so a single footpath after a ride
(or from the origin) is exactly the allowed movement. A walk relaxes only the
board label, never triggers another walk, and the arrival station of a walk is
not a visit.

**Early termination.** When only a set of stations matters (the unvisited
targets), the scan stops as soon as a connection departs after the best visit
found among them, since no later connection can improve it.

**Connections at the same instant.** Real timetables round to whole minutes,
so many hops take no time and several connections share one departure time
(the MVV feed has 72,732 bus hops of zero seconds). Connections are sorted by
departure, then arrival, trip and position, so a trip's own hops are scanned
in order. A change between two trips, or a walk, always takes at least one
second (the rules reject shorter minimums), so nothing that becomes possible
after an arrival can depart at that same instant, and the scan never misses
such a transfer. With zero-second changes this would not hold, which is why
the rules forbid them.

**Walk pruning.** A walk from a later alighting at a station reaches every
destination no earlier than a walk from an earlier one, so walks start from a
station only after its earliest alighting so far. This keeps every label and
cut a one-to-all scan of the MVV network from 6.99 ms to 1.78 ms on average.

**Correctness check.** `allstops-core/src/oracle.rs` computes the same visit
labels with a different method: Dijkstra over explicit states (ready at a
station, just alighted, at the origin, aboard a hop). A property test compares
the two on random networks with transfer times, walks, pickup and drop-off
restrictions, non-visiting connector trips, zero-second hops and all three
origin kinds. The default test run checks 512 networks; a long run with
`ALLSTOPS_PROPTEST_CASES=100000` also agrees on every one. Deliberately breaking
the change time or the pickup check makes the test fail. RAPTOR (Delling,
Pajor, Werneck, 2012) is implemented as a second, independent check of the
board labels.

## The first route: nearest unvisited target

`plan::greedy` starts at a station and time. Its first step visits that
station (a run from a station starts there), and every later step runs a scan
with early termination towards all unvisited targets, takes the journey to
whichever can be visited first, and repeats from where that journey ends,
staying aboard the train when that is faster. If the last target is reached
aboard a train that does not let passengers off there, the last ride goes on
to the next stop that does; the total time does not change. Consecutive rides on the same trip are merged into
one leg. The visited set is recomputed from the legs after every step, never
taken from search bookkeeping. The CLI runs the greedy from every target
station at 12 start times ten minutes apart, in parallel, and keeps the
shortest result, with ties broken by start time and then station index so the
result does not depend on thread scheduling.

## Lower bounds

A lower bound is a time that no feasible itinerary for the same targets and
rules can beat. Both bounds below come from the same relaxation.

**Relaxation.** Take any feasible itinerary and order the targets by the time
of their first visit: `v1, v2, ..., vn` at times `t1 <= t2 <= ... <= tn`. The
itinerary's total time is `tn - t1`, which equals the sum of the gaps
`t(k+1) - t(k)`. If every gap is at least some lower bound `g(v(k), v(k+1))`,
then the total is at least the length of the Hamiltonian path `v1 ... vn` under
`g`, and so at least the length of the **shortest** Hamiltonian path under `g`.

**Static gaps.** `g` is the shortest travel time in the static graph whose
edges are the fastest scheduled hop between two stations, over every trip in
the network, and every walk link. Waiting, dwell and change times count as
zero. Between the first visits of `v(k)` and `v(k+1)` the runner rides and walks
along edges of this graph, each taking at least its static weight, so the gap
is at least the shortest-path time.

**Profile gaps.** `g(i, j)` is the least time, over every moment at which `i`
can be visited (aboard a train stopping there, or boarding one), from that
visit to the earliest visit of `j` reachable from that moment. It is computed
for all pairs with one backward profile scan per destination `j`
(`allstops-core/src/profile.rs`, after the profile variant of Connection Scan
in the same paper): for every connection, the earliest visit of `j` for a
runner aboard it at its arrival, with a staircase of (departure, earliest
visit) pairs per station for runners ready to board. The true gap between
consecutive first visits starts from one of those moments, so it is at least
`g`. Transfer waits are included; only the visiting order is relaxed. Pairs
that can never follow each other get a large finite cost, which keeps the
relaxation valid because no feasible itinerary uses them.

**Solving the path relaxation.** The shortest Hamiltonian path is itself
NP-hard, so it is bounded from below with the Held-Karp 1-tree relaxation
(Held and Karp, *The traveling-salesman problem and minimum spanning trees*,
Operations Research 18, 1970, and part II, Mathematical Programming 1, 1971).
A dummy node joined at cost 0 to every target turns the path into a tour with
free ends. Costs are symmetrised with `min(g(i, j), g(j, i))`, which only
lowers them. For node penalties `pi`, the minimum 1-tree under costs
`c(i, j) + pi(i) + pi(j)` minus `2 * sum(pi)` is a lower bound on every tour;
subgradient steps on `pi` raise it. Every iterate is a valid bound and the best
is kept. Path costs are whole seconds, so the bound is rounded up.

**Checks.** On random synthetic instances small enough for an exhaustive
search (`oracle::optimum`, Dijkstra over states and visited sets), both bounds
are always at most the true optimum, and the greedy is never better than it:
400 instances by default and 5,000 in a long run (`ALLSTOPS_SYNTH_CASES`),
each also with zero-second hops. During review, the exhaustive optimum was
cross-checked against a separate brute force that enumerates rides, with the
same answer on about 42,000 feasible networks. Every reported result checks `bound <= found route` and fails
loudly otherwise.

**Why the bounds are still loose.** On the Munich U-Bahn the profile bound is
about 2 h 47 min against routes of about 4 h 30 min. The Held-Karp
optimisation has converged (different step rules give the same value), so the
looseness comes from the relaxation itself: each pair of consecutive targets
gets its best-aligned connection from anywhere in the day, while a real route
has to accept the transfer waits that its own timing produces. A stronger
bound needs the time-expanded structure, for example by decomposing the
network into corridors and decision stations (Stage 3).
