# Rules

A run is planned and checked under a set of rules saved with every result
(`data/rules/*.toml`, embedded in each itinerary's `rules` field). This page
defines each option, states which values are assumptions, and compares the
options with the published rules of the London and Munich records.

Nothing here is legal or official guidance from any record keeper. The record
keeper's own guidelines for an attempt win over this page.

## Options

| Option | Default | Meaning |
|---|---|---|
| `mode` | `"stops"` | Every target station must be visited. (`"segments"`, riding every segment, is a stretch goal.) |
| `selection` | | Path to the selection file that defines the target stations (relative to the rules file). |
| `date` | | Plan date, `YYYY-MM-DD`. Must lie inside the feed's validity range. |
| `earliest_start` | `04:30` | Earliest first visit, as a clock time of the plan date's service day. |
| `latest_end` | `26:00` | Latest last visit; may pass 24:00 (26:00 is 02:00 the next morning). |
| `start` | `"any"` | `"any"`, or the ID of a target station where the first visit must be. |
| `end` | `"any"` | `"any"`, or the ID of the station of the last visit. The verifier checks it; the planner does not support it yet and refuses other values. |
| `allow_walking` | `true` | Whether walks between stations are allowed. |
| `walking_speed_kmh` | `4.5` | **Assumption.** Average walking speed. Not a measurement. |
| `walk_detour_factor` | `1.3` | **Assumption.** Ratio of walked distance to straight-line distance. |
| `max_walk_m` | `1200` | Longest straight-line distance a single walk may cover. |
| `connector_modes` | `["tram", "bus"]` | Other scheduled public transport allowed for moving between stations. Riding these never counts as a visit. |
| `min_transfer_s.same_station` | `60` | **Assumption.** Minimum time between arriving by train and departing on another train at the same station. At least 1 second. |
| `min_transfer_s.walk_link` | `120` | **Assumption.** A walk between two stations never takes less than this. At least 1 second. |
| `tight_transfer_s` | `120` | `allstops solve` flags every change with less time than this to spare beyond its minimum. |
| `count_pass_through` | `false` | Whether a scheduled pass-through (pickup and drop-off both forbidden) counts as a visit. |
| `stay_aboard_through_terminus` | `false` | Riding through a terminus when the train continues as a new trip (`block_id`). Not supported yet; must be `false`. |

The defaults are deliberately conservative. They are planning assumptions, not
measurements, and a real runner should check them against their own pace and
the stations on the route. Walk links are straight-line approximations unless
overridden with measured times (`walks.toml`, planned for Stage 1).

Rules are checked before any work starts, and every problem is reported at
once: the date must be `YYYY-MM-DD`, the window must end after it starts and
last at most 48 hours, walking speed must be above 0 and at most 30 km/h, the
detour factor between 1 and 5, `max_walk_m` between 0 and 10,000, transfer
minimums between 1 second and one day, and connector modes must be known.
Transfer minimums must be at least one second because a change or walk taking
no time would let connections at the same instant chain in an order the
search does not model.

## What counts as a visit

The planner, the verifier and every output use one definition:

1. A station is visited when the runner is aboard a trip of a **target mode**
   and that trip has a **scheduled stop** there, or when the runner **boards**
   such a trip there. The runner does not need to get off. The target modes are
   the route types of the routes that the selection's route filters
   (`route_types`, `agencies`, `route_short_names`) match; a selection that
   names stations only, without any route filter, is rejected because it does
   not say which trips count.
2. A scheduled stop is a `stop_times.txt` row that is not a pass-through. A row
   with `pickup_type = 1` and `drop_off_type = 1` is a pass-through and does
   not count unless `count_pass_through = true`.
3. Boarding requires `pickup_type != 1`; alighting requires
   `drop_off_type != 1`. Values 2 and 3 (phone the agency, coordinate with the
   driver) are treated as allowed.
4. Walking to, past or through a station is never a visit, and neither is
   riding a connector mode (tram, bus) through it.
5. Stations are clusters of GTFS stops (see `docs/DATA.md`): a station counts
   once, however many lines or platform levels it has.
6. The visited set is always recomputed from the legs of the itinerary.

## Movement rules

- **Same-station transfer:** after arriving by train, the runner can board
  another train at the same station no earlier than
  `arrival + min_transfer_s.same_station`. Staying aboard the same trip needs
  no transfer time.
- **Walks:** a walk goes from one station to another within `max_walk_m`
  straight-line distance and takes
  `max(ceil(distance * walk_detour_factor / walking_speed), min_transfer_s.walk_link)`
  seconds. After a walk the runner can board immediately.
- **No chained walks:** a walk can follow a ride or the start of the run, never
  another walk. Otherwise several short walks could add up to a walk longer
  than `max_walk_m`. A walk goes between two different stations that both
  have coordinates.
- **Waits** keep the runner at a station; nothing after a wait may start
  before it ends.
- **The last ride** ends at a stop where alighting is allowed. When the last
  target is reached aboard a train that does not let passengers off there,
  the ride continues to the next stop that does; the total time is unchanged.
- **transfers.txt** is applied as the GTFS reference defines it. A row naming
  a station applies to all of its stops; rows with route or trip IDs apply only
  to those routes and trips; when several rows apply to a change, the most
  specific one wins (both trips, then a trip and a route, one trip, both
  routes, one route, stops only; between equally specific rows, one naming the
  stops beats one naming their stations). A `transfer_type = 3` row forbids
  the change, also across a walk. A `transfer_type = 2` row requires its
  `min_transfer_time` between alighting and boarding, also across a walk,
  and never less than `min_transfer_s.same_station` for a change at one
  station. The verifier checks exactly this. The planner applies minimum times
  conservatively per station (the largest one in a station raises its change
  time; one between two stations lengthens their walk link), so its routes
  meet whichever row applies, and it refuses to plan when a row forbids a
  transfer inside the network, because it cannot yet tell stops apart within
  a station.
- **Time window:** the first and last target visits must lie inside
  `[earliest_start, latest_end]` of the plan date's service day. Trips of every
  service day that reaches into the window are included, shifted by the exact
  offset between service days in the feed's time zone.
- **Data the planner leaves out:** trips whose times go backwards along their
  stop sequence, and trips with more than 65,537 stops, are left out and
  counted in the build report, so the planner and the verifier never disagree
  about when a trip calls where.

## Total time

The planned time is measured from the **first target visit** to the **last
target visit**. A visit by boarding happens at the scheduled departure; a visit
aboard happens at the scheduled arrival. This is close to the Guinness timing
rule (clock starts when the doors close on the first train, stops when the
challengers set foot on the last platform) but not identical: the planner has
no model of door closing, platform exits or real delays.

## Published rules

### Guinness World Records: the London Underground record

Sources, opened on 2026-10-06:

- GWR record page *Fastest time to travel to all London Underground stations*
  (https://www.guinnessworldrecords.com/world-records/677236-fastest-time-to-travel-to-all-london-underground-stations):
  17 h 46 min 48 s by Robin Otter and Thomas Sheat, 10 August 2024. The page
  gives no station count and no route rules beyond an age requirement.
- GWR news article, 8 July 2024
  (https://www.guinnessworldrecords.com/news/2024/7/teenagers-smash-london-tube-challenge-record-with-nearly-two-hours-to-spare):
  272 stations; the Elizabeth line and the DLR are not part of the challenge;
  the train must stop at a station for it to count but challengers need not get
  off; no private transport may be used at any point.
- GWR does not publish its guidelines; they are sent once an application is
  accepted. The fullest GWR rule text found is a *Specific Guidelines Pack*
  dated 23 January 2024 for the San Francisco BART record, published by BART
  (https://bart.gov/sites/default/files/2024-07/GuinnessWorldRecord_applicationguidelines_BART.pdf).
  Its generic section on "fastest to visit all stations in an underground
  network" records says, in summary:
  - a visit means arriving at and/or departing from the station on a train in
    normal public service that stops there; passing through without stopping
    counts only if the station is temporarily closed;
  - separate stations that share a name each count; a station served by several
    lines needs to be visited on one line only;
  - lines and stations may be repeated; the clock never stops, breaks included;
  - transfers must be made on foot or by scheduled public transport; private
    vehicles, taxis, bicycles and similar are not allowed;
  - a terminal station closed for long-term repairs must still be visited, on
    foot or by scheduled surface transport;
  - timing runs from the doors closing on the first train until the challengers
    set foot on the platform of the last station;
  - evidence includes witnesses at start and finish, a timestamped photo at
    every station, a logbook, witness statements and video.

Whether the London pack uses the same wording as the BART pack is not
confirmed.

### Guinness World Records: the Munich U-Bahn record

- GWR record page *Fastest time to travel to all Munich U-Bahn (metro)
  stations*
  (https://www.guinnessworldrecords.com/world-records/423617-fastest-time-to-travel-to-all-munich-u-bahn-metro-stations):
  **4 h 19 min 21 s** by Lorenz Wünsch and Till Rasche, **21 April 2022**, from
  **Garching-Forschungszentrum** to **Messestadt Ost**. Still shown as current on
  2026-10-06. The page gives no station count and no Munich-specific rules.
- The city portal muenchen.de
  (https://www.muenchen.de/freizeit/rekorde-und-superlative-ueber-muenchen.html,
  modified 2024-06-02) reports the same 2022 record and an earlier Guinness time
  of 4 h 48 min 53 s by Adham Fisher in 2020.
- No newer Guinness record was found. A community run of 4:33:48 on
  2025-09-22 is listed by TransitRuns (https://www.transitruns.org/munich_u),
  which uses its own rules.
- Which station count the 2022 record used (96 or 100, see below) is not
  stated in any source found.

### The Munich station count: 96 or 100

Both numbers describe the same network. MVG's own figures (*MVG in Zahlen
2026*, figures as of 31 December 2025,
https://www.mvg.de/dam/jcr:bc1ca7c4-a1ea-4a3b-adc9-ebdb1da8bb0b/260624_MVG_Flyer_MVG_in_Zahlen_2026_Web-RGB.pdf)
give 100 U-Bahn stations and note that four crossing stations are counted
twice. They are the four stations with two separate platform levels for
different trunk lines: **Hauptbahnhof**, **Sendlinger Tor**, **Odeonsplatz** and
**Olympia-Einkaufszentrum** (also listed by the city's building department,
and by the German Wikipedia station list, which gives each level its own
code). Counting each station once gives 96. Single-level interchanges such as
Scheidplatz, Innsbrucker Ring and Münchner Freiheit count once either way.

The MVV feed models each of the four as one parent station with platforms on
both levels, and the Munich U-Bahn selection produces **96** stations
(snapshot: `crates/allstops-gtfs/tests/snapshots/mvv-ubahn-stations.tsv`). The
GWR rule that a station on several lines needs one visit supports 96. Under the
100 definition a route would also have to call at both levels of the four
stations; in practice that matters only where a level can be skipped, such as
the U1 level of Olympia-Einkaufszentrum, which is the U1 terminus.

## Which configuration matches

| Rule | GWR generic (London, presumably Munich) | allstops setting |
|---|---|---|
| What counts | train stops at the station, no need to alight | default visit definition |
| Pass-through | only for temporarily closed stations | `count_pass_through = false` |
| Station on several lines | one visit is enough | stations are clusters; one visit |
| Moving between stations | on foot or scheduled public transport | `allow_walking = true`, `connector_modes = ["tram", "bus", "rail"]` to allow every scheduled mode |
| Running | allowed by GWR | **not modelled**: the planner never requires running |
| Closed terminal station | must still be visited on foot or by surface transport | **not supported**: the planner reports the station as unserved and the plan as infeasible |
| Timing | doors closing on the first train to setting foot on the last platform | first visit to last visit (scheduled times) |

The Munich rules file `data/rules/mvv-ubahn.toml` allows tram, bus and rail
as connectors, so every scheduled mode can be used between stations, as the
GWR wording allows. In the MVV feed the S-Bahn lines are coded as tram (see
`docs/DATA.md`) and regional trains (RB, RE) as rail. The built-in default,
used when a rules file names no connector modes, is tram and bus.

## Differences between planned and achieved times

A planned time is not a record. It assumes every train runs exactly to the
published timetable, every transfer takes the assumed time, and the runner
walks at the assumed speed. Real runs are slower when trains run late and can
be faster where running between stations is allowed. Comparisons with records
in this project always say "planned versus achieved" and list these
differences.
