//! Seeded synthetic metro networks, written as GTFS zips in memory.
//!
//! Lines are laid out on a plane around central Munich: a trunk line
//! through the centre; optionally a branch line that shares the start of the
//! trunk and then splits off; crossing lines that meet an existing line at an
//! interchange station or pass within walking distance of one; a ring line
//! whose trips run a full loop; and a connector bus between two metro
//! stations through stops no metro line serves. Every metro station is a
//! parent row (location_type 1) with one platform per line. Trips run both
//! ways over a whole service day on a weekday calendar, every 3 to 20
//! minutes.
//!
//! The same seed and size give the same bytes. Positions are rounded to
//! whole metres as they are made, so platform differences in the last bit
//! of `sin` and `cos` almost never show up in the output.

use std::f64::consts::{PI, TAU};
use std::fmt::Write as _;

use allstops_core::builder::random::Lcg;
use allstops_gtfs::cluster::distance_m;
use allstops_gtfs::fixture::zip_files;
use serde::Serialize;

/// The plan date the synthetic calendars serve, a Thursday.
pub const PLAN_DATE: &str = "2026-11-12";

/// Central Munich, the origin of the plane.
const CENTRE: (f64, f64) = (48.137, 11.575);
const M_PER_DEG: f64 = 111_320.0;
/// New stations keep at least this distance from existing ones when a few
/// tries allow it.
const MIN_SEPARATION_M: f64 = 300.0;
/// Walk links join stations up to this far apart under the default rules.
const WALK_M: f64 = 1200.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    /// 3 to 7 target stations: small enough for the brute-force optimum.
    Tiny,
    /// 15 to 30 target stations.
    Small,
}

impl Size {
    pub fn name(self) -> &'static str {
        match self {
            Size::Tiny => "tiny",
            Size::Small => "small",
        }
    }

    /// Inclusive range of metro (target) station counts.
    pub fn targets(self) -> (u64, u64) {
        match self {
            Size::Tiny => (3, 7),
            Size::Small => (15, 30),
        }
    }

    /// Mixed into the seed so a tiny and a small network with the same seed
    /// do not share their random draws ("tiny" and "small" in ASCII).
    fn salt(self) -> u64 {
        match self {
            Size::Tiny => 0x7469_6e79,
            Size::Small => 0x0073_6d61_6c6c,
        }
    }
}

/// What a generated network contains.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Shape {
    pub metro_stations: usize,
    pub bus_only_stops: usize,
    pub metro_lines: usize,
    pub branch: bool,
    pub ring: bool,
    pub bus: bool,
    /// Crossing lines that meet the rest of the network only by a walk.
    pub near_misses: usize,
    /// Stations served by two or more metro lines.
    pub interchanges: usize,
    /// Pairs of metro stations close enough for a walk link.
    pub walk_pairs: usize,
    pub trips: usize,
}

pub struct Synthetic {
    pub shape: Shape,
    pub zip: Vec<u8>,
}

/// Metres east and north of the centre.
#[derive(Debug, Clone, Copy)]
struct Pt {
    x: f64,
    y: f64,
}

impl Pt {
    const ORIGIN: Pt = Pt { x: 0.0, y: 0.0 };

    fn dist(self, o: Pt) -> f64 {
        (self.x - o.x).hypot(self.y - o.y)
    }

    fn step(self, heading: f64, d: f64) -> Pt {
        Pt {
            x: self.x + d * heading.cos(),
            y: self.y + d * heading.sin(),
        }
    }

    fn towards(self, o: Pt) -> f64 {
        (o.y - self.y).atan2(o.x - self.x)
    }

    fn lerp(self, o: Pt, f: f64) -> Pt {
        Pt {
            x: self.x + (o.x - self.x) * f,
            y: self.y + (o.y - self.y) * f,
        }
    }

