//! Bus and train lines: where a line runs and where it stops, read either
//! from the route relations of OpenStreetMap data or, for buses, from a GTFS
//! feed.
//!
//! A train line is read like a bus line, from `route=train` relations — and
//! `light_rail`, `subway`, `tram` and `monorail` ones, which is what makes
//! Bologna's Marconi Express (`ref=MEX`) a train line here. Train lines carry
//! an official colour more often than bus lines do, and keep it
//! ([`BusLine::color`]).
//!
//! Both sources describe a line as several variants — the two directions,
//! and the short runs and branches a timetable adds — and a map showing a
//! line once wants one route and every stop it serves. So a line comes out
//! of either reader as one [`BusLine`]: the route of its main variant, the
//! stretches where its main variant the other way runs elsewhere — the other
//! half of a circular line, a one-way system's parallel street — and the
//! stops of both.
//!
//! # OpenStreetMap
//!
//! A line is a `type=route` + `route=bus` relation per variant, its `ref`
//! the line number. The members are the route's ways in driving order
//! (empty, `forward` or `backward` role), its stop positions (`stop`,
//! `stop_entry_only`, `stop_exit_only`) and its platforms. The main variant
//! is the one with the most members; its counterpart the other way is the
//! one running between the same two places, `to` and `from` swapped.
//!
//! # GTFS
//!
//! A line is a route in `routes.txt`, its `route_short_name` the line number.
//! Each trip of it runs one direction (`direction_id`) along one shape; the
//! main variant of a direction is the shape most trips follow, its route
//! the shape's points (`shapes.txt`) and its stops those of one trip along
//! it (`stop_times.txt`, `stops.txt`). The feed is read from its `.zip` or
//! from a folder it has been unpacked into.
//!
//! # How often a line runs
//!
//! [`BusLine::headway`] is the minutes between two buses of the line, on
//! average over the middle of the day ([`HEADWAY_HOURS`], 10:00 to 16:00):
//! the trips of the direction drawn that leave their first stop in those
//! hours, on one service day (by default a Wednesday the feed runs, or the
//! Saturday or Sunday of [`Day`], or a date asked for), spread over the six
//! hours. A line with no trip at all that day does not run then, and is
//! told apart ([`GtfsLines::idle`]). OpenStreetMap has no timetables: a
//! relation's `interval` tag, where a mapper has set one, is all it says.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use crate::osm::{MemberKind, OsmData, Relation};

/// What kind of line: which route relations it is read from, and how it is
/// drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A bus or trolleybus line.
    Bus,
    /// A train, light rail, metro, tram or monorail line.
    Train,
}

impl Mode {
    /// The `route` values of the relations a line of this kind is read from.
    pub fn routes(self) -> &'static [&'static str] {
        match self {
            Mode::Bus => &["bus", "trolleybus"],
            Mode::Train => &["train", "light_rail", "subway", "tram", "monorail"],
        }
    }

    /// Whether a GTFS `route_type` is a line of this kind: the basic types
    /// (3 bus, 11 trolleybus; 0 tram, 1 metro, 2 rail, 5 cable car, 7
    /// funicular, 12 monorail) and the extended ones' ranges (700s bus,
    /// 800s trolleybus; 100s rail, 400s urban rail, 900s tram).
    pub fn has_route_type(self, route_type: u32) -> bool {
        match self {
            Mode::Bus => matches!(route_type, 3 | 11 | 700..=899),
            Mode::Train => {
                matches!(route_type, 0 | 1 | 2 | 5 | 7 | 12 | 100..=199 | 400..=599 | 900..=999)
            }
        }
    }

    /// The word a line's symbols are keyed by, before its number: `bus 13`,
    /// `train S1`.
    pub fn key(self) -> &'static str {
        match self {
            Mode::Bus => "bus",
            Mode::Train => "train",
        }
    }
}

/// A colour as OpenStreetMap's `colour` tag gives one — `#rgb`, `#rrggbb`
/// or one of the plain names mappers use — as `#rrggbb`.
pub fn parse_colour(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(hex) = value.strip_prefix('#') {
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        return match hex.len() {
            6 => Some(format!("#{hex}")),
            3 => Some(hex.chars().fold(String::from("#"), |mut s, c| {
                s.push(c);
                s.push(c);
                s
            })),
            _ => None,
        };
    }
    let hex = match value.as_str() {
        "black" => "#000000",
        "white" => "#ffffff",
        "red" => "#ff0000",
        "green" => "#008000",
        "blue" => "#0000ff",
        "yellow" => "#ffff00",
        "orange" => "#ffa500",
        "purple" => "#800080",
        "brown" => "#a52a2a",
        "gray" | "grey" => "#808080",
        "darkblue" => "#00008b",
        _ => return None,
    };
    Some(hex.to_string())
}

/// One bus or train line, as a map draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct BusLine {
    /// Whether it is a bus or a train line.
    pub mode: Mode,
    /// The line number, e.g. `"13"` or `"T2"`.
    pub reference: String,
    /// What the source calls the line, e.g. `"13 : Pavese -> Piazza
    /// Malpighi"`.
    pub name: String,
    /// The route of the main variant, as `(lon, lat)`: one piece where it is
    /// continuous, several where the source has gaps in it.
    pub route: Vec<Vec<(f64, f64)>>,
    /// The stretches where the way back leaves [`BusLine::route`] — the
    /// other half of a circular line, a branch, the parallel street of a
    /// one-way system — each from where it leaves the route to where it
    /// comes back, `(lon, lat)`.
    pub detours: Vec<Vec<(f64, f64)>>,
    /// Every stop, each once for each way the line runs through it.
    pub stops: Vec<Stop>,
    /// The line's own colour, `#rrggbb`, where the source gives one.
    pub color: Option<String>,
    /// How many minutes apart its buses run, on average between
    /// [`HEADWAY_HOURS`], where the source says.
    pub headway: Option<f64>,
}

/// A stop of a line, and the way the line runs through it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    /// Where it is, `(lon, lat)`.
    pub at: (f64, f64),
    /// Which way the buses stopping there run: a point a few meters ahead
    /// of [`Stop::at`] along the route of the variant that stops there,
    /// `(lon, lat)`. `None` where there is neither a route nor another stop
    /// to tell.
    pub toward: Option<(f64, f64)>,
}

/// How far ahead of a stop [`Stop::toward`] is, in degrees of latitude:
/// about 10 m.
const AHEAD: f64 = 0.0001;

