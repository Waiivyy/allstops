# Data

Feeds are listed in `data/feeds.toml` with their licence, attribution text and
a pinned SHA-256. `allstops fetch <id>` downloads a feed into `data/cache/`
(not committed) and refuses a file whose hash differs from the pin. No raw
timetable data is committed to this repository; the only feed-derived file in
git is the list of Munich U-Bahn station IDs and names used by a snapshot test.

## MVV Munich (`mvv`)

| | |
|---|---|
| Publisher | Münchner Verkehrs- und Tarifverbund GmbH (MVV) |
| Download | https://www.mvv-muenchen.de/fileadmin/mediapool/developer/opendata/gesamt_gtfs.zip |
| Licence | CC BY 4.0. MVV's page says "cc-by" without a version; the national GovData catalogue lists this exact URL as CC BY 4.0. |
| Attribution | "Münchner Verkehrs- und Tarifverbund GmbH (MVV)", the retrieval date and the feed version |
| Pinned file | SHA-256 `13e9b6db…961b21c`, 17,880,036 bytes, retrieved 2026-10-06 |
| Feed version | `20261005` |
| Validity | 2026-10-01 to 2026-12-13 (`feed_info.txt`, matches the calendars) |

The publisher updates the feed irregularly, usually every four to eight weeks,
and each file covers about three months. A plan date must fall inside the
validity range; `allstops inspect --date` checks it.

### Profile

Measured with `allstops inspect data/cache/mvv.gtfs.zip` (load 643 ms, profile
198 ms on an Apple M5).

| | |
|---|---|
| Files | agency, calendar, calendar_dates, feed_info, routes, shapes, stop_times, stops, ticketing_deep_links, ticketing_identifiers, trips |
| Missing optional files | transfers, frequencies, pathways, levels |
| Empty files | `shapes.txt` has a header and no rows |
| Agencies | 98, one time zone (Europe/Berlin) |
| Routes by type | `0` (tram): 27 routes, 17,022 trips; `1` (subway): 9 routes, 12,476 trips; `2` (rail): 34 routes, 4,520 trips; `3` (bus): 829 routes, 81,207 trips. See the note on route types below. |
| Stops | 9,309 stations (`location_type = 1`) and 18,861 platforms, every platform with a `parent_station`; hierarchy depth 1 |
| ID scheme | German DHID (`de:<area>:<station>:<level>:<platform>`) for 99.7% of stops; a few Austrian IDs (`at:…`) for cross-border stops |
| Trips / stop_times | 115,225 / 2,217,866 |
| Services | 1,881 |
| Past midnight | 4,518 trips end after 24:00; latest time 30:08:00 |
| Pickup and drop-off | all rows allow both, except 10,216 with drop-off forbidden and 13,961 with pickup forbidden; no pass-through rows. All 192,167 U-Bahn rows allow both. |

Format details the loader handles: several files start with a UTF-8 byte-order
mark, and headers and values are quoted.

### Route types: the S-Bahn is coded as tram

The feed gives every Munich S-Bahn line (S1 to S8 and S20, operated by DB
Regio AG Bayern) `route_type = 0`, the GTFS code for tram. So `route_type 0`
holds 18 tram routes (11,230 trips) and 9 S-Bahn routes (5,792 trips).
Regional trains (RB and RE lines) are `route_type = 2`.

allstops follows the published route types, so the connector mode `"tram"`
also lets a runner ride the S-Bahn in Munich, and routes do use it (for
example S1 from Moosach to Feldmoching). The solver and the verifier agree,
because both read the same field. A per-feed override of route types is
planned for Stage 1 so that a selection can name the modes it means.

### Warnings

Only one warning class fires: **8 duplicate trips** (same route, service and
identical stop times). No orphan stops, no trips without service days, no
times going backwards, no unknown references.

### Stations

Clustering (`allstops stations`) groups every stop under its `parent_station`;
the DHID prefix rule never needs to merge anything in this feed. 9,309
stations. The clustering reports 31 pairs of same-named stations more than
1 km apart and 34 pairs of differently named stations under 50 m apart, for
review with overrides in Stage 1.

The Munich U-Bahn selection (`data/selections/mvv-ubahn.toml`, every station
with a scheduled `route_type = 1` stop) gives **96 stations**: 93 in Munich
(`de:09162:…`) and the three Garching stations (`de:09184:…`). The four
two-level interchanges are one parent station each with platforms on both
levels (Hauptbahnhof 9 platforms, Sendlinger Tor 12, Olympia-Einkaufszentrum 8,
Odeonsplatz 6). See `docs/RULES.md` for the 96 versus 100 discussion.

### U-Bahn routes in the feed

`U1` to `U8` plus a route named `U` with 274 trips: 272 between Goetheplatz and
Implerstraße (140 in one direction, 132 in the other) and 2 from
Brudermühlstraße via Implerstraße to Goetheplatz. That is the shuttle train
that runs during the 2026 renovation works at Poccistraße and Goetheplatz. Several lines serve more stations than
their regular route in this feed (U2 serves 36 stations, U8 30), consistent
with the construction diversions announced by the operator for October and
November 2026.

### Service on the plan dates checked

With the default rules (04:30 to 26:00, tram and bus as connectors):

| Date | Day | Network (trips / connections / stations / walk links) | Result |
|---|---|---|---|
| 2026-10-06 | Tue | 43,800 / 773,088 / 8,989 / 88,576 | infeasible: Neuperlach Süd, Therese-Giehse-Allee and Poccistraße have no U-Bahn service |
| 2026-10-10 | Sat | 30,690 / 538,891 / 6,636 / 70,616 | infeasible: same three stations |
| 2026-11-09 | Mon | 42,322 / 778,244 / 8,988 / 88,868 | greedy route found and verified |
| 2026-11-12 | Thu | 42,258 / 778,688 / 8,987 / 88,842 | greedy route found and verified |
| 2026-11-14 | Sat | 29,096 / 542,282 / 6,560 / 70,622 | greedy route found and verified |

The October gaps match the operator's announcements of construction closures
(U5 cut back with replacement buses to Neuperlach Süd, the Poccistraße
renovation). The feed shows Lehel as served in this period even though a U4
closure there was announced; the planner follows the feed.

### Shapes

`shapes.txt` is empty, so map lines are drawn as straight segments between
stops and labelled as such. MVV also offers `mvv_shape_gtfs.zip` (84 MB,
linked from the same page without a description), which may contain shapes;
it has not been evaluated.