    /// Rounded to the six decimals written to stops.txt.
    fn lat_lon(self) -> (f64, f64) {
        let lat = CENTRE.0 + self.y / M_PER_DEG;
        let lon = CENTRE.1 + self.x / (M_PER_DEG * CENTRE.0.to_radians().cos());
        ((lat * 1e6).round() / 1e6, (lon * 1e6).round() / 1e6)
    }
}

fn uniform(r: &mut Lcg, lo: f64, hi: f64) -> f64 {
    lo + (hi - lo) * (r.below(1 << 20) as f64 / (1u64 << 20) as f64)
}

/// Uniform in `lo..=hi`.
fn int(r: &mut Lcg, lo: u64, hi: u64) -> u64 {
    lo + r.below(hi - lo + 1)
}

fn sign(r: &mut Lcg) -> f64 {
    if r.chance(50) { 1.0 } else { -1.0 }
}

const NAME_HEADS: [&str; 10] = [
    "Ahorn", "Birken", "Brunnen", "Eichen", "Erlen", "Fichten", "Linden", "Weiden", "Tannen",
    "Ulmen",
];
const NAME_TAILS: [&str; 8] = ["platz", "hof", "feld", "weg", "anger", "berg", "au", "ring"];

/// Invented station names, distinct for every index: the head comes from
/// the last digit and the tail from `(3 * tens + last digit) mod 8`, which
/// differs for every tens value below 8; later indices get a number.
fn station_name(i: usize) -> String {
    let (a, p) = (i % 10, i / 10);
    let base = format!("{}{}", NAME_HEADS[a], NAME_TAILS[(3 * p + a) % 8]);
    if i < 80 {
        base
    } else {
        format!("{base} {}", i / 80 + 1)
    }
}

struct Station {
    p: Pt,
    metro: bool,
}

struct Line {
    id: String,
    short_name: String,
    route_type: u16,
    /// Stations in order; a ring repeats its first station at the end.
    stops: Vec<usize>,
    headway_min: i64,
    kmh: f64,
}

struct Gen {
    r: Lcg,
    stations: Vec<Station>,
    lines: Vec<Line>,
}

impl Gen {
    fn add(&mut self, p: Pt, metro: bool) -> usize {
        self.stations.push(Station {
            p: Pt {
                x: p.x.round(),
                y: p.y.round(),
            },
            metro,
        });
        self.stations.len() - 1
    }

    fn clear(&self, p: Pt) -> bool {
        self.stations
            .iter()
            .all(|s| s.p.dist(p) >= MIN_SEPARATION_M)
    }

    /// A new metro station `lo..hi` metres from `from`, roughly towards
    /// `heading`, away from other stations when a few tries allow.
    fn place(&mut self, from: Pt, heading: f64, lo: f64, hi: f64) -> usize {
        let mut p = from;
        for _ in 0..12 {
            let d = uniform(&mut self.r, lo, hi);
            let h = heading + uniform(&mut self.r, -0.25, 0.25);
            p = from.step(h, d);
            if self.clear(p) {
                break;
            }
        }
        self.add(p, true)
    }

    /// Extend `stops` by `count` new stations heading away from its last.
    fn extend(&mut self, stops: &mut Vec<usize>, count: usize, heading: f64, lo: f64, hi: f64) {
        for _ in 0..count {
            let from = self.stations[stops[stops.len() - 1]].p;
            let s = self.place(from, heading, lo, hi);
            stops.push(s);
        }
    }

    fn metro(&self) -> Vec<usize> {
        (0..self.stations.len())
            .filter(|&i| self.stations[i].metro)
            .collect()
    }

    fn pick_metro(&mut self) -> usize {
        let m = self.metro();
        m[self.r.below(m.len() as u64) as usize]
    }

    fn add_metro_line(&mut self, stops: Vec<usize>) {
        let n = self.lines.iter().filter(|l| l.route_type == 1).count() + 1;
        let headway_min = int(&mut self.r, 3, 20) as i64;
        let kmh = uniform(&mut self.r, 30.0, 40.0);
        self.lines.push(Line {
            id: format!("U{n}"),
            short_name: format!("U{n}"),
            route_type: 1,
            stops,
            headway_min,
            kmh,
        });
    }