/// The stops of one variant, in the order it serves them, each with the way
/// the variant runs there: along the closest stretch of its `route`, or
/// where it has none, toward the next stop (from the one before, at the
/// last).
fn directed(stops: &[(f64, f64)], route: &[Vec<(f64, f64)>]) -> Vec<Stop> {
    stops
        .iter()
        .enumerate()
        .map(|(i, &at)| {
            // Degrees of longitude are shorter than those of latitude.
            let k = at.1.to_radians().cos();
            let mut best: Option<(f64, (f64, f64))> = None;
            for piece in route {
                for w in piece.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    let (dx, dy) = ((b.0 - a.0) * k, b.1 - a.1);
                    let length2 = dx * dx + dy * dy;
                    if length2 == 0.0 {
                        continue;
                    }
                    let (px, py) = ((at.0 - a.0) * k, at.1 - a.1);
                    let t = ((px * dx + py * dy) / length2).clamp(0.0, 1.0);
                    let d = (px - t * dx).hypot(py - t * dy);
                    if best.is_none_or(|(b, _)| d < b) {
                        best = Some((d, (dx, dy)));
                    }
                }
            }
            let way = best.map(|(_, way)| way).or_else(|| {
                let (from, to) = match (stops.get(i + 1), i.checked_sub(1)) {
                    (Some(&next), _) => (at, next),
                    (None, Some(previous)) => (stops[previous], at),
                    (None, None) => return None,
                };
                Some(((to.0 - from.0) * k, to.1 - from.1))
            });
            let toward = way.and_then(|(dx, dy)| {
                let length = dx.hypot(dy);
                (length > 0.0).then(|| (at.0 + dx / length * AHEAD / k, at.1 + dy / length * AHEAD))
            });
            Stop { at, toward }
        })
        .collect()
}

/// How far from the route, in meters, the way back has to run to be a
/// detour rather than the same street ([`BusLine::detours`]).
const DETOUR: f64 = 30.0;

/// Where `back` runs more than [`DETOUR`] meters from `route`, for longer
/// than twice that: each such stretch with the points either side of it, so
/// that it starts and ends on the route. `back` is looked at every 10
/// meters, so that one long segment of it cutting a corner counts too.
fn detours(route: &[Vec<(f64, f64)>], back: &[Vec<(f64, f64)>]) -> Vec<Vec<(f64, f64)>> {
    let Some(&(_, lat)) = route.iter().chain(back).flatten().next() else {
        return Vec::new();
    };
    // Meters east and north of nowhere in particular: near enough flat over
    // a city.
    let k = lat.to_radians().cos();
    let meters = |(lon, lat): (f64, f64)| (lon * k * 111_320.0, lat * 111_320.0);
    let segments: Vec<((f64, f64), (f64, f64))> = route
        .iter()
        .flat_map(|piece| piece.windows(2).map(|w| (meters(w[0]), meters(w[1]))))
        .collect();
    let away = |p: (f64, f64)| {
        segments.iter().all(|&(a, b)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let length2 = dx * dx + dy * dy;
            let t = if length2 > 0.0 {
                (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy) > DETOUR
        })
    };
    let mut out = Vec::new();
    for piece in back {
        // Every 10 m along the piece, in degrees.
        let mut samples = Vec::new();
        for w in piece.windows(2) {
            let (a, b) = (meters(w[0]), meters(w[1]));
            let steps = ((b.0 - a.0).hypot(b.1 - a.1) / 10.0).ceil().max(1.0) as usize;
            for i in 0..steps {
                let t = i as f64 / steps as f64;
                samples.push((
                    w[0].0 + t * (w[1].0 - w[0].0),
                    w[0].1 + t * (w[1].1 - w[0].1),
                ));
            }
        }
        samples.extend(piece.last());
        let far: Vec<bool> = samples.iter().map(|&p| away(meters(p))).collect();
        let mut i = 0;
        while i < samples.len() {
            if !far[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < samples.len() && far[i] {
                i += 1;
            }
            let stretch = &samples[start.saturating_sub(1)..(i + 1).min(samples.len())];
            if (stretch.len() as f64 - 1.0) * 10.0 > 2.0 * DETOUR {
                out.push(stretch.to_vec());
            }
        }
    }
    out
}

/// The kind of day the GTFS headways are counted on: a working day, a
/// Saturday, or a Sunday — which stands for the holidays too, as Italian
/// timetables run them alike.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Day {
    /// Monday to Friday, looked for on a Wednesday.
    #[default]
    Weekday,
    /// A Saturday.
    Saturday,
    /// A Sunday or a holiday.
    Sunday,
}

impl Day {
    /// The day of the week a typical such day is looked for on.
    fn weekday(self) -> chrono::Weekday {
        match self {
            Day::Weekday => chrono::Weekday::Wed,
            Day::Saturday => chrono::Weekday::Sat,
            Day::Sunday => chrono::Weekday::Sun,
        }
    }

    /// The kind of day `date` (`YYYYMMDD`) is, by its day of the week.
    pub fn of(date: &str) -> Option<Day> {
        let day = chrono::NaiveDate::parse_from_str(date, "%Y%m%d").ok()?;
        Some(match chrono::Datelike::weekday(&day) {
            chrono::Weekday::Sat => Day::Saturday,
            chrono::Weekday::Sun => Day::Sunday,
            _ => Day::Weekday,
        })
    }
}

/// The hours of the day [`BusLine::headway`] averages over: from 10:00 to
/// 16:00, the middle of the day, clear of the rush hours.
pub const HEADWAY_HOURS: (u32, u32) = (10, 16);

/// An OpenStreetMap `interval` — `HH:MM`, `HH:MM:SS` or plain minutes — in
/// minutes.
pub fn parse_interval(value: &str) -> Option<f64> {
    let parts: Vec<f64> = value
        .trim()
        .split(':')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    let minutes = match parts[..] {
        [minutes] => minutes,
        [hours, minutes] => hours * 60.0 + minutes,
        [hours, minutes, seconds] => hours * 60.0 + minutes + seconds / 60.0,
        _ => return None,
    };
    (minutes > 0.0).then_some(minutes)
}

impl BusLine {
    /// The two ends of the route, where the line number goes: `None` for a
    /// line with no route, and the end the same as the start for a circular
    /// one.
    pub fn ends(&self) -> Option<((f64, f64), (f64, f64))> {
        let start = *self.route.first()?.first()?;
        let end = *self.route.last()?.last()?;
        Some((start, end))
    }
}