    /// Whether some line runs directly between `a` and `b`.
    fn adjacent(&self, a: usize, b: usize) -> bool {
        self.lines.iter().any(|l| {
            l.stops
                .windows(2)
                .any(|w| (w[0] == a && w[1] == b) || (w[0] == b && w[1] == a))
        })
    }
}

fn take(spare: &mut usize, want: usize) -> usize {
    let k = want.min(*spare);
    *spare -= k;
    k
}

/// Generate the network for `seed` and `size`.
pub fn generate(seed: u64, size: Size) -> Synthetic {
    let small = size == Size::Small;
    let mut g = Gen {
        r: Lcg::new(seed ^ size.salt()),
        stations: Vec::new(),
        lines: Vec::new(),
    };
    let (lo, hi) = size.targets();
    let n = int(&mut g.r, lo, hi) as usize;
    let mut shape = Shape::default();

    // Which features, and how many new stations each gets; the trunk takes
    // the rest.
    let want_branch = if small {
        g.r.chance(75)
    } else {
        n >= 4 && g.r.chance(45)
    };
    let min_trunk = if small {
        5
    } else if want_branch {
        3
    } else {
        2
    };
    let mut spare = n - min_trunk;
    let branch_new = if want_branch {
        let want = if small {
            int(&mut g.r, 2, 5)
        } else {
            int(&mut g.r, 1, 2)
        };
        take(&mut spare, want as usize)
    } else {
        0
    };
    let want_ring = if small {
        g.r.chance(50)
    } else {
        n >= 5 && g.r.chance(25)
    };
    let mut ring_new = if want_ring {
        let want = if small { int(&mut g.r, 4, 7) } else { 2 };
        take(&mut spare, want as usize)
    } else {
        0
    };
    if ring_new < 2 {
        spare += ring_new;
        ring_new = 0;
    }
    let crossing_count = if small {
        int(&mut g.r, 1, 2)
    } else if n >= 5 && g.r.chance(40) {
        1
    } else {
        0
    };
    let mut crossings = Vec::new();
    for _ in 0..crossing_count {
        let want = if small {
            int(&mut g.r, 3, 6)
        } else {
            int(&mut g.r, 1, 2)
        };
        let k = take(&mut spare, want as usize);
        if k > 0 {
            crossings.push(k);
        }
    }
    let trunk_len = min_trunk + spare;

    // Trunk through the centre.
    let heading = uniform(&mut g.r, 0.0, TAU);
    let (sp_lo, sp_hi) = (500.0, if small { 2200.0 } else { 1800.0 });
    let half = (trunk_len - 1) as f64 * (sp_lo + sp_hi) / 4.0;
    let jitter = uniform(&mut g.r, 0.0, TAU);
    let start = Pt::ORIGIN
        .step(heading + PI, half)
        .step(jitter, uniform(&mut g.r, 0.0, 300.0));
    let mut trunk = vec![g.add(start, true)];
    g.extend(&mut trunk, trunk_len - 1, heading, sp_lo, sp_hi);
    g.add_metro_line(trunk.clone());

    // Branch: shares the trunk up to a split station, then turns away.
    if branch_new > 0 {
        let k = int(&mut g.r, 1, trunk_len as u64 - 2) as usize;
        let h = heading + sign(&mut g.r) * uniform(&mut g.r, 0.6, 1.0);
        let mut stops = trunk[..=k].to_vec();
        g.extend(&mut stops, branch_new, h, sp_lo, sp_hi);
        g.add_metro_line(stops);
        shape.branch = true;
    }

    // Crossing lines through an existing station, or past one close enough
    // to walk.
    for c_new in crossings {
        let anchor = g.pick_metro();
        let h = heading + sign(&mut g.r) * uniform(&mut g.r, 1.0, 2.1);
        let near_miss = c_new >= 2 && g.r.chance(35);
        let (through, rest) = if near_miss {
            let ap = g.stations[anchor].p;
            let side = sign(&mut g.r);
            (g.place(ap, h + side * PI / 2.0, 350.0, 900.0), c_new - 1)
        } else {
            (anchor, c_new)
        };
        shape.near_misses += usize::from(near_miss);
        let back = g.r.below(rest as u64 + 1) as usize;
        let mut behind = vec![through];
        g.extend(&mut behind, back, h + PI, sp_lo, sp_hi);
        behind.reverse();
        g.extend(&mut behind, rest - back, h, sp_lo, sp_hi);
        g.add_metro_line(behind);
    }

    // Ring: a loop through one existing station.
    if ring_new > 0 {
        let anchor = g.pick_metro();
        let radius = if small {
            uniform(&mut g.r, 1200.0, 2600.0)
        } else {
            uniform(&mut g.r, 700.0, 1400.0)
        };
        let ap = g.stations[anchor].p;
        let centre = ap.step(uniform(&mut g.r, 0.0, TAU), radius);
        let base = centre.towards(ap);
        let m = ring_new + 1;
        let mut stops = vec![anchor];
        for j in 1..m {
            let mut p = centre;
            for _ in 0..12 {
                let a = base + TAU * (j as f64 + uniform(&mut g.r, -0.2, 0.2)) / m as f64;
                p = centre.step(a, radius * uniform(&mut g.r, 0.9, 1.1));
                if g.clear(p) {
                    break;
                }
            }
            stops.push(g.add(p, true));
        }
        stops.push(anchor);
        g.add_metro_line(stops);
        shape.ring = true;
    }

    // Connector bus between two metro stations no line joins directly,
    // through stops of its own.
    if g.r.chance(if small { 60 } else { 50 }) {
        let metro = g.metro();
        for _ in 0..30 {
            let a = metro[g.r.below(metro.len() as u64) as usize];
            let b = metro[g.r.below(metro.len() as u64) as usize];
            let (pa, pb) = (g.stations[a].p, g.stations[b].p);
            if a == b || !(1500.0..=6000.0).contains(&pa.dist(pb)) || g.adjacent(a, b) {
                continue;
            }
            let k = int(&mut g.r, 1, 3) as usize;
            let normal = pa.towards(pb) + PI / 2.0;
            let mut stops = vec![a];
            for j in 1..=k {
                let off = sign(&mut g.r) * uniform(&mut g.r, 200.0, 600.0);
                let p = pa.lerp(pb, j as f64 / (k + 1) as f64).step(normal, off);
                stops.push(g.add(p, false));
            }
            stops.push(b);
            let headway_min = int(&mut g.r, 10, 20) as i64;
            let kmh = uniform(&mut g.r, 16.0, 22.0);
            g.lines.push(Line {
                id: "B1".into(),
                short_name: "150".into(),
                route_type: 3,
                stops,
                headway_min,
                kmh,
            });
            shape.bus = true;
            break;
        }
    }

    let metro = g.metro();
    debug_assert_eq!(metro.len(), n, "metro station budget");
    let mut metro_lines_at = vec![0usize; g.stations.len()];
    for l in g.lines.iter().filter(|l| l.route_type == 1) {
        let mut seen = l.stops.clone();
        seen.sort_unstable();
        seen.dedup();
        for s in seen {
            metro_lines_at[s] += 1;
        }
    }
    shape.metro_stations = metro.len();
    shape.bus_only_stops = g.stations.len() - metro.len();
    shape.metro_lines = g.lines.iter().filter(|l| l.route_type == 1).count();
    shape.interchanges = metro_lines_at.iter().filter(|&&k| k >= 2).count();
    let ll: Vec<(f64, f64)> = g.stations.iter().map(|s| s.p.lat_lon()).collect();
    for (i, &a) in metro.iter().enumerate() {
        for &b in &metro[i + 1..] {
            if distance_m(ll[a].0, ll[a].1, ll[b].0, ll[b].1) <= WALK_M {
                shape.walk_pairs += 1;
            }
        }
    }

    let (files, trips) = g.into_files(seed, size, &metro_lines_at);
    shape.trips = trips;
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, b)| (*n, b.as_str())).collect();
    Synthetic {
        shape,
        zip: zip_files(&refs),
    }
}

/// `HH:MM:00` for minutes after the start of the service day.
fn hms(min: i64) -> String {
    format!("{:02}:{:02}:00", min / 60, min % 60)
}

/// The base headway from 06:30 to 20:30, up to twice as long (but at most
/// 20 minutes) outside those hours.
fn headway_at(t_min: i64, base: i64) -> i64 {
    if (6 * 60 + 30..20 * 60 + 30).contains(&t_min) {
        base
    } else {
        (2 * base).min(20).max(base)
    }
}

impl Gen {
    fn station_id(&self, s: usize) -> String {
        if self.stations[s].metro {
            format!("S{s:02}")
        } else {
            format!("B{s:02}")
        }
    }

    /// The stop `line` calls at in station `s`.
    fn platform_id(&self, s: usize, line: &Line) -> String {
        if self.stations[s].metro {
            format!("S{s:02}_{}", line.id)
        } else {
            format!("B{s:02}")
        }
    }

    /// The GTFS files, and the number of trips.
    fn into_files(
        mut self,
        seed: u64,
        size: Size,
        metro_lines_at: &[usize],
    ) -> (Vec<(&'static str, String)>, usize) {
        let mut stops = String::from(
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station,platform_code\n",
        );
        for (s, st) in self.stations.iter().enumerate() {
            let (lat, lon) = st.p.lat_lon();
            let (id, name) = (self.station_id(s), station_name(s));
            if st.metro {
                let _ = writeln!(stops, "{id},{name},{lat:.6},{lon:.6},1,,");
                for (k, l) in self.lines.iter().enumerate() {
                    if l.stops.contains(&s) {
                        let pid = self.platform_id(s, l);
                        let _ = writeln!(stops, "{pid},{name},{lat:.6},{lon:.6},0,{id},{}", k + 1);
                    }
                }
            } else {
                let _ = writeln!(stops, "{id},{name},{lat:.6},{lon:.6},0,,");
            }
        }

        let mut routes =
            String::from("route_id,agency_id,route_short_name,route_long_name,route_type\n");
        let mut trips = String::from("route_id,service_id,trip_id,trip_headsign,direction_id\n");
        let mut times = String::from(
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type\n",
        );
        let mut trip_count = 0;
        let lines = std::mem::take(&mut self.lines);
        for line in &lines {
            let n = line.stops.len();
            let _ = writeln!(
                routes,
                "{},SYN,{},{} - {},{}",
                line.id,
                line.short_name,
                station_name(line.stops[0]),
                station_name(line.stops[n - 1]),
                line.route_type
            );
            let hop_min: Vec<i64> = line
                .stops
                .windows(2)
                .map(|w| {
                    let d = self.stations[w[0]].p.dist(self.stations[w[1]].p) * 1.2;
                    ((d / (line.kmh / 3.6) / 60.0).round() as i64).max(1)
                })
                .collect();
            // A minute's dwell where metro lines meet.
            let dwell_min: Vec<i64> = line
                .stops
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    i64::from(line.route_type == 1 && i > 0 && i + 1 < n && metro_lines_at[s] >= 2)
                })
                .collect();
            for dir in 0..2u8 {
                let (mut order, mut hops, mut dwell) =
                    (line.stops.clone(), hop_min.clone(), dwell_min.clone());
                if dir == 1 {
                    order.reverse();
                    hops.reverse();
                    dwell.reverse();
                }
                let offset = if dir == 1 {
                    self.r.below(line.headway_min as u64) as i64
                } else {
                    0
                };
                let first = 5 * 60 + int(&mut self.r, 0, 30) as i64 + offset;
                let last = 23 * 60 + 30 + int(&mut self.r, 0, 90) as i64;
                let headsign = station_name(order[n - 1]);
                let mut t = first;
                let mut k = 0;
                while t <= last {
                    let trip_id = format!("{}_{dir}_{k:03}", line.id);
                    let _ = writeln!(trips, "{},WD,{trip_id},{headsign},{dir}", line.id);
                    let mut clock = t;
                    for (i, &s) in order.iter().enumerate() {
                        let (arr, dep) = (clock, clock + dwell[i]);
                        let pickup = u8::from(i + 1 == n);
                        let drop_off = u8::from(i == 0);
                        let _ = writeln!(
                            times,
                            "{trip_id},{},{},{},{},{pickup},{drop_off}",
                            hms(arr),
                            hms(dep),
                            self.platform_id(s, line),
                            i + 1
                        );
                        if let Some(h) = hops.get(i) {
                            clock = dep + h;
                        }
                    }
                    t += headway_at(t, line.headway_min);
                    k += 1;
                    trip_count += 1;
                }
            }
        }