/// The stops of a relation, as `(lon, lat)`: its stop positions, or where it
/// has none, its platforms which are nodes.
fn relation_stops(data: &OsmData, relation: &Relation) -> Vec<(f64, f64)> {
    let nodes_with = |roles: &[&str]| -> Vec<(f64, f64)> {
        relation
            .members
            .iter()
            .filter(|m| m.kind == MemberKind::Node && roles.contains(&m.role.as_str()))
            .filter_map(|m| data.nodes.get(&m.id).copied())
            .collect()
    };
    let stops = nodes_with(&["stop", "stop_entry_only", "stop_exit_only"]);
    if stops.is_empty() {
        nodes_with(&["platform", "platform_entry_only", "platform_exit_only"])
    } else {
        stops
    }
}

/// A relation's route: its ways, in order, each turned to carry on from the
/// one before; a new piece wherever one does not join the last.
fn relation_route(data: &OsmData, relation: &Relation) -> Vec<Vec<(f64, f64)>> {
    let ways = relation
        .members
        .iter()
        .filter(|m| {
            m.kind == MemberKind::Way && matches!(m.role.as_str(), "" | "forward" | "backward")
        })
        .filter_map(|m| data.way(m.id))
        .filter(|w| w.nodes.len() >= 2);

    let mut pieces: Vec<Vec<i64>> = Vec::new();
    // Whether the last piece is still a single way, which may be the wrong
    // way round: only the next way says which of its ends the route leaves.
    let mut fresh = false;
    for way in ways {
        let nodes = &way.nodes;
        let touches = |node: Option<&i64>| nodes.first() == node || nodes.last() == node;
        if let Some(piece) = pieces.last_mut() {
            if fresh && !touches(piece.last()) && touches(piece.first()) {
                piece.reverse();
            }
            let end = *piece.last().expect("a piece has nodes");
            if nodes.first() == Some(&end) {
                piece.extend_from_slice(&nodes[1..]);
                fresh = false;
                continue;
            }
            if nodes.last() == Some(&end) {
                piece.extend(nodes.iter().rev().skip(1));
                fresh = false;
                continue;
            }
        }
        pieces.push(nodes.clone());
        fresh = true;
    }
    pieces
        .into_iter()
        .map(|piece| {
            piece
                .iter()
                .filter_map(|id| data.nodes.get(id).copied())
                .collect::<Vec<_>>()
        })
        .filter(|piece| piece.len() >= 2)
        .collect()
}

/// Every stop once: the same position twice the same way, as two variants
/// share a stop, is one stop.
fn unique(stops: impl IntoIterator<Item = Stop>) -> Vec<Stop> {
    let mut seen = HashSet::new();
    let bits = |(x, y): (f64, f64)| (x.to_bits(), y.to_bits());
    stops
        .into_iter()
        .filter(|stop| seen.insert((bits(stop.at), stop.toward.map(bits))))
        .collect()
}

/// The order a timetable lists line numbers in: numbers by their value,
/// then those starting with a letter, alphabetically — `2, 11, 13, 101, A,
/// N1` rather than `101, 11, 13, 2, A, N1`.
pub fn line_order(a: &str, b: &str) -> std::cmp::Ordering {
    let number = |s: &str| -> Option<u64> {
        let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    match (number(a), number(b)) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.cmp(b)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

/// Every line number of kind `mode` among `data`'s route relations, each
/// once, in [`line_order`].
pub fn osm_references(data: &OsmData, mode: Mode) -> Vec<String> {
    let mut references: Vec<String> = data
        .relations
        .iter()
        .filter(|r| {
            r.tags.is("type", "route")
                && r.tags
                    .get("route")
                    .is_some_and(|v| mode.routes().contains(&v))
        })
        .filter_map(|r| r.tags.get("ref").map(str::to_string))
        .collect();
    references.sort_by(|a, b| line_order(a, b));
    references.dedup();
    references
}

/// Every line number of kind `mode` in the GTFS feed at `path`, each once,
/// in [`line_order`].
pub fn gtfs_references(path: &Path, mode: Mode) -> Result<Vec<String>, String> {
    let routes = Feed::open(path)?.table("routes.txt")?;
    let short_name = routes.column("routes.txt", "route_short_name")?;
    let route_type = routes.column("routes.txt", "route_type")?;
    let mut references: Vec<String> = routes
        .rows
        .iter()
        .filter(|row| {
            row.get(route_type)
                .and_then(|t| t.trim().parse::<u32>().ok())
                .is_some_and(|t| mode.has_route_type(t))
        })
        .filter_map(|row| row.get(short_name).cloned())
        .filter(|r| !r.is_empty())
        .collect();
    references.sort_by(|a, b| line_order(a, b));
    references.dedup();
    Ok(references)
}

/// The lines numbered `references`, of kind `mode`, from the route
/// relations of `data`, in the order asked for. A number with no relation is
/// left out.
///
/// The way back is the variant running between the same two places, `from`
/// and `to` swapped; where the relations do not say, the largest of the
/// other variants. The colour is the main variant's `colour`, or its
/// route master's.
pub fn lines_from_osm(data: &OsmData, references: &[String], mode: Mode) -> Vec<BusLine> {
    references
        .iter()
        .filter_map(|reference| {
            let variants: Vec<&Relation> = data
                .relations
                .iter()
                .filter(|r| {
                    r.tags.is("type", "route")
                        && r.tags
                            .get("route")
                            .is_some_and(|v| mode.routes().contains(&v))
                        && r.tags.get("ref") == Some(reference.as_str())
                })
                .collect();
            let main = *variants.iter().max_by_key(|r| r.members.len())?;
            let others = variants.iter().filter(|r| r.id != main.id);
            let back = others
                .clone()
                .find(|r| {
                    r.tags.get("from").is_some()
                        && r.tags.get("from") == main.tags.get("to")
                        && r.tags.get("to") == main.tags.get("from")
                })
                .or_else(|| {
                    let directed = main.tags.get("from").is_some() && main.tags.get("to").is_some();
                    (!directed)
                        .then(|| others.max_by_key(|r| r.members.len()))
                        .flatten()
                });
            let route = relation_route(data, main);
            let mut stops = directed(&relation_stops(data, main), &route);
            let mut detoured = Vec::new();
            if let Some(back) = back {
                let back_route = relation_route(data, back);
                stops.extend(directed(&relation_stops(data, back), &back_route));
                detoured = detours(&route, &back_route);
            }
            let master = data.relations.iter().find(|r| {
                r.tags.is("type", "route_master")
                    && r.members
                        .iter()
                        .any(|m| m.kind == MemberKind::Relation && m.id == main.id)
            });
            let tag = |key: &str| {
                main.tags
                    .get(key)
                    .or_else(|| master.and_then(|m| m.tags.get(key)))
            };
            let color = tag("colour").and_then(parse_colour);
            let headway = tag("interval").and_then(parse_interval);
            Some(BusLine {
                mode,
                reference: reference.clone(),
                name: main.tags.get("name").unwrap_or(reference).to_string(),
                route,
                detours: detoured,
                stops: unique(stops),
                color,
                headway,
            })
        })
        .collect()
}

/// The fields of one CSV line, as GTFS writes them: comma separated, a field
/// in double quotes where it holds a comma, a quote doubled inside one.
fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => fields.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    fields.push(field);
    fields
}

/// A GTFS table: its rows, each a map from column name to value.
struct Table {
    columns: HashMap<String, usize>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn parse(name: &str, text: &str) -> Result<Table, String> {
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header = lines.next().ok_or(format!("{name} is empty"))?;
        let columns = csv_fields(header.trim_start_matches('\u{feff}'))
            .into_iter()
            .enumerate()
            .map(|(i, c)| (c.trim().to_string(), i))
            .collect();
        Ok(Table {
            columns,
            rows: lines.map(csv_fields).collect(),
        })
    }

    /// The column's position, or an error naming the table and the column.
    fn column(&self, table: &str, name: &str) -> Result<usize, String> {
        self.columns
            .get(name)
            .copied()
            .ok_or(format!("{table} has no {name} column"))
    }
}

/// Where a GTFS feed's tables come from: its `.zip`, or a folder it has been
/// unpacked into.
enum Feed {
    Zip(zip::ZipArchive<std::fs::File>),
    Folder(std::path::PathBuf),
}

impl Feed {
    fn open(path: &Path) -> Result<Feed, String> {
        if path.is_dir() {
            return Ok(Feed::Folder(path.to_path_buf()));
        }
        let file = std::fs::File::open(path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        zip::ZipArchive::new(file)
            .map(Feed::Zip)
            .map_err(|e| format!("{} is not a GTFS zip: {e}", path.display()))
    }

    /// A table the GTFS reference makes optional: `None` where the feed has
    /// none.
    fn optional_table(&mut self, name: &str) -> Result<Option<Table>, String> {
        let present = match self {
            Feed::Folder(dir) => dir.join(name).exists(),
            Feed::Zip(archive) => archive.index_for_name(name).is_some(),
        };
        if present {
            self.table(name).map(Some)
        } else {
            Ok(None)
        }
    }

    fn table(&mut self, name: &str) -> Result<Table, String> {
        let mut text = String::new();
        match self {
            Feed::Folder(dir) => {
                text = std::fs::read_to_string(dir.join(name))
                    .map_err(|e| format!("cannot read {name}: {e}"))?;
            }
            Feed::Zip(archive) => {
                archive
                    .by_name(name)
                    .map_err(|e| format!("the feed has no {name}: {e}"))?
                    .read_to_string(&mut text)
                    .map_err(|e| format!("cannot read {name}: {e}"))?;
            }
        }
        Table::parse(name, &text)
    }
}

/// Values in the order of a GTFS `*_sequence` column, still to be sorted by it.
type Sequenced<T> = Vec<(u64, T)>;

/// What [`lines_from_gtfs`] reads from a feed.
pub struct GtfsLines {
    /// The lines, in the order asked for.
    pub lines: Vec<BusLine>,
    /// The lines of the feed asked for which run no trip at all on
    /// [`GtfsLines::date`], in the order asked for: read like the others,
    /// but not to be drawn on a map of that day.
    pub idle: Vec<BusLine>,
    /// The day the lines' headways were counted on, `YYYYMMDD`: `None` where
    /// the feed has no calendar to pick one from.
    pub date: Option<String>,
}

/// The service ids running on `date` (`YYYYMMDD`): those of `calendar.txt`
/// whose dates and weekdays take it in, with `calendar_dates.txt`'s
/// exceptions for that day added (type 1) and taken away (type 2).
fn services_on(
    date: &str,
    calendar: Option<&Table>,
    exceptions: Option<&Table>,
) -> Result<HashSet<String>, String> {
    let day = chrono::NaiveDate::parse_from_str(date, "%Y%m%d")
        .map_err(|_| format!("not a date: {date:?} (want YYYYMMDD)"))?;
    let weekday = [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ][chrono::Datelike::weekday(&day).num_days_from_monday() as usize];
    let mut services = HashSet::new();
    if let Some(calendar) = calendar {
        let (id, start, end, runs) = (
            calendar.column("calendar.txt", "service_id")?,
            calendar.column("calendar.txt", "start_date")?,
            calendar.column("calendar.txt", "end_date")?,
            calendar.column("calendar.txt", weekday)?,
        );
        for row in &calendar.rows {
            if row[start].as_str() <= date && date <= row[end].as_str() && row[runs] == "1" {
                services.insert(row[id].clone());
            }
        }
    }
    if let Some(exceptions) = exceptions {
        let (id, on, kind) = (
            exceptions.column("calendar_dates.txt", "service_id")?,
            exceptions.column("calendar_dates.txt", "date")?,
            exceptions.column("calendar_dates.txt", "exception_type")?,
        );
        for row in exceptions.rows.iter().filter(|row| row[on] == date) {
            if row[kind] == "1" {
                services.insert(row[id].clone());
            } else {
                services.remove(&row[id]);
            }
        }
    }
    Ok(services)
}

/// The `day` the feed runs the most services on, `YYYYMMDD`: for a working
/// day a Wednesday, clear of the Monday and Friday variants and of
/// holidays; a Saturday or a Sunday clear of the reduced service of a
/// holiday season.
///
/// The days looked at are those `calendar.txt`'s date ranges span, and
/// those `calendar_dates.txt` adds a service on — which is all a feed that
/// lists its days one by one, with no `calendar.txt`, has.
fn typical_day(
    day: Day,
    calendar: Option<&Table>,
    exceptions: Option<&Table>,
) -> Result<Option<String>, String> {
    let parse = |d: &str| chrono::NaiveDate::parse_from_str(d, "%Y%m%d").ok();
    let mut days: Vec<chrono::NaiveDate> = Vec::new();
    if let Some(calendar) = calendar {
        let (start, end) = (
            calendar.column("calendar.txt", "start_date")?,
            calendar.column("calendar.txt", "end_date")?,
        );
        let first = calendar.rows.iter().filter_map(|r| parse(&r[start])).min();
        let last = calendar.rows.iter().filter_map(|r| parse(&r[end])).max();
        if let (Some(first), Some(last)) = (first, last) {
            let mut day = first;
            while day <= last {
                days.push(day);
                day += chrono::TimeDelta::days(1);
            }
        }
    }
    if let Some(exceptions) = exceptions {
        let (on, kind) = (
            exceptions.column("calendar_dates.txt", "date")?,
            exceptions.column("calendar_dates.txt", "exception_type")?,
        );
        days.extend(
            exceptions
                .rows
                .iter()
                .filter(|row| row[kind] == "1")
                .filter_map(|row| parse(&row[on])),
        );
    }
    days.sort_unstable();
    days.dedup();

    let mut best: Option<(usize, String)> = None;
    for each in days {
        if chrono::Datelike::weekday(&each) == day.weekday() {
            let date = each.format("%Y%m%d").to_string();
            let count = services_on(&date, calendar, exceptions)?.len();
            if count > 0 && best.as_ref().is_none_or(|(most, _)| count > *most) {
                best = Some((count, date));
            }
        }
    }
    Ok(best.map(|(_, date)| date))
}

/// A GTFS time, `H:MM:SS` or `HH:MM:SS` (past 24 for a trip running after
/// midnight), in minutes since the service day began.
fn minutes(time: &str) -> Option<u32> {
    let mut parts = time.trim().split(':').map(|p| p.parse::<u32>().ok());
    let (hours, mins) = (parts.next()??, parts.next()??);
    Some(hours * 60 + mins)
}

/// The lines numbered `references`, from the GTFS feed at `path` (its `.zip`
/// or the folder it has been unpacked into), in the order asked for. A
/// number the feed has no route for is left out.
///
/// The headways are counted on `date` (`YYYYMMDD`), or where none is given
/// on the typical `day` the feed runs the most services on; a line with no
/// trip that day comes back among the idle ones. The lines come back
/// as lines of kind `mode`, in the colour the feed gives them where it gives
/// one other than white.
pub fn lines_from_gtfs(
    path: &Path,
    references: &[String],
    date: Option<&str>,
    day: Day,
    mode: Mode,
) -> Result<GtfsLines, String> {
    let mut feed = Feed::open(path)?;

    // calendar.txt and calendar_dates.txt: which services run that day.
    let calendar = feed.optional_table("calendar.txt")?;
    let exceptions = feed.optional_table("calendar_dates.txt")?;
    let date = match date {
        Some(date) => Some(date.to_string()),
        None => typical_day(day, calendar.as_ref(), exceptions.as_ref())?,
    };
    let running = match &date {
        Some(date) => services_on(date, calendar.as_ref(), exceptions.as_ref())?,
        None => HashSet::new(),
    };

    // routes.txt: which routes are the lines asked for.
    let routes = feed.table("routes.txt")?;
    let (route_id, short_name) = (
        routes.column("routes.txt", "route_id")?,
        routes.column("routes.txt", "route_short_name")?,
    );
    let long_name = routes.columns.get("route_long_name").copied();
    let route_color = routes.columns.get("route_color").copied();
    let wanted: HashSet<&str> = references.iter().map(String::as_str).collect();
    let mut route_of: HashMap<String, (String, String)> = HashMap::new();
    let mut color_of: HashMap<String, String> = HashMap::new();
    for row in &routes.rows {
        let reference = row.get(short_name).cloned().unwrap_or_default();
        if wanted.contains(reference.as_str()) {
            let name = long_name
                .and_then(|c| row.get(c).cloned())
                .unwrap_or_default();
            // A colour of the line's own, `RRGGBB` without the `#`. White
            // is no colour at all: it is what TPER's bus feed gives every
            // line alike.
            let color = route_color
                .and_then(|c| row.get(c))
                .and_then(|c| parse_colour(&format!("#{}", c.trim())))
                .filter(|c| c != "#ffffff");
            if let Some(color) = color {
                color_of.insert(reference.clone(), color);
            }
            route_of.insert(row[route_id].clone(), (reference, name));
        }
    }

    // trips.txt: how many trips each (line, direction, shape) has, and one
    // trip along each shape.
    let trips = feed.table("trips.txt")?;
    let (t_route, t_trip, t_shape) = (
        trips.column("trips.txt", "route_id")?,
        trips.column("trips.txt", "trip_id")?,
        trips.column("trips.txt", "shape_id")?,
    );
    let t_direction = trips.columns.get("direction_id").copied();
    let t_service = trips.column("trips.txt", "service_id")?;
    let mut counts: HashMap<(String, String, String), usize> = HashMap::new();
    let mut trip_along: HashMap<String, String> = HashMap::new();
    // The trips running that day: their line and direction.
    let mut running_trips: HashMap<String, (String, String)> = HashMap::new();
    for row in &trips.rows {
        let Some((reference, _)) = route_of.get(&row[t_route]) else {
            continue;
        };
        let direction = t_direction
            .and_then(|c| row.get(c).cloned())
            .unwrap_or_default();
        if running.contains(&row[t_service]) {
            running_trips.insert(row[t_trip].clone(), (reference.clone(), direction.clone()));
        }
        let shape = row[t_shape].clone();
        *counts
            .entry((reference.clone(), direction, shape.clone()))
            .or_default() += 1;
        trip_along
            .entry(shape)
            .or_insert_with(|| row[t_trip].clone());
    }

    // The main shape of each direction of each line, the busiest first.
    let mut main_shapes: HashMap<String, Vec<(usize, String, String)>> = HashMap::new();
    for ((reference, direction, shape), count) in counts {
        let shapes = main_shapes.entry(reference).or_default();
        match shapes.iter_mut().find(|(_, d, _)| *d == direction) {
            Some(best) if best.0 >= count => {}
            Some(best) => *best = (count, direction, shape),
            None => shapes.push((count, direction, shape)),
        }
    }
    for shapes in main_shapes.values_mut() {
        // Direction 0 is the route drawn; the busier direction where a line
        // has no direction 0.
        shapes.sort_by(|a, b| (a.1 != "0").cmp(&(b.1 != "0")).then(b.0.cmp(&a.0)));
    }
    // Both directions': the way back's stops are headed along its own, and
    // where it leaves the route drawn, it is drawn too.
    let needed_shapes: HashSet<&str> = main_shapes
        .values()
        .flatten()
        .map(|(_, _, shape)| shape.as_str())
        .collect();
    let needed_trips: HashSet<&str> = main_shapes
        .values()
        .flatten()
        .filter_map(|(_, _, shape)| trip_along.get(shape).map(String::as_str))
        .collect();

    // shapes.txt: the points of the main shapes, in order.
    let shapes = feed.table("shapes.txt")?;
    let (s_id, s_lat, s_lon, s_seq) = (
        shapes.column("shapes.txt", "shape_id")?,
        shapes.column("shapes.txt", "shape_pt_lat")?,
        shapes.column("shapes.txt", "shape_pt_lon")?,
        shapes.column("shapes.txt", "shape_pt_sequence")?,
    );
    let mut points: HashMap<&str, Sequenced<(f64, f64)>> = HashMap::new();
    for row in &shapes.rows {
        if let Some(&shape) = needed_shapes.get(row[s_id].as_str()) {
            let (Ok(lat), Ok(lon), Ok(seq)) = (
                row[s_lat].parse::<f64>(),
                row[s_lon].parse::<f64>(),
                row[s_seq].parse::<u64>(),
            ) else {
                continue;
            };
            points.entry(shape).or_default().push((seq, (lon, lat)));
        }
    }

    // stop_times.txt and stops.txt: the stops of one trip along each.
    let stop_times = feed.table("stop_times.txt")?;
    let (st_trip, st_stop, st_seq) = (
        stop_times.column("stop_times.txt", "trip_id")?,
        stop_times.column("stop_times.txt", "stop_id")?,
        stop_times.column("stop_times.txt", "stop_sequence")?,
    );
    let st_departure = stop_times.column("stop_times.txt", "departure_time")?;
    let mut stops_of: HashMap<&str, Sequenced<String>> = HashMap::new();
    // When each trip running that day leaves its first stop.
    let mut departs: HashMap<&str, (u64, Option<u32>)> = HashMap::new();
    for row in &stop_times.rows {
        if running_trips.contains_key(row[st_trip].as_str()) {
            let seq = row[st_seq].parse::<u64>().unwrap_or(0);
            let time = minutes(&row[st_departure]);
            let first = departs.entry(row[st_trip].as_str()).or_insert((seq, time));
            if seq < first.0 {
                *first = (seq, time);
            }
        }
        if let Some(&trip) = needed_trips.get(row[st_trip].as_str()) {
            let seq = row[st_seq].parse::<u64>().unwrap_or(0);
            stops_of
                .entry(trip)
                .or_default()
                .push((seq, row[st_stop].clone()));
        }
    }
    let stops = feed.table("stops.txt")?;
    let (p_id, p_lat, p_lon) = (
        stops.column("stops.txt", "stop_id")?,
        stops.column("stops.txt", "stop_lat")?,
        stops.column("stops.txt", "stop_lon")?,
    );
    let position: HashMap<&str, (f64, f64)> = stops
        .rows
        .iter()
        .filter_map(|row| {
            let lat = row[p_lat].parse::<f64>().ok()?;
            let lon = row[p_lon].parse::<f64>().ok()?;
            Some((row[p_id].as_str(), (lon, lat)))
        })
        .collect();

    // How many trips each (line, direction) starts in the middle of the day.
    let (from, to) = (HEADWAY_HOURS.0 * 60, HEADWAY_HOURS.1 * 60);
    let mut starts: HashMap<(&str, &str), u32> = HashMap::new();
    for (trip, (_, time)) in &departs {
        if time.is_some_and(|t| from <= t && t < to) {
            let (reference, direction) = &running_trips[*trip];
            *starts.entry((reference, direction)).or_default() += 1;
        }
    }

    let name_of: HashMap<&str, &str> = route_of
        .values()
        .map(|(reference, name)| (reference.as_str(), name.as_str()))
        .collect();
    let lines = references
        .iter()
        .filter_map(|reference| {
            let shapes = main_shapes.get(reference)?;
            let (_, direction, main) = shapes.first()?;
            let headway = starts
                .get(&(reference.as_str(), direction.as_str()))
                .map(|&count| f64::from(to - from) / f64::from(count));
            let shape_points = |shape: &str| {
                let mut points = points.get(shape).cloned().unwrap_or_default();
                points.sort_by_key(|(seq, _)| *seq);
                points.into_iter().map(|(_, p)| p).collect::<Vec<_>>()
            };
            let route = shape_points(main);
            let mut line_stops = Vec::new();
            for (_, _, shape) in shapes {
                let Some(trip) = trip_along.get(shape) else {
                    continue;
                };
                let mut trip_stops = stops_of.get(trip.as_str()).cloned().unwrap_or_default();
                trip_stops.sort_by_key(|(seq, _)| *seq);
                let at: Vec<(f64, f64)> = trip_stops
                    .iter()
                    .filter_map(|(_, id)| position.get(id.as_str()).copied())
                    .collect();
                line_stops.extend(directed(&at, &[shape_points(shape)]));
            }
            let route = vec![route];
            let detoured = shapes
                .iter()
                .filter(|(_, _, shape)| shape != main)
                .flat_map(|(_, _, shape)| detours(&route, &[shape_points(shape)]))
                .collect();
            Some(BusLine {
                mode,
                color: color_of.get(reference).cloned(),
                reference: reference.clone(),
                name: name_of.get(reference.as_str()).unwrap_or(&"").to_string(),
                route,
                detours: detoured,
                stops: unique(line_stops),
                headway,
            })
        })
        .collect::<Vec<_>>();
    // With no day to go by, every line counts as running.
    let runs: HashSet<&str> = running_trips
        .values()
        .map(|(reference, _)| reference.as_str())
        .collect();
    let (lines, idle) = lines
        .into_iter()
        .partition(|line| date.is_none() || runs.contains(line.reference.as_str()));
    Ok(GtfsLines { lines, idle, date })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::read_osm_str;

    #[test]
    fn the_way_back_is_drawn_only_where_it_leaves_the_route() {
        // East along a street, about 800 m; back west along it, but for a
        // block where it runs on a parallel street 100 m north, and 10 m
        // off the street — the same street — for the rest.
        let route = vec![vec![(11.0, 44.0), (11.01, 44.0)]];
        let back = vec![vec![
            (11.01, 44.0001),
            (11.006, 44.0001),
            (11.006, 44.0009),
            (11.003, 44.0009),
            (11.003, 44.0001),
            (11.0, 44.0001),
        ]];
        let detoured = detours(&route, &back);
        assert_eq!(detoured.len(), 1);
        let stretch = &detoured[0];
        // From the street, round the block, back to the street.
        assert!(stretch.first().unwrap().1 < 44.0004 && stretch.last().unwrap().1 < 44.0004);
        assert!(stretch.iter().any(|p| p.1 > 44.0008));
        let (east, west) = (stretch.first().unwrap().0, stretch.last().unwrap().0);
        assert!(east > 11.0055 && west < 11.0035, "{east} {west}");
        // The same street both ways is no detour.
        assert!(detours(&route, &[vec![(11.01, 44.0001), (11.0, 44.0001)]]).is_empty());
    }

    #[test]
    fn csv_fields_keep_quoted_commas_and_doubled_quotes() {
        assert_eq!(
            csv_fields(r#""13","TPERBO","Pavese, Rastignano","say ""hi""",,3"#),
            vec!["13", "TPERBO", "Pavese, Rastignano", r#"say "hi""#, "", "3"]
        );
    }

    const ROUTES: &str = r#"<?xml version='1.0' encoding='UTF-8'?>
<osm version="0.6">
  <node id="1" lat="44.0" lon="11.0"/>
  <node id="2" lat="44.0" lon="11.1"/>
  <node id="3" lat="44.1" lon="11.1"/>
  <node id="4" lat="44.1" lon="11.2"/>
  <node id="5" lat="44.05" lon="11.1"/>
  <node id="6" lat="44.06" lon="11.11"/>
  <way id="10"><nd ref="2"/><nd ref="1"/></way>
  <way id="11"><nd ref="2"/><nd ref="5"/><nd ref="3"/></way>
  <way id="12"><nd ref="4"/><nd ref="3"/></way>
  <relation id="20">
    <member type="node" ref="1" role="stop"/>
    <member type="node" ref="5" role="stop"/>
    <member type="way" ref="10" role=""/>
    <member type="way" ref="11" role=""/>
    <member type="way" ref="12" role=""/>
    <tag k="type" v="route"/><tag k="route" v="bus"/><tag k="ref" v="13"/>
    <tag k="name" v="13 : A -> B"/><tag k="from" v="A"/><tag k="to" v="B"/>
  </relation>
  <relation id="21">
    <member type="node" ref="6" role="stop"/>
    <member type="way" ref="12" role=""/>
    <tag k="type" v="route"/><tag k="route" v="bus"/><tag k="ref" v="13"/>
    <tag k="from" v="B"/><tag k="to" v="A"/>
  </relation>
</osm>"#;

    #[test]
    fn a_relation_is_one_route_with_both_directions_stops() {
        let data = read_osm_str(ROUTES).unwrap();
        let lines = lines_from_osm(&data, &["13".to_string(), "99".to_string()], Mode::Bus);
        assert_eq!(lines.len(), 1, "line 99 has no relation");
        let line = &lines[0];
        assert_eq!(line.name, "13 : A -> B");
        // Way 10 runs 2-1 but the route leaves from 1: it is turned round,
        // and 12, running 4-3, is turned to carry on from 3.
        assert_eq!(
            line.route,
            vec![vec![
                (11.0, 44.0),
                (11.1, 44.0),
                (11.1, 44.05),
                (11.1, 44.1),
                (11.2, 44.1)
            ]]
        );
        assert_eq!(line.ends(), Some(((11.0, 44.0), (11.2, 44.1))));
        // Its own two stops and the one of the way back.
        let at: Vec<(f64, f64)> = line.stops.iter().map(|s| s.at).collect();
        assert_eq!(at, vec![(11.0, 44.0), (11.1, 44.05), (11.11, 44.06)]);
        // Each the way its own relation runs there: east, north, and west
        // along the way back.
        let sign = |d: f64| match d {
            d if d > 0.0 => 1,
            d if d < 0.0 => -1,
            _ => 0,
        };
        let way = |i: usize| {
            let (stop, toward) = (line.stops[i].at, line.stops[i].toward.unwrap());
            (sign(toward.0 - stop.0), sign(toward.1 - stop.1))
        };
        assert_eq!([way(0), way(1), way(2)], [(1, 0), (0, 1), (-1, 0)]);
    }

    #[test]
    fn a_train_line_is_not_a_bus_line_and_keeps_its_colour() {
        let text = ROUTES.replace(
            r#"<tag k="route" v="bus"/>"#,
            r#"<tag k="route" v="train"/>"#,
        );
        let text = text.replace(
            r#"<tag k="from" v="A"/>"#,
            r##"<tag k="from" v="A"/><tag k="colour" v="#ED1D24"/>"##,
        );
        let data = read_osm_str(&text).unwrap();
        assert!(lines_from_osm(&data, &["13".to_string()], Mode::Bus).is_empty());
        let lines = lines_from_osm(&data, &["13".to_string()], Mode::Train);
        assert_eq!(lines[0].mode, Mode::Train);
        assert_eq!(lines[0].color.as_deref(), Some("#ed1d24"));
    }

    #[test]
    fn colours_come_as_osm_writes_them() {
        assert_eq!(parse_colour("#BED747").as_deref(), Some("#bed747"));
        assert_eq!(parse_colour("#a0f").as_deref(), Some("#aa00ff"));
        assert_eq!(parse_colour("red").as_deref(), Some("#ff0000"));
        assert_eq!(parse_colour("#12345"), None);
        assert_eq!(parse_colour("fuchsia-ish"), None);
    }

    #[test]
    fn gtfs_takes_the_busiest_shape_the_stops_of_both_directions_and_the_headway() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
        write(
            "routes.txt",
            "route_id,route_short_name,route_long_name\nR13,13,\"Pavese, Malpighi\"\nR19,19,x\n",
        );
        write(
            "trips.txt",
            "route_id,service_id,trip_id,direction_id,shape_id\n\
             R13,wk,t1,0,main\nR13,wk,t2,0,main\nR13,wk,t3,0,short\nR13,wk,t4,1,back\n\
             R19,wk,t5,0,other\nR13,su,t6,0,main\nR13,wk,t7,0,main\n",
        );
        write(
            "shapes.txt",
            "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence\n\
             main,44.1,11.1,2\nmain,44.0,11.0,1\nshort,44.0,11.0,1\nshort,44.05,11.05,2\n\
             back,44.1,11.1,1\nback,44.0,11.0,2\nother,45.0,12.0,1\nother,45.1,12.0,2\n",
        );
        write(
            "stop_times.txt",
            "trip_id,stop_id,stop_sequence,departure_time\n\
             t1,s2,2,10:20:00\nt1,s1,1,10:05:00\nt4,s3,1,12:00:00\nt4,s1,2,12:10:00\n\
             t3,s9,1,15:59:00\nt2,s1,1,11:00:00\nt6,s1,1,12:00:00\nt7,s1,1,09:30:00\n",
        );
        write(
            "stops.txt",
            "stop_id,stop_lat,stop_lon\ns1,44.0,11.0\ns2,44.1,11.1\ns3,44.1,11.11\ns9,44.05,11.05\n",
        );

        // Weekdays through January, but not on the 7th; Sundays apart.
        write(
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
             wk,1,1,1,1,1,0,0,20260105,20260130\nsu,0,0,0,0,0,0,1,20260105,20260130\n",
        );
        write(
            "calendar_dates.txt",
            "service_id,date,exception_type\nwk,20260107,2\n",
        );

        let read = lines_from_gtfs(
            dir.path(),
            &["13".to_string()],
            None,
            Day::Weekday,
            Mode::Bus,
        )
        .unwrap();
        // The first Wednesday the weekday service runs.
        assert_eq!(read.date.as_deref(), Some("20260114"));
        let lines = read.lines;
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line.name, "Pavese, Malpighi");
        assert_eq!(line.route, vec![vec![(11.0, 44.0), (11.1, 44.1)]]);
        // The busiest shape each way: main (2 trips) and back. The short
        // run's stop is not among them; s1 is shared.
        let at: Vec<(f64, f64)> = line.stops.iter().map(|s| s.at).collect();
        assert_eq!(
            at,
            vec![(11.0, 44.0), (11.1, 44.1), (11.11, 44.1), (11.0, 44.0)]
        );
        // The first stop twice: north-east on the way out, south-west on the
        // way back, along each direction's own shape.
        let (out, back) = (line.stops[0], line.stops[3]);
        assert!(out.toward.is_some_and(|t| t.0 > 11.0 && t.1 > 44.0));
        assert!(back.toward.is_some_and(|t| t.0 < 11.0 && t.1 < 44.0));
        // Direction 0 starts t1, t2 and t3 between 10 and 16 on a weekday;
        // t7 starts before 10 and t6 runs on Sundays: 360 minutes / 3.
        assert_eq!(line.headway, Some(120.0));

        // On the 7th the weekday service is off: line 13 does not run.
        let off = lines_from_gtfs(
            dir.path(),
            &["13".to_string()],
            Some("20260107"),
            Day::Weekday,
            Mode::Bus,
        )
        .unwrap();
        assert!(off.lines.is_empty());
        assert_eq!(off.idle[0].reference, "13");

        // On a Sunday only t6 runs, at 12:00: 360 minutes / 1. Line 19
        // runs no trip then.
        let sunday = lines_from_gtfs(
            dir.path(),
            &["13".to_string(), "19".to_string()],
            None,
            Day::Sunday,
            Mode::Bus,
        )
        .unwrap();
        assert_eq!(sunday.date.as_deref(), Some("20260111"));
        assert_eq!(sunday.lines.len(), 1);
        assert_eq!(sunday.lines[0].headway, Some(360.0));
        assert_eq!(sunday.idle[0].reference, "19");
    }

    #[test]
    fn a_date_is_the_kind_of_day_its_day_of_the_week_is() {
        assert_eq!(Day::of("20261021"), Some(Day::Weekday));
        assert_eq!(Day::of("20261024"), Some(Day::Saturday));
        assert_eq!(Day::of("20261025"), Some(Day::Sunday));
        assert_eq!(Day::of("2026-10-25"), None);
    }

    #[test]
    fn a_feed_listing_its_days_one_by_one_still_gives_a_day_and_a_colour() {
        // The Marconi Express feed's shape: no calendar.txt, every day it
        // runs listed in calendar_dates.txt, and a colour of its own.
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
        write(
            "routes.txt",
            "route_id,route_short_name,route_long_name,route_type,route_color\n\
             MEX,MEX,Marconi Express,12,C1D72F\n",
        );
        write(
            "trips.txt",
            "route_id,service_id,trip_id,direction_id,shape_id\n\
             MEX,wed,t1,0,s\nMEX,wed,t2,0,s\nMEX,thu,t3,0,s\n",
        );
        write(
            "shapes.txt",
            "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence\ns,44.507,11.343,1\ns,44.529,11.294,2\n",
        );
        write(
            "stop_times.txt",
            "trip_id,stop_id,stop_sequence,departure_time\n\
             t1,a,1,10:00:00\nt2,a,1,13:00:00\nt3,a,1,11:00:00\n",
        );
        write("stops.txt", "stop_id,stop_lat,stop_lon\na,44.507,11.343\n");
        write(
            "calendar_dates.txt",
            "service_id,date,exception_type\nwed,20261021,1\nthu,20261022,1\n",
        );

        let read = lines_from_gtfs(
            dir.path(),
            &["MEX".to_string()],
            None,
            Day::Weekday,
            Mode::Train,
        )
        .unwrap();
        assert_eq!(read.date.as_deref(), Some("20261021"));
        let line = &read.lines[0];
        assert_eq!(line.mode, Mode::Train);
        assert_eq!(line.color.as_deref(), Some("#c1d72f"));
        // Two trips on that Wednesday: 360 minutes / 2.
        assert_eq!(line.headway, Some(180.0));
    }

    #[test]
    fn line_numbers_sort_the_way_a_timetable_lists_them() {
        let mut refs = vec!["N1", "101", "A", "13", "2", "11A", "11", "T2"];
        refs.sort_by(|a, b| line_order(a, b));
        assert_eq!(refs, vec!["2", "11", "11A", "13", "101", "A", "N1", "T2"]);
    }

    #[test]
    fn route_types_tell_buses_from_trains() {
        assert!(Mode::Bus.has_route_type(3));
        assert!(Mode::Bus.has_route_type(704));
        assert!(!Mode::Bus.has_route_type(12));
        assert!(Mode::Train.has_route_type(12));
        assert!(Mode::Train.has_route_type(2));
        assert!(!Mode::Train.has_route_type(3));
    }

    #[test]
    fn intervals_come_as_osm_writes_them() {
        assert_eq!(parse_interval("00:10"), Some(10.0));
        assert_eq!(parse_interval("01:00"), Some(60.0));
        assert_eq!(parse_interval("15"), Some(15.0));
        assert_eq!(parse_interval("00:07:30"), Some(7.5));
        assert_eq!(parse_interval("often"), None);
    }
}