        let files = vec![
            (
                "agency.txt",
                "agency_id,agency_name,agency_url,agency_timezone,agency_lang\n\
                 SYN,Synthetic Transit,https://example.org,Europe/Berlin,de\n"
                    .to_string(),
            ),
            (
                "feed_info.txt",
                format!(
                    "feed_publisher_name,feed_publisher_url,feed_lang,feed_start_date,feed_end_date,feed_version\n\
                     allstops synthetic generator,https://example.org,de,20261001,20261231,{}-{seed}\n",
                    size.name()
                ),
            ),
            (
                "calendar.txt",
                "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
                 WD,1,1,1,1,1,0,0,20261001,20261231\n"
                    .to_string(),
            ),
            ("stops.txt", stops),
            ("routes.txt", routes),
            ("trips.txt", trips),
            ("stop_times.txt", times),
        ];
        (files, trip_count)
    }
}

#[cfg(test)]
mod tests {
    use allstops_gtfs::cluster::{ClusterConfig, cluster};
    use allstops_gtfs::select::{Rule, Selection, select};
    use allstops_gtfs::{Feed, Limits};

    use super::*;

    #[test]
    fn same_seed_same_bytes() {
        for size in [Size::Tiny, Size::Small] {
            for seed in 0..5 {
                assert_eq!(generate(seed, size).zip, generate(seed, size).zip);
            }
            assert_ne!(generate(1, size).zip, generate(2, size).zip);
        }
    }

    #[test]
    fn names_are_distinct() {
        let names: std::collections::HashSet<String> = (0..200).map(station_name).collect();
        assert_eq!(names.len(), 200);
    }

    #[test]
    fn every_metro_station_is_a_target_and_sizes_hold() {
        let sel = Selection {
            name: "metro".into(),
            include: vec![Rule {
                route_types: vec![1],
                ..Rule::default()
            }],
            exclude_stations: Vec::new(),
        };
        for size in [Size::Tiny, Size::Small] {
            let (lo, hi) = size.targets();
            for seed in 0..20 {
                let s = generate(seed, size);
                let n = s.shape.metro_stations as u64;
                assert!((lo..=hi).contains(&n), "{size:?} seed {seed}: {n} stations");
                let feed = Feed::from_zip_bytes(&s.zip, &Limits::default()).unwrap();
                assert_eq!(feed.timezone().unwrap().name(), "Europe/Berlin");
                let c = cluster(&feed, &ClusterConfig::default());
                assert_eq!(
                    c.stations.len(),
                    s.shape.metro_stations + s.shape.bus_only_stops
                );
                let targets = select(&feed, &c, &sel).unwrap();
                assert_eq!(
                    targets.len(),
                    s.shape.metro_stations,
                    "{size:?} seed {seed}"
                );
                assert_eq!(feed.trips.len(), s.shape.trips);
            }
        }
    }
}
