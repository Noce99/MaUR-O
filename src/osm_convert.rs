//! Turns OpenStreetMap data ([`crate::osm`]) into a Mapper map drawn with
//! [`crate::osm_style`]'s symbol set: the projection, the clipping to an
//! area, and the objects.
//!
//! # Projection
//!
//! The map is placed in the UTM zone of its centre (WGS 84, EPSG:326xx),
//! like most orienteering maps, and properly georeferenced there: Mapper
//! opens it at the right place on the earth. It is then turned so that *true*
//! north is up rather than grid north — openstreetmap.org's Web Mercator
//! tiles are north-up, and away from a zone's central meridian the two
//! differ by a degree or two. That turn is the map's grivation.
//!
//! The scale is the one of the style's level of detail
//! ([`Details::scale`](crate::osm_style::Details::scale)): 1:5 000 by
//! default.
//!
//! # Clipping
//!
//! Everything is cut to the bounding box, in degrees, before it is
//! projected. A line is cut wherever it leaves the box ([`clip_polyline`]);
//! an area which crosses the edge has each of its rings cut to it
//! ([`clip_rings`]), and one wholly inside is kept as it is. A large extract converts whole
//! with no bounding box given: [`crate::osm::OsmData::extent`] then decides
//! what "whole" means.

use std::collections::HashMap;
use std::path::Path;

use crate::bundle;
use crate::bus::{BusLine, Day, Mode, HEADWAY_HOURS};
use crate::map::{coord_flag, h_align, v_align, Coord, Object, ObjectKind, PathObject, TextObject};
use crate::map_areas::{MapArea, MapKind};
use crate::osm::{BBox, OsmData};
use crate::osm_style::{
    line_width_px, map_key, stop_circle_px, Sample, Style, ARROW_PX, MAP_NAME_PX, PIXEL_MM,
};
use crate::xml_writer::{
    color_element, georeferencing_element, symbol_element, Georeference, MapFile,
};

/// The WGS 84 ellipsoid's semi-major axis, in meters.
const WGS84_A: f64 = 6_378_137.0;
/// The WGS 84 ellipsoid's flattening.
const WGS84_F: f64 = 1.0 / 298.257_223_563;
/// UTM's scale on the central meridian.
const UTM_K0: f64 = 0.9996;

/// The UTM zone (1–60) a longitude falls in.
pub fn utm_zone(lon: f64) -> u32 {
    (((lon + 180.0) / 6.0).floor() as i64).clamp(0, 59) as u32 + 1
}

/// `(lon, lat)` in degrees to UTM `(easting, northing)` in meters, in the
/// northern-hemisphere grid of `zone`.
///
/// Krüger's series to the third power of the third flattening, which is
/// good to well under a millimetre within a zone.
pub fn utm(zone: u32, (lon, lat): (f64, f64)) -> (f64, f64) {
    let n = WGS84_F / (2.0 - WGS84_F);
    let big_a = WGS84_A / (1.0 + n) * (1.0 + n * n / 4.0 + n.powi(4) / 64.0);
    let alpha = [
        n / 2.0 - 2.0 * n * n / 3.0 + 5.0 * n.powi(3) / 16.0,
        13.0 * n * n / 48.0 - 3.0 * n.powi(3) / 5.0,
        61.0 * n.powi(3) / 240.0,
    ];
    let lon0 = (f64::from(zone) - 1.0) * 6.0 - 180.0 + 3.0;
    let phi = lat.to_radians();
    let dlambda = (lon - lon0).to_radians();

    let e = 2.0 * n.sqrt() / (1.0 + n);
    let t = (phi.sin().atanh() - e * (e * phi.sin()).atanh()).sinh();
    let xi = t.atan2(dlambda.cos());
    let eta = (dlambda.sin() / (1.0 + t * t).sqrt()).atanh();

    let mut easting = eta;
    let mut northing = xi;
    for (j, a) in alpha.iter().enumerate() {
        let k = 2.0 * (j as f64 + 1.0);
        easting += a * (k * xi).cos() * (k * eta).sinh();
        northing += a * (k * xi).sin() * (k * eta).cosh();
    }
    (
        500_000.0 + UTM_K0 * big_a * easting,
        UTM_K0 * big_a * northing,
    )
}

/// Degrees on the earth to millimetres on the paper.
pub struct Projection {
    /// The UTM zone everything is projected into.
    pub zone: u32,
    /// The map's georeferencing: scale, EPSG code, the projected reference
    /// point at the paper origin, and the grivation turning true north up.
    pub georeference: Georeference,
}

impl Projection {
    /// A projection centred on `(lon, lat)`, at `scale`.
    pub fn new((lon, lat): (f64, f64), scale: i32) -> Projection {
        let zone = utm_zone(lon);
        let (e0, n0) = utm(zone, (lon, lat));

        // True north, in the grid: the way a step north goes.
        let step = 1e-4;
        let (e1, n1) = utm(zone, (lon, lat + step));
        let grivation = (e1 - e0).atan2(n1 - n0).to_degrees();

        // The grid's scale, as the ratio of a short east step in the grid to
        // the same step on the ellipsoid.
        let e2 = WGS84_F * (2.0 - WGS84_F);
        let phi = lat.to_radians();
        let prime_vertical = WGS84_A / (1.0 - e2 * phi.sin().powi(2)).sqrt();
        let (e3, n3) = utm(zone, (lon + step, lat));
        let grid_scale_factor =
            (e3 - e0).hypot(n3 - n0) / (prime_vertical * phi.cos() * step.to_radians());

        Projection {
            zone,
            georeference: Georeference {
                scale,
                epsg: 32600 + zone as i32,
                ref_point: (e0, n0),
                ref_lat_lon: (lat, lon),
                grivation,
                // The map is turned to true north, so magnetic declination
                // aside — which this map has no way of knowing — it is zero.
                declination: 0.0,
                grid_scale_factor,
            },
        }
    }

    /// A position, `(lon, lat)` in degrees, on the paper in mm.
    ///
    /// Mapper's own `Georeferencing::toMapCoords`: grid meters from the
    /// reference point, divided by the grid's scale into ground meters,
    /// turned by the grivation with the paper's `y` pointing down, and
    /// scaled onto the paper.
    pub fn to_paper(&self, position: (f64, f64)) -> (f64, f64) {
        let georef = &self.georeference;
        let (e, n) = utm(self.zone, position);
        let dx = (e - georef.ref_point.0) / georef.grid_scale_factor;
        let dy = (georef.ref_point.1 - n) / georef.grid_scale_factor;
        let (sin_g, cos_g) = georef.grivation.to_radians().sin_cos();
        let mm_per_meter = 1000.0 / f64::from(georef.scale);
        (
            (cos_g * dx + sin_g * dy) * mm_per_meter,
            (-sin_g * dx + cos_g * dy) * mm_per_meter,
        )
    }
}

/// The pieces of a polyline inside `bbox`, in order: one where it stays
/// inside, several where it leaves and comes back, none where it never
/// enters.
///
/// Each segment is cut with Liang and Barsky's parametric clip, so a piece
/// starts and ends exactly on the edge it crosses.
pub fn clip_polyline(points: &[(f64, f64)], bbox: &BBox) -> Vec<Vec<(f64, f64)>> {
    let mut pieces: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut current: Vec<(f64, f64)> = Vec::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        match clip_segment(a, b, bbox) {
            Some((p, q)) => {
                if current.last() != Some(&p) {
                    if current.len() >= 2 {
                        pieces.push(std::mem::take(&mut current));
                    }
                    current.clear();
                    current.push(p);
                }
                current.push(q);
                if q != b {
                    // The segment leaves the box here.
                    pieces.push(std::mem::take(&mut current));
                }
            }
            None => {
                if current.len() >= 2 {
                    pieces.push(std::mem::take(&mut current));
                }
                current.clear();
            }
        }
    }
    if current.len() >= 2 {
        pieces.push(current);
    }
    pieces
}

fn clip_segment(a: (f64, f64), b: (f64, f64), bbox: &BBox) -> Option<((f64, f64), (f64, f64))> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let mut t0: f64 = 0.0;
    let mut t1: f64 = 1.0;
    for (p, q) in [
        (-dx, a.0 - bbox.min_lon),
        (dx, bbox.max_lon - a.0),
        (-dy, a.1 - bbox.min_lat),
        (dy, bbox.max_lat - a.1),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    let at = |t: f64| {
        if t == 0.0 {
            a
        } else if t == 1.0 {
            b
        } else {
            (a.0 + t * dx, a.1 + t * dy)
        }
    };
    Some((at(t0), at(t1)))
}

/// How far inside one edge of a box a point is: negative outside it.
type Inside = fn((f64, f64), &BBox) -> f64;

/// The rings of an area inside `bbox`, closed, holes included.
///
/// Each ring is cut to the box on its own, with Sutherland and Hodgman's
/// clip. The box is convex, so a cut ring is exactly the part of the ring
/// inside it (with stretches running along the box's edge where the ring
/// went outside), and under the even-odd rule Mapper fills an area with,
/// the cut rings together cover exactly what the uncut ones covered inside
/// the box: a hole stays a hole.
pub fn clip_rings(rings: Vec<Vec<(f64, f64)>>, bbox: &BBox) -> Vec<Vec<(f64, f64)>> {
    if rings.iter().flatten().all(|&p| bbox.contains(p)) {
        return rings;
    }
    rings
        .iter()
        .filter_map(|ring| {
            let mut points: Vec<(f64, f64)> = ring[..ring.len() - 1].to_vec();
            let edges: [Inside; 4] = [
                |p, b| p.0 - b.min_lon,
                |p, b| b.max_lon - p.0,
                |p, b| p.1 - b.min_lat,
                |p, b| b.max_lat - p.1,
            ];
            for inside in edges {
                if points.is_empty() {
                    break;
                }
                let mut kept = Vec::with_capacity(points.len() + 4);
                for (i, &current) in points.iter().enumerate() {
                    let previous = points[(i + points.len() - 1) % points.len()];
                    let (d_previous, d_current) = (inside(previous, bbox), inside(current, bbox));
                    if (d_previous >= 0.0) != (d_current >= 0.0) {
                        let t = d_previous / (d_previous - d_current);
                        kept.push((
                            previous.0 + t * (current.0 - previous.0),
                            previous.1 + t * (current.1 - previous.1),
                        ));
                    }
                    if d_current >= 0.0 {
                        kept.push(current);
                    }
                }
                points = kept;
            }
            if points.len() < 3 {
                return None;
            }
            points.push(points[0]);
            Some(points)
        })
        .collect()
}

/// The streets buses run on and the tracks trains do, as the network
/// [`bundle`] matches routes to: every road and railway way of `data` with
/// a node inside `bbox` or near it, node to node, on the paper.
fn transit_network(
    data: &OsmData,
    bbox: &BBox,
    projection: &Projection,
    cell: f64,
) -> bundle::Network {
    const ROADS: &[&str] = &[
        "motorway",
        "motorway_link",
        "trunk",
        "trunk_link",
        "primary",
        "primary_link",
        "secondary",
        "secondary_link",
        "tertiary",
        "tertiary_link",
        "unclassified",
        "residential",
        "living_street",
        "service",
        "pedestrian",
        "road",
        "busway",
        "bus_guideway",
    ];
    const RAILS: &[&str] = &[
        "rail",
        "light_rail",
        "subway",
        "tram",
        "monorail",
        "narrow_gauge",
        "funicular",
    ];
    // A little beyond the map, so that a route is matched right up to its
    // edge.
    let margin = 0.003;
    let around = BBox {
        min_lon: bbox.min_lon - margin,
        min_lat: bbox.min_lat - margin,
        max_lon: bbox.max_lon + margin,
        max_lat: bbox.max_lat + margin,
    };
    let mut paper: HashMap<i64, (f64, f64)> = HashMap::new();
    let mut segments = Vec::new();
    for way in &data.ways {
        if way.tags.is("area", "yes") {
            continue;
        }
        let rail = match (way.tags.get("highway"), way.tags.get("railway")) {
            (Some(h), _) if ROADS.contains(&h) => false,
            (_, Some(r)) if RAILS.contains(&r) => true,
            _ => continue,
        };
        for pair in way.nodes.windows(2) {
            let (Some(&a), Some(&b)) = (data.nodes.get(&pair[0]), data.nodes.get(&pair[1])) else {
                continue;
            };
            if !around.contains(a) && !around.contains(b) {
                continue;
            }
            let mut at =
                |id: i64, p: (f64, f64)| *paper.entry(id).or_insert_with(|| projection.to_paper(p));
            segments.push(bundle::Segment {
                a: at(pair[0], a),
                b: at(pair[1], b),
                nodes: (pair[0], pair[1]),
                rail,
            });
        }
    }
    bundle::Network::new(segments, cell)
}

/// A headway, as written along a line: `ogni 8 min`.
fn headway_text(minutes: f64) -> String {
    format!("ogni {} min", minutes.round().max(1.0) as i64)
}

/// Where a line's headway is written along it, in mm on the paper: the first
/// this far from where the line starts, then every [`HEADWAY_EVERY`], none
/// closer than [`HEADWAY_END`] to where it ends.
const HEADWAY_FIRST: f64 = 40.0;
const HEADWAY_EVERY: f64 = 120.0;
const HEADWAY_END: f64 = 20.0;
/// Half the length of the stretch of line a headway's direction is taken
/// along: about half the text's own length, so that it follows the street
/// it is written on rather than the last corner.
const HEADWAY_HALF: f64 = 6.0;
/// How far a headway may be moved along its line to stay clear of others.
const HEADWAY_SHIFT: f64 = 40.0;
/// How close the centre of anything else may come to a written headway,
/// in mm: the length of `ogni 120 min`, which two such texts running along
/// the same street need between their centres, and a little more.
const HEADWAY_CLEARANCE: f64 = 18.0;

/// The point `distance` mm along a polyline on the paper, clamped to its
/// ends.
fn point_along(points: &[(f64, f64)], distance: f64) -> (f64, f64) {
    let mut left = distance.max(0.0);
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let length = (b.0 - a.0).hypot(b.1 - a.1);
        if left <= length && length > 0.0 {
            let t = left / length;
            return (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
        }
        left -= length;
    }
    *points.last().expect("a polyline has points")
}

/// Where to write a line's headway along one stretch of its route, on the
/// paper: `(x, y, rotation)`, the rotation in radians counter-clockwise, the
/// way Mapper turns a text object, and never more than a quarter turn
/// either way so that the text never reads upside down. A stretch too short
/// for the first spacing gets one in its middle, one shorter than a text gets
/// none. Each one is moved along the line, a few mm at a time, out of every
/// circle of `blocked` (`(x, y, radius)`, in mm: a stop, a number, another
/// text); one with nowhere to go within 40 mm either way is left out.
pub fn along(points: &[(f64, f64)], blocked: &[(f64, f64, f64)]) -> Vec<(f64, f64, f64)> {
    let length: f64 = points
        .windows(2)
        .map(|p| (p[1].0 - p[0].0).hypot(p[1].1 - p[0].1))
        .sum();
    if length < 4.0 * HEADWAY_HALF {
        return Vec::new();
    }
    let mut at = Vec::new();
    let mut distance = HEADWAY_FIRST;
    while distance <= length - HEADWAY_END {
        at.push(distance);
        distance += HEADWAY_EVERY;
    }
    if at.is_empty() {
        at.push(length / 2.0);
    }
    let clear = |distance: f64, taken: &[(f64, f64, f64)]| {
        let (x, y) = point_along(points, distance);
        blocked
            .iter()
            .chain(taken)
            .all(|&(bx, by, radius)| (bx - x).hypot(by - y) > radius)
    };
    let mut taken: Vec<(f64, f64, f64)> = Vec::new();
    at.into_iter()
        .filter_map(|distance| {
            let steps = (HEADWAY_SHIFT / 4.0) as i32;
            let distance = (0..=steps)
                .flat_map(|i| [f64::from(i) * 4.0, -f64::from(i) * 4.0])
                .map(|shift| distance + shift)
                .find(|&d| d >= HEADWAY_HALF && d <= length - HEADWAY_HALF && clear(d, &taken))?;
            let (x, y) = point_along(points, distance);
            taken.push((x, y, HEADWAY_CLEARANCE));
            let (x0, y0) = point_along(points, distance - HEADWAY_HALF);
            let (x1, y1) = point_along(points, distance + HEADWAY_HALF);
            let (mut dx, mut dy) = (x1 - x0, y1 - y0);
            if dx < 0.0 {
                (dx, dy) = (-dx, -dy);
            }
            // The paper's y points down: a line going down to the right is
            // turned clockwise, a negative rotation.
            Some((x, y, (-dy).atan2(dx)))
        })
        .collect()
}

/// How far in from the edge of the map, in mm along the line, the number of
/// a route the edge cuts belongs.
const EXIT_STEP: f64 = 5.0;

/// Whether a point lies on the edge of `bbox`, as a route cut by
/// [`clip_polyline`] starts or ends there.
fn on_edge((lon, lat): (f64, f64), bbox: &BBox) -> bool {
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    close(lon, bbox.min_lon)
        || close(lon, bbox.max_lon)
        || close(lat, bbox.min_lat)
        || close(lat, bbox.max_lat)
}

/// How large a line's number is on the paper, in mm, badge and all: about
/// 2.2 mm a character, and the badge's margin; 4.4 mm high.
fn label_size(text: &str) -> (f64, f64) {
    (text.chars().count() as f64 * 2.2 + 2.2, LABEL_HEIGHT)
}

/// How high a line's number is, badge and all, in mm on the paper.
const LABEL_HEIGHT: f64 = 4.4;

/// How wide a short line's number is, in mm: what a headway keeps clear of
/// one by.
const LABEL_WIDTH: f64 = 8.0;

/// How far from where it belongs a line's number may go to find room, in
/// mm, and in which steps.
const LABEL_REACH: f64 = 25.0;
const LABEL_STEP: f64 = 0.75;

/// What a line's number has to keep clear of — every line, stop and arrow
/// drawn, as dots of some radius, and the numbers already placed — and the
/// map's frame it has to stay inside, all in mm on the paper.
struct Clutter {
    /// The dots, by the [`CLUTTER_CELL`]-mm square they are in.
    cells: HashMap<(i32, i32), Dots>,
    /// The numbers placed, as `(min x, min y, max x, max y)`.
    boxes: Vec<(f64, f64, f64, f64)>,
    /// The corners of the map, in order round it.
    frame: Vec<(f64, f64)>,
}

/// What a number over another counts as, in dots of line covered: a number
/// rather covers a stretch of line than one it cannot be read under.
const LABEL_OVER_LABEL: usize = 1000;

/// Dots, as `(x, y, radius)` in mm.
type Dots = Vec<(f64, f64, f64)>;

/// The side of a [`Clutter`] cell, in mm.
const CLUTTER_CELL: f64 = 4.0;

/// The widest a [`Clutter`] dot may be, in mm: what a box is looked round
/// by for dots reaching into it.
const CLUTTER_RADIUS: f64 = 1.5;

impl Clutter {
    fn new(frame: Vec<(f64, f64)>) -> Clutter {
        Clutter {
            cells: HashMap::new(),
            boxes: Vec::new(),
            frame,
        }
    }

    fn cell(x: f64, y: f64) -> (i32, i32) {
        (
            (x / CLUTTER_CELL).floor() as i32,
            (y / CLUTTER_CELL).floor() as i32,
        )
    }

    fn dot(&mut self, (x, y): (f64, f64), radius: f64) {
        let radius = radius.min(CLUTTER_RADIUS);
        self.cells
            .entry(Clutter::cell(x, y))
            .or_default()
            .push((x, y, radius));
    }

    /// A line `radius` wide either side: a dot every half mm along it.
    fn line(&mut self, points: &[(f64, f64)], radius: f64) {
        for w in points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let steps = ((b.0 - a.0).hypot(b.1 - a.1) / 0.5).ceil().max(1.0) as usize;
            for i in 0..steps {
                let t = i as f64 / steps as f64;
                self.dot((a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1)), radius);
            }
        }
        if let Some(&last) = points.last() {
            self.dot(last, radius);
        }
    }

    /// How much a box `(min x, min y, max x, max y)` would cover: the dots
    /// it overlaps, and [`LABEL_OVER_LABEL`] for each number, or
    /// `usize::MAX` where it is not all inside the frame. Counting stops at
    /// `enough`.
    fn covers(&self, b: (f64, f64, f64, f64), enough: usize) -> usize {
        let inside = |(x, y): (f64, f64)| {
            let n = self.frame.len();
            let sides: Vec<f64> = (0..n)
                .map(|i| {
                    let (p, q) = (self.frame[i], self.frame[(i + 1) % n]);
                    (q.0 - p.0) * (y - p.1) - (q.1 - p.1) * (x - p.0)
                })
                .collect();
            sides.iter().all(|&s| s >= 0.0) || sides.iter().all(|&s| s <= 0.0)
        };
        if ![(b.0, b.1), (b.2, b.1), (b.2, b.3), (b.0, b.3)]
            .into_iter()
            .all(inside)
        {
            return usize::MAX;
        }
        let mut count = LABEL_OVER_LABEL
            * self
                .boxes
                .iter()
                .filter(|o| o.0 < b.2 && b.0 < o.2 && o.1 < b.3 && b.1 < o.3)
                .count();
        let (lo, hi) = (
            Clutter::cell(b.0 - CLUTTER_RADIUS, b.1 - CLUTTER_RADIUS),
            Clutter::cell(b.2 + CLUTTER_RADIUS, b.3 + CLUTTER_RADIUS),
        );
        for cx in lo.0..=hi.0 {
            for cy in lo.1..=hi.1 {
                for &(x, y, r) in self.cells.get(&(cx, cy)).into_iter().flatten() {
                    if count >= enough {
                        return count;
                    }
                    let dx = (b.0 - x).max(x - b.2).max(0.0);
                    let dy = (b.1 - y).max(y - b.3).max(0.0);
                    if dx.hypot(dy) < r {
                        count += 1;
                    }
                }
            }
        }
        count
    }

    /// Where a number `size` large goes for `anchor`: centred on it where
    /// that is clear, otherwise the nearest spot round it, out to
    /// [`LABEL_REACH`], that covers nothing; where there is none, the one
    /// covering least. The number is then in the way of the next.
    fn place(&mut self, anchor: (f64, f64), size: (f64, f64)) -> (f64, f64) {
        let (hw, hh) = (size.0 / 2.0, size.1 / 2.0);
        let at = |c: (f64, f64)| (c.0 - hw, c.1 - hh, c.0 + hw, c.1 + hh);
        let mut best: Option<(usize, (f64, f64))> = None;
        let rings = (LABEL_REACH / LABEL_STEP) as usize;
        'search: for ring in 0..=rings {
            let radius = ring as f64 * LABEL_STEP;
            // More directions further out, about one every step of the way
            // round.
            let around = if ring == 0 {
                1
            } else {
                (std::f64::consts::TAU * radius / LABEL_STEP).ceil() as usize
            };
            for k in 0..around {
                let angle = std::f64::consts::TAU * k as f64 / around as f64;
                let c = (
                    anchor.0 + radius * angle.cos(),
                    anchor.1 + radius * angle.sin(),
                );
                let enough = best.map_or(usize::MAX, |(n, _)| n);
                let n = self.covers(at(c), enough);
                if best.is_none_or(|(b, _)| n < b) {
                    best = Some((n, c));
                }
                if n == 0 {
                    break 'search;
                }
            }
        }
        let c = best.map_or(anchor, |(_, c)| c);
        let b = at(c);
        // A little room round it.
        self.boxes
            .push((b.0 - 0.3, b.1 - 0.3, b.2 + 0.3, b.3 + 0.3));
        c
    }
}

/// What [`convert`] makes of an extract.
pub struct Conversion {
    /// Where the map sits, and how degrees become millimetres.
    pub projection: Projection,
    /// The area converted.
    pub bbox: BBox,
    /// Every object, the land under everything first.
    pub objects: Vec<Object>,
    /// How many objects are areas, lines and points, in that order.
    pub counts: (usize, usize, usize),
}

impl Conversion {
    /// The map's scale denominator.
    pub fn scale(&self) -> i32 {
        self.projection.georeference.scale
    }

    /// The resolution, in pixels per meter on the ground, at which a
    /// rendering of the map draws one carto pixel as one pixel, as
    /// openstreetmap.org's own tiles do.
    pub fn tile_resolution(&self) -> f64 {
        1000.0 / (f64::from(self.scale()) * PIXEL_MM)
    }

    /// Adds bus and train lines over the map: each line's route, its stops,
    /// and its number at either end of the route (once, for a circular line)
    /// and wherever the edge of the map cuts the route, with the symbols
    /// `style` keys by the line's kind and number (see
    /// [`carto_style_with_bus_lines`](crate::osm_style::carto_style_with_bus_lines)).
    ///
    /// Lines sharing a street are drawn side by side on it, in the order of
    /// their numbers, rather than one over the other: [`crate::bundle`]
    /// matches every route to `data`'s streets and tracks and lays the
    /// lines out, none of its bundles wider than `bundle_cap` carto pixels.
    /// A stop on a bundle is one bar across it, and stops less than
    /// `stop_merge` mm apart along it one bar too, with an arrow at the
    /// end on each stop's side pointing the way its buses run; one with no
    /// bundle to cross is a circle, with an arrow beside it. Everything is
    /// clipped to the map's area, like
    /// the rest of it; a number goes only at an end which lies inside, or
    /// where the edge cuts the route.
    pub fn add_bus_lines(
        &mut self,
        style: &Style,
        lines: &[BusLine],
        data: &OsmData,
        bundle_cap: f64,
        stop_merge: f64,
    ) {
        let bbox = self.bbox;
        let mm = 1000.0 / f64::from(self.projection.georeference.scale);
        let settings = bundle::Settings {
            sample: 2.0 * mm,
            tolerance: 15.0 * mm,
            max_angle: 35f64.to_radians(),
            jump: 4.0 * mm,
            smooth: 1.5,
            // Half the widest a bundle may be, and a little more: a corner
            // rounded that much takes the inside line round it too.
            round: (bundle_cap * PIXEL_MM / 2.0 + 0.5).max(1.0),
            cap: bundle_cap * PIXEL_MM,
            stop_reach: 25.0 * mm,
            bar_overhang: PIXEL_MM,
            stop_merge,
            stop_side: 1.5 * mm,
            // The bar's round end, a gap, and half the arrow's width.
            arrow_gap: (1.2 + 0.8 + ARROW_PX.1 / 2.0) * PIXEL_MM,
            arrow_length: ARROW_PX.0 * PIXEL_MM,
        };
        let network = transit_network(data, &bbox, &self.projection, 20.0 * mm);

        // Every stretch of every line inside the map, ranked by the line's
        // number, buses before trains.
        let mut ranked: Vec<usize> = (0..lines.len()).collect();
        ranked.sort_by(|&a, &b| {
            (lines[a].mode != Mode::Bus)
                .cmp(&(lines[b].mode != Mode::Bus))
                .then_with(|| crate::bus::line_order(&lines[a].reference, &lines[b].reference))
        });
        let rank_of: HashMap<usize, usize> = ranked
            .iter()
            .enumerate()
            .map(|(rank, &l)| (l, rank))
            .collect();
        let mut routes: Vec<bundle::RouteInput> = Vec::new();
        // For each stretch: whether the edge of the map cuts its start and
        // its end.
        let mut cut: Vec<(bool, bool)> = Vec::new();
        // For each stretch: whether it is of the line's own route, rather
        // than of the way back where it leaves it.
        let mut on_route: Vec<bool> = Vec::new();
        for (l, line) in lines.iter().enumerate() {
            let pieces = line
                .route
                .iter()
                .map(|p| (p, true))
                .chain(line.detours.iter().map(|p| (p, false)));
            for (piece, own) in pieces {
                for clipped in clip_polyline(piece, &bbox) {
                    on_route.push(own);
                    routes.push(bundle::RouteInput {
                        line: l,
                        rank: rank_of[&l],
                        rail: line.mode == Mode::Train,
                        width: line_width_px(line.mode) * PIXEL_MM,
                        points: clipped
                            .iter()
                            .map(|&p| self.projection.to_paper(p))
                            .collect(),
                    });
                    cut.push((
                        on_edge(clipped[0], &bbox),
                        on_edge(clipped[clipped.len() - 1], &bbox),
                    ));
                }
            }
        }
        let laid = bundle::lay_out(&network, &routes, &settings);

        // The routes, each in pieces of one width.
        let key_of = |l: usize| format!("{} {}", lines[l].mode.key(), lines[l].reference);
        for (route, l) in routes.iter().zip(&laid) {
            let key = key_of(route.line);
            for (class, points) in l.pieces() {
                let symbol = if class == 0 {
                    style.id(&key)
                } else {
                    style.id(&format!("{key} w{class}"))
                };
                let coords = points.iter().map(|&(x, y)| Coord::new(x, y, 0)).collect();
                self.objects.push(path_object(symbol, coords));
                self.counts.1 += 1;
            }
        }

        // The stops: bars across the bundles, circles off them, and the
        // arrows beside both.
        let stops_of: HashMap<usize, Vec<bundle::StopInput>> = lines
            .iter()
            .enumerate()
            .map(|(l, line)| {
                let inside = line
                    .stops
                    .iter()
                    .filter(|stop| bbox.contains(stop.at))
                    .map(|stop| {
                        let at = self.projection.to_paper(stop.at);
                        let heading = stop.toward.and_then(|toward| {
                            let ahead = self.projection.to_paper(toward);
                            let (dx, dy) = (ahead.0 - at.0, ahead.1 - at.1);
                            let length = dx.hypot(dy);
                            (length > 0.0).then(|| (dx / length, dy / length))
                        });
                        bundle::StopInput { at, heading }
                    })
                    .collect();
                (l, inside)
            })
            .collect();
        let laid_stops = bundle::stops(&routes, &laid, &stops_of, &settings);
        let bars = &laid_stops.bars;
        let mut arrows: Vec<bundle::Arrow> = Vec::new();
        if !bars.is_empty() {
            let bar = style.id("transit stop bar");
            for b in bars {
                let coords = b.ends.iter().map(|&(x, y)| Coord::new(x, y, 0)).collect();
                self.objects.push(path_object(bar, coords));
                self.counts.2 += 1;
                arrows.extend(&b.arrows);
            }
        }
        for circle in &laid_stops.circles {
            // One line's own circle, or a neutral one shared by several.
            let (symbol, mode) = match circle.lines[..] {
                [l] => (style.id(&format!("{} stop", key_of(l))), lines[l].mode),
                _ => (style.id("transit stop"), Mode::Bus),
            };
            let mut object = Object::new(ObjectKind::Point);
            object.symbol_id = symbol;
            object.coords = vec![Coord::new(circle.centre.0, circle.centre.1, 0)];
            self.objects.push(object);
            self.counts.2 += 1;
            // On the right of the way the buses run, clear of the ring.
            let (radius, ring) = stop_circle_px(mode);
            let reach = (radius + ring + 0.8 + ARROW_PX.1 / 2.0) * PIXEL_MM;
            arrows.extend(circle.headings.iter().map(|&(hx, hy)| bundle::Arrow {
                at: (circle.centre.0 - hy * reach, circle.centre.1 + hx * reach),
                heading: (hx, hy),
            }));
        }
        if !arrows.is_empty() {
            let symbol = style.id("transit stop arrow");
            for arrow in &arrows {
                let mut object = Object::new(ObjectKind::Point);
                object.symbol_id = symbol;
                object.coords = vec![Coord::new(arrow.at.0, arrow.at.1, 0)];
                // The paper's y points down; a rotation turns counter-clockwise.
                object.rotation = (-arrow.heading.1).atan2(arrow.heading.0);
                self.objects.push(object);
                self.counts.2 += 1;
            }
        }

        // The numbers, each as near where it belongs as it can be without
        // covering a line, a stop, an arrow or another number.
        let corners = [
            (bbox.min_lon, bbox.min_lat),
            (bbox.max_lon, bbox.min_lat),
            (bbox.max_lon, bbox.max_lat),
            (bbox.min_lon, bbox.max_lat),
        ];
        let mut clutter = Clutter::new(
            corners
                .iter()
                .map(|&p| self.projection.to_paper(p))
                .collect(),
        );
        for (route, l) in routes.iter().zip(&laid) {
            clutter.line(&l.points, route.width / 2.0 + 0.2);
        }
        for bar in bars {
            clutter.line(&bar.ends, 0.5);
        }
        for circle in &laid_stops.circles {
            clutter.dot(circle.centre, 1.3);
        }
        for arrow in &arrows {
            clutter.dot(arrow.at, 0.8);
        }
        let mut placed: Vec<(f64, f64)> = Vec::new();

        // At the termini: the ends of a line's own route the edge of the
        // map does not cut.
        for (l, line) in lines.iter().enumerate() {
            let own: Vec<usize> = (0..routes.len())
                .filter(|&r| routes[r].line == l && on_route[r])
                .collect();
            let (Some(&first), Some(&last)) = (own.first(), own.last()) else {
                continue;
            };
            let mut ends: Vec<(f64, f64)> = Vec::new();
            if !cut[first].0 {
                ends.push(laid[first].points[0]);
            }
            if !cut[last].1 {
                let end = *laid[last].points.last().expect("a route has points");
                // Closer than a label is wide, the two ends are one terminus.
                if ends.iter().all(|s| (s.0 - end.0).hypot(s.1 - end.1) >= 5.0) {
                    ends.push(end);
                }
            }
            let label = style.id(&format!("{} label", key_of(l)));
            for end in ends {
                let (x, y) = clutter.place(end, label_size(&line.reference));
                placed.push((x, y));
                self.push_label(label, &line.reference, x, y);
            }
        }

        // Where the edge of the map cuts a route: a little way in along it.
        for (r, route) in routes.iter().enumerate() {
            let paper = &laid[r].points;
            let reference = &lines[route.line].reference;
            let label = style.id(&format!("{} label", key_of(route.line)));
            let length: f64 = paper
                .windows(2)
                .map(|p| (p[1].0 - p[0].0).hypot(p[1].1 - p[0].1))
                .sum();
            for (at_start, is_cut) in [(true, cut[r].0), (false, cut[r].1)] {
                if !is_cut {
                    continue;
                }
                let d = EXIT_STEP.min(length / 2.0);
                let anchor = point_along(paper, if at_start { d } else { length - d });
                let (x, y) = clutter.place(anchor, label_size(reference));
                placed.push((x, y));
                self.push_label(label, reference, x, y);
            }
        }

        // The headways last, each kept off the stops, off every number, and
        // off every headway already written — two lines sharing a street
        // would otherwise print theirs in the same place.
        let mut written: Vec<(f64, f64, f64)> =
            placed.iter().map(|&(x, y)| (x, y, LABEL_WIDTH)).collect();
        written.extend(
            bars.iter()
                .map(|b| (b.centre.0, b.centre.1, HEADWAY_HALF + 1.5)),
        );
        for (r, route) in routes.iter().enumerate() {
            let line = &lines[route.line];
            let Some(minutes) = line.headway else {
                continue;
            };
            let text = headway_text(minutes);
            let symbol = style.id(&format!("{} headway", key_of(route.line)));
            let mut blocked = written.clone();
            if let Some(stops) = laid_stops.drawn_at.get(&route.line) {
                blocked.extend(stops.iter().map(|&(x, y)| (x, y, HEADWAY_HALF + 1.5)));
            }
            for (x, y, rotation) in along(&laid[r].points, &blocked) {
                written.push((x, y, HEADWAY_CLEARANCE));
                blocked.push((x, y, HEADWAY_CLEARANCE));
                let mut object = Object::new(ObjectKind::Text(TextObject {
                    text: text.clone(),
                    h_align: h_align::HCENTER,
                    v_align: v_align::VCENTER,
                    box_size: None,
                }));
                object.symbol_id = symbol;
                object.rotation = rotation;
                object.coords = vec![Coord::new(x, y, 0)];
                self.objects.push(object);
                self.counts.2 += 1;
            }
        }
    }

    /// A line's number, as written at a terminus or where the map's edge cuts
    /// its route.
    fn push_label(&mut self, symbol: i32, reference: &str, x: f64, y: f64) {
        let mut object = Object::new(ObjectKind::Text(TextObject {
            text: reference.to_string(),
            h_align: h_align::HCENTER,
            v_align: v_align::VCENTER,
            box_size: None,
        }));
        object.symbol_id = symbol;
        object.coords = vec![Coord::new(x, y, 0)];
        self.objects.push(object);
        self.counts.2 += 1;
    }

    /// Adds the areas of an event's maps over the map: each map's areas
    /// with the symbol `map <i>`, and its name with `map <i> name` once (see
    /// [`carto_style_with_overlays`](crate::osm_style::carto_style_with_overlays)).
    /// Clipped to the map's area like the rest of it.
    ///
    /// A name goes inside one of its map's areas wherever it fits there
    /// whole, as far from every map's outline as it can, level or turned
    /// along a long thin area; where it fits nowhere inside, beside the area,
    /// just outside it, clear of the other maps' areas, of the names already
    /// placed and of the map's edge.
    ///
    /// A forbidden zone is drawn the same, its name left out, and the start
    /// and the finish are points.
    ///
    /// Returns the names of the maps with no area on the map at all, which
    /// are left out, and of the start or finish off it.
    pub fn add_map_areas(&mut self, style: &Style, maps: &[MapArea]) -> Vec<String> {
        let mut missing = Vec::new();
        // Each map's areas on the paper, clipped; the forbidden zones' too,
        // for the names to keep clear of.
        let mut drawn: Vec<(usize, Vec<Ring>)> = Vec::new();
        let mut forbidden: Vec<Ring> = Vec::new();
        for (i, map) in maps.iter().enumerate() {
            let area_symbol = style.id(&map_key(map, i));
            if map.kind.is_point() {
                let at = map.polygons[0][0];
                if !self.bbox.contains(at) {
                    missing.push(map.name.clone());
                    continue;
                }
                let (x, y) = self.projection.to_paper(at);
                let mut object = Object::new(ObjectKind::Point);
                object.symbol_id = area_symbol;
                object.coords = vec![Coord::new(x, y, 0)];
                self.objects.push(object);
                self.counts.2 += 1;
                continue;
            }
            let polygons: Vec<Vec<(f64, f64)>> = map
                .polygons
                .iter()
                .flat_map(|polygon| clip_rings(vec![polygon.clone()], &self.bbox))
                .map(|ring| ring.iter().map(|&p| self.projection.to_paper(p)).collect())
                .collect();
            if polygons.is_empty() {
                missing.push(map.name.clone());
                continue;
            }
            for ring in &polygons {
                let last = ring.len() - 1;
                let coords = ring
                    .iter()
                    .enumerate()
                    .map(|(j, &(x, y))| {
                        let flags = if j == last {
                            coord_flag::CLOSE_POINT
                        } else {
                            0
                        };
                        Coord::new(x, y, flags)
                    })
                    .collect();
                self.objects.push(path_object(area_symbol, coords));
                self.counts.0 += 1;
            }
            if map.kind == MapKind::Forbidden {
                forbidden.extend(polygons);
            } else {
                drawn.push((i, polygons));
            }
        }

        let frame: Vec<(f64, f64)> = [
            (self.bbox.min_lon, self.bbox.min_lat),
            (self.bbox.max_lon, self.bbox.min_lat),
            (self.bbox.max_lon, self.bbox.max_lat),
            (self.bbox.min_lon, self.bbox.max_lat),
            (self.bbox.min_lon, self.bbox.min_lat),
        ]
        .iter()
        .map(|&p| self.projection.to_paper(p))
        .collect();
        let outlines: Vec<&[(f64, f64)]> = drawn
            .iter()
            .flat_map(|(_, rings)| rings.iter())
            .chain(&forbidden)
            .map(|r| r.as_slice())
            .collect();
        // The names which fit inside their areas first, then the others
        // round them.
        let mut placed: Vec<NameBox> = Vec::new();
        let mut names: Vec<Option<NameBox>> = vec![None; drawn.len()];
        for inside in [true, false] {
            for (k, (i, rings)) in drawn.iter().enumerate() {
                if names[k].is_some() {
                    continue;
                }
                let others: Vec<&[(f64, f64)]> = drawn
                    .iter()
                    .filter(|(j, _)| j != i)
                    .flat_map(|(_, rings)| rings.iter())
                    .chain(&forbidden)
                    .map(|r| r.as_slice())
                    .collect();
                let around = Around {
                    outlines: &outlines,
                    others: &others,
                    placed: &placed,
                    frame: &frame,
                };
                let size = map_name_size(&maps[*i].name);
                let found = if inside {
                    place_inside(rings, size, &around)
                } else {
                    Some(place_beside(rings, size, &around))
                };
                if let Some(name) = found {
                    placed.push(name.clone());
                    names[k] = Some(name);
                }
            }
        }
        for ((i, _), name) in drawn.iter().zip(names) {
            let name = name.expect("every name placed");
            let mut object = Object::new(ObjectKind::Text(TextObject {
                text: maps[*i].name.clone(),
                h_align: h_align::HCENTER,
                v_align: v_align::VCENTER,
                box_size: None,
            }));
            object.symbol_id = style.id(&format!("map {i} name"));
            object.coords = vec![Coord::new(name.center.0, name.center.1, 0)];
            object.rotation = name.rotation;
            self.objects.push(object);
            self.counts.2 += 1;
        }
        missing
    }

    /// Adds a legend under the map: a sample of every symbol the map uses,
    /// each with what it means, in columns as wide as the map allows.
    ///
    /// Under the map rather than over it: in Mapper a colour is drawn at the
    /// same level everywhere, so a panel laid over the map to hold the legend
    /// would hide the legend's own samples along with the map. The bus and
    /// train lines come first, each with its name from `lines`, then an
    /// entry for their stops and one for the numbers at their ends; then
    /// everything else, in the order the style defines it. The frequencies
    /// are said to be those of `day`.
    pub fn add_legend(&mut self, style: &Style, lines: &[BusLine], day: Day) {
        let background = style.background();
        let Some(land) = self.objects.iter().find(|o| o.symbol_id == background) else {
            return;
        };
        let min_x = land
            .coords
            .iter()
            .map(|c| c.x)
            .fold(f64::INFINITY, f64::min);
        let max_x = land
            .coords
            .iter()
            .map(|c| c.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let max_y = land
            .coords
            .iter()
            .map(|c| c.y)
            .fold(f64::NEG_INFINITY, f64::max);
        let used: std::collections::HashSet<i32> =
            self.objects.iter().map(|o| o.symbol_id).collect();

        // (symbol, sample, caption, the text of a text sample)
        let mut entries: Vec<(i32, Sample, String, String)> = Vec::new();
        let mut first_stop = None;
        let mut first_label = None;
        let mut first_headway = None;
        for line in lines {
            let key = format!("{} {}", line.mode.key(), line.reference);
            let id = style.id(&key);
            // Drawn at any width: in a wide bundle only thinner.
            let drawn = used.contains(&id)
                || (1..crate::bundle::WIDTH_FACTORS.len())
                    .any(|class| used.contains(&style.id(&format!("{key} w{class}"))));
            if !drawn {
                continue;
            }
            let name = legend_name(&line.name, &line.reference);
            let caption = match style.caption(id) {
                Some(caption) if !name.is_empty() => format!("{caption}: {name}"),
                Some(caption) => caption,
                None => continue,
            };
            entries.push((id, Sample::Line, caption, String::new()));
            let stop = style.id(&format!("{key} stop"));
            if first_stop.is_none() && used.contains(&stop) {
                first_stop = Some(stop);
            }
            let label = style.id(&format!("{key} label"));
            if first_label.is_none() && used.contains(&label) {
                first_label = Some((label, line.reference.clone()));
            }
            let headway = style.id(&format!("{key} headway"));
            if first_headway.is_none() && used.contains(&headway) {
                first_headway = line.headway.map(|m| (headway, headway_text(m)));
            }
        }
        // A stop: the bar across a bundle, and the circle of a stop off it
        // where there is one.
        if !lines.is_empty() {
            let bar = style.id("transit stop bar");
            if used.contains(&bar) {
                entries.push((
                    bar,
                    Sample::Bar,
                    "Fermata, stazione".to_string(),
                    String::new(),
                ));
            }
        }
        if let Some(stop) = first_stop {
            entries.push((
                stop,
                Sample::Point,
                "Fermata fuori dal percorso".to_string(),
                String::new(),
            ));
        }
        if !lines.is_empty() {
            for (key, caption) in [
                ("transit stop", "Fermata di più linee fuori dal percorso"),
                ("transit stop arrow", "Senso di marcia alla fermata"),
            ] {
                let id = style.id(key);
                if used.contains(&id) {
                    entries.push((id, Sample::Point, caption.to_string(), String::new()));
                }
            }
        }
        if let Some((label, reference)) = first_label {
            entries.push((
                label,
                Sample::Text,
                "Numero: capolinea o bordo mappa".to_string(),
                reference,
            ));
        }
        if let Some((headway, text)) = first_headway {
            entries.push((
                headway,
                Sample::Text,
                format!(
                    "Frequenza media {}–{} {}",
                    HEADWAY_HOURS.0,
                    HEADWAY_HOURS.1,
                    match day {
                        Day::Weekday => "feriali",
                        Day::Saturday => "il sabato",
                        Day::Sunday => "festivi",
                    }
                ),
                text,
            ));
        }
        let mut ids: Vec<i32> = used.iter().copied().collect();
        ids.sort_unstable();
        for id in ids {
            if style
                .key(id)
                .is_some_and(|k| k.starts_with("bus ") || k.starts_with("train "))
            {
                continue;
            }
            let (Some(caption), Some(sample)) = (style.caption(id), style.sample(id)) else {
                continue;
            };
            // Two symbols the legend cannot tell apart in words — a paved and
            // an unknown cycleway — are listed once.
            if entries.iter().all(|(_, _, c, _)| *c != caption) {
                entries.push((id, sample, caption, String::new()));
            }
        }
        if entries.is_empty() {
            return;
        }

        const COLUMN: f64 = 70.0;
        const ROW: f64 = 5.5;
        const SAMPLE: f64 = 9.0;
        const CAPTION_CHARS: usize = 34;
        // About how wide one character of a caption is, in mm.
        const CAPTION_CHAR_WIDTH: f64 = 1.6;
        let columns = (((max_x - min_x) / COLUMN).floor() as usize).max(1);
        let rows = entries.len().div_ceil(columns);
        let top = max_y + 10.0;

        let text = |symbol: i32, x: f64, y: f64, text: String, align: i32| {
            let mut object = Object::new(ObjectKind::Text(TextObject {
                text,
                h_align: align,
                v_align: v_align::VCENTER,
                box_size: None,
            }));
            object.symbol_id = symbol;
            object.coords = vec![Coord::new(x, y, 0)];
            object
        };
        self.objects.push(text(
            style.id("legend title"),
            min_x,
            top,
            "Legenda".to_string(),
            h_align::LEFT,
        ));
        let caption_symbol = style.id("legend text");
        for (i, (symbol, sample, caption, sample_text)) in entries.into_iter().enumerate() {
            // About how wide a text sample is: its badge's bold characters
            // and the blob around them.
            let sample_width = if sample == Sample::Text {
                sample_text.chars().count() as f64 * 1.25 + 1.5
            } else {
                0.0
            };
            let x = min_x + (i / rows) as f64 * COLUMN;
            let y = top + 9.0 + (i % rows) as f64 * ROW;
            let object = match sample {
                Sample::Area => {
                    let (left, right, up, down) = (x, x + SAMPLE, y - 2.0, y + 2.0);
                    path_object(
                        symbol,
                        vec![
                            Coord::new(left, up, 0),
                            Coord::new(right, up, 0),
                            Coord::new(right, down, 0),
                            Coord::new(left, down, 0),
                            Coord::new(left, up, coord_flag::CLOSE_POINT),
                        ],
                    )
                }
                Sample::Line => path_object(
                    symbol,
                    vec![Coord::new(x, y, 0), Coord::new(x + SAMPLE, y, 0)],
                ),
                Sample::Bar => path_object(
                    symbol,
                    vec![
                        Coord::new(x + SAMPLE / 2.0, y - 1.6, 0),
                        Coord::new(x + SAMPLE / 2.0, y + 1.6, 0),
                    ],
                ),
                Sample::Point => {
                    let mut object = Object::new(ObjectKind::Point);
                    object.symbol_id = symbol;
                    object.coords = vec![Coord::new(x + SAMPLE / 2.0, y, 0)];
                    object
                }
                // A text wider than a sample — `ogni 8 min` — starts where
                // the samples do and pushes its caption along.
                Sample::Text if sample_width > SAMPLE => {
                    text(symbol, x, y, sample_text, h_align::LEFT)
                }
                Sample::Text => text(symbol, x + SAMPLE / 2.0, y, sample_text, h_align::HCENTER),
            };
            self.objects.push(object);
            let width = sample_width.max(SAMPLE);
            let chars = CAPTION_CHARS - ((width - SAMPLE) / CAPTION_CHAR_WIDTH).ceil() as usize;
            let caption = if caption.chars().count() > chars {
                let cut: String = caption.chars().take(chars - 1).collect();
                format!("{}…", cut.trim_end())
            } else {
                caption
            };
            self.objects.push(text(
                caption_symbol,
                x + width + 3.0,
                y,
                caption,
                h_align::LEFT,
            ));
        }
    }

    /// Writes the map, with `style`'s colours and symbols, as an `.omap`.
    pub fn write(&self, style: &Style, path: &Path) -> Result<(), String> {
        let fragment = |id: i32, text: String| crate::xml_reader::Fragment { id, text };
        let colors: Vec<_> = style
            .colors
            .iter()
            .enumerate()
            .map(|(i, color)| fragment(-1, color_element(i, color)))
            .collect();
        let symbols: Vec<_> = style
            .symbols
            .iter()
            .map(|(id, symbol)| fragment(*id, symbol_element(*id, symbol)))
            .collect();
        MapFile {
            scale_denominator: self.scale(),
            colors: &colors,
            symbols: &symbols,
            objects: &self.objects,
            // The headways written along the lines are turned with them.
            rotatable: true,
            georeferencing: Some(georeferencing_element(&self.projection.georeference)),
        }
        .write(path)
    }
}

/// Converts everything in `bbox` (or the whole extract, where `None`) into
/// objects drawn with `style`.
pub fn convert(data: &OsmData, style: &Style, bbox: Option<BBox>) -> Result<Conversion, String> {
    let bbox = bbox
        .or_else(|| data.extent())
        .ok_or("the extract holds nothing to convert")?;
    let (center_lon, center_lat) = bbox.center();
    let scale = style.details.scale();
    let projection = Projection::new((center_lon, center_lat), scale);
    let paper = |p: (f64, f64)| {
        let (x, y) = projection.to_paper(p);
        (x, y)
    };

    let area_object = |symbol: i32, rings: &[Vec<(f64, f64)>]| {
        let mut coords = Vec::new();
        for (i, ring) in rings.iter().enumerate() {
            let last = ring.len() - 1;
            for (j, &p) in ring.iter().enumerate() {
                let (x, y) = paper(p);
                let mut flags = 0;
                if j == last {
                    flags = coord_flag::CLOSE_POINT;
                    if i + 1 < rings.len() {
                        flags |= coord_flag::HOLE_POINT;
                    }
                }
                coords.push(Coord::new(x, y, flags));
            }
        }
        path_object(symbol, coords)
    };
    let line_object = |symbol: i32, points: &[(f64, f64)]| {
        let closed = points.len() >= 4 && points.first() == points.last();
        let last = points.len() - 1;
        let coords = points
            .iter()
            .enumerate()
            .map(|(j, &p)| {
                let (x, y) = paper(p);
                let flags = if closed && j == last {
                    coord_flag::CLOSE_POINT
                } else {
                    0
                };
                Coord::new(x, y, flags)
            })
            .collect();
        path_object(symbol, coords)
    };

    let mut areas = Vec::new();
    let mut lines = Vec::new();
    let mut points = Vec::new();

    let corners = vec![
        (bbox.min_lon, bbox.min_lat),
        (bbox.max_lon, bbox.min_lat),
        (bbox.max_lon, bbox.max_lat),
        (bbox.min_lon, bbox.max_lat),
        (bbox.min_lon, bbox.min_lat),
    ];
    areas.push(area_object(style.background(), &[corners]));

    for relation in &data.relations {
        if !relation.tags.is("type", "multipolygon") {
            continue;
        }
        let symbols = style.area_symbols(&relation.tags, true);
        if symbols.is_empty() {
            continue;
        }
        let rings = clip_rings(data.multipolygon_rings(relation), &bbox);
        if rings.is_empty() {
            continue;
        }
        for symbol in symbols {
            areas.push(area_object(symbol, &rings));
        }
    }

    for way in &data.ways {
        let area_symbols = if way.is_closed() && !way.tags.is("area", "no") {
            style.area_symbols(&way.tags, false)
        } else {
            Vec::new()
        };
        let line_symbols = style.line_symbols(&way.tags);
        if area_symbols.is_empty() && line_symbols.is_empty() {
            continue;
        }
        let Some(coords) = data.way_coords(way) else {
            continue;
        };
        if !area_symbols.is_empty() {
            let rings = clip_rings(vec![coords.clone()], &bbox);
            if !rings.is_empty() {
                for &symbol in &area_symbols {
                    areas.push(area_object(symbol, &rings));
                }
            }
        }
        if !line_symbols.is_empty() {
            let pieces = if coords.iter().all(|&p| bbox.contains(p)) {
                vec![coords]
            } else {
                clip_polyline(&coords, &bbox)
            };
            for piece in &pieces {
                for &symbol in &line_symbols {
                    lines.push(line_object(symbol, piece));
                }
            }
        }
    }

    for node in &data.tagged_nodes {
        if !bbox.contains(node.position) {
            continue;
        }
        for symbol in style.point_symbols(&node.tags) {
            let (x, y) = paper(node.position);
            let mut object = Object::new(ObjectKind::Point);
            object.symbol_id = symbol;
            object.coords = vec![Coord::new(x, y, 0)];
            points.push(object);
        }
    }

    let counts = (areas.len(), lines.len(), points.len());
    let mut objects = areas;
    objects.append(&mut lines);
    objects.append(&mut points);
    Ok(Conversion {
        projection,
        bbox,
        objects,
        counts,
    })
}

/// A line's name as the legend gives it, after the line's number: without
/// the number again where the name starts with it, as OpenStreetMap's do —
/// `13 : Pavese -> Malpighi` is `Pavese -> Malpighi`, `S1 (Pianoro)` is
/// `Pianoro`.
fn legend_name(name: &str, reference: &str) -> String {
    let Some(rest) = name.strip_prefix(reference) else {
        return name.to_string();
    };
    if rest.starts_with(|c: char| c.is_alphanumeric()) {
        // "13A : …" names a variant of 13, not 13 itself.
        return name.to_string();
    }
    let rest = rest.trim_start_matches([' ', ':', '-']).trim();
    match rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        Some(inner) => inner.to_string(),
        None => rest.to_string(),
    }
}

/// The area a closed ring encloses, whichever way round it runs.
fn ring_area(ring: &[(f64, f64)]) -> f64 {
    ring.windows(2)
        .map(|w| w[0].0 * w[1].1 - w[1].0 * w[0].1)
        .sum::<f64>()
        .abs()
        / 2.0
}

/// Whether `p` is inside the closed `ring`.
fn inside_ring((x, y): (f64, f64), ring: &[(f64, f64)]) -> bool {
    let mut inside = false;
    for w in ring.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if (y0 > y) != (y1 > y) && x < x0 + (y - y0) / (y1 - y0) * (x1 - x0) {
            inside = !inside;
        }
    }
    inside
}

/// How far `p` is from the nearest side of the closed `ring`.
fn distance_to_ring(p: (f64, f64), ring: &[(f64, f64)]) -> f64 {
    ring.windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let length = dx * dx + dy * dy;
            let t = if length > 0.0 {
                (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0.0, 1.0)
            } else {
                0.0
            };
            ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
        })
        .fold(f64::INFINITY, f64::min)
}

/// A closed ring on the paper, in mm.
type Ring = Vec<(f64, f64)>;

/// How large a map's name is on the paper, in mm, halo and all: about
/// 0.6 of the font's size a character, bold, and as high as the font.
fn map_name_size(text: &str) -> (f64, f64) {
    let size = MAP_NAME_PX * PIXEL_MM;
    (text.chars().count() as f64 * 0.6 * size + 1.0, size)
}

/// How far, in mm, a map's name is kept from any map's outline inside an
/// area, at the least; and how far outside an area its name goes.
const NAME_CLEARANCE: f64 = 0.5;
const NAME_GAP: f64 = 1.0;

/// Where a map's name goes: its middle, its size in mm, and its rotation
/// in radians counter-clockwise, as Mapper's.
#[derive(Clone, Debug)]
struct NameBox {
    center: (f64, f64),
    size: (f64, f64),
    rotation: f64,
}

impl NameBox {
    /// The box's edge, a point every millimetre or closer, and its middle.
    fn points(&self) -> Vec<(f64, f64)> {
        // The paper's y points down; a rotation turns counter-clockwise.
        let (sin, cos) = self.rotation.sin_cos();
        let along = (cos, -sin);
        let across = (sin, cos);
        let (w, h) = (self.size.0 / 2.0, self.size.1 / 2.0);
        let at = |u: f64, v: f64| {
            (
                self.center.0 + u * along.0 + v * across.0,
                self.center.1 + u * along.1 + v * across.1,
            )
        };
        let mut points = vec![self.center];
        let steps = |length: f64| (length.ceil() as usize).max(1);
        let (n, m) = (steps(2.0 * w), steps(2.0 * h));
        for k in 0..=n {
            let u = -w + 2.0 * w * k as f64 / n as f64;
            points.push(at(u, -h));
            points.push(at(u, h));
        }
        for k in 1..m {
            let v = -h + 2.0 * h * k as f64 / m as f64;
            points.push(at(-w, v));
            points.push(at(w, v));
        }
        points
    }

    /// The box as a closed ring.
    fn ring(&self) -> Vec<(f64, f64)> {
        let (sin, cos) = self.rotation.sin_cos();
        let (along, across) = ((cos, -sin), (sin, cos));
        let (w, h) = (self.size.0 / 2.0, self.size.1 / 2.0);
        let at = |u: f64, v: f64| {
            (
                self.center.0 + u * along.0 + v * across.0,
                self.center.1 + u * along.1 + v * across.1,
            )
        };
        vec![at(-w, -h), at(w, -h), at(w, h), at(-w, h), at(-w, -h)]
    }

    /// Whether this box and `other` overlap.
    fn overlaps(&self, other: &NameBox) -> bool {
        let (mine, theirs) = (self.ring(), other.ring());
        self.points().iter().any(|&p| inside_ring(p, &theirs))
            || other.points().iter().any(|&p| inside_ring(p, &mine))
    }
}

/// What a map's name keeps clear of: every map's outline, the other maps'
/// areas, the names already placed, and the edge of the map.
struct Around<'a> {
    outlines: &'a [&'a [(f64, f64)]],
    others: &'a [&'a [(f64, f64)]],
    placed: &'a [NameBox],
    frame: &'a [(f64, f64)],
}

impl Around<'_> {
    /// How far each of `points` is from the nearest outline.
    fn distances(&self, points: &[(f64, f64)]) -> Vec<f64> {
        points
            .iter()
            .map(|&p| {
                self.outlines
                    .iter()
                    .map(|r| distance_to_ring(p, r))
                    .fold(f64::INFINITY, f64::min)
            })
            .collect()
    }

    /// How close the nearest outline is to any of `points`.
    fn clearance(&self, points: &[(f64, f64)]) -> f64 {
        self.distances(points)
            .into_iter()
            .fold(f64::INFINITY, f64::min)
    }
}

/// The direction of a ring's longest extent, as a rotation that writes a
/// name along it the right way up.
fn long_axis(ring: &[(f64, f64)]) -> f64 {
    // The first vertex once, not again at the end.
    let ring = &ring[..ring.len() - 1];
    let n = ring.len() as f64;
    let (mx, my) = ring
        .iter()
        .fold((0.0, 0.0), |(x, y), p| (x + p.0 / n, y + p.1 / n));
    let (mut xx, mut xy, mut yy) = (0.0, 0.0, 0.0);
    for &(x, y) in ring {
        let (dx, dy) = (x - mx, y - my);
        xx += dx * dx;
        xy += dx * dy;
        yy += dy * dy;
    }
    let angle = 0.5 * (2.0 * xy).atan2(xx - yy);
    // That angle is the paper's, y down: a rotation the other way round,
    // kept within a quarter turn of level.
    let mut rotation = -angle;
    if rotation > std::f64::consts::FRAC_PI_2 {
        rotation -= std::f64::consts::PI;
    } else if rotation <= -std::f64::consts::FRAC_PI_2 {
        rotation += std::f64::consts::PI;
    }
    rotation
}

/// A name of `size` inside one of `rings`, wholly, clear of every outline
/// by [`NAME_CLEARANCE`] at the least and as far from them as it can be —
/// and between places as clear as each other, the one farthest from them
/// on average, or nearest the middle of the area — over no name already placed: level where it fits so, or else turned
/// along the area. `None` where it fits in none.
fn place_inside(rings: &[Vec<(f64, f64)>], size: (f64, f64), around: &Around) -> Option<NameBox> {
    // How clear of the outlines a name at `center` is, where it fits, and
    // how good a place that is: the clearer the better, then the farther
    // on average, then the nearer the middle of the area's box.
    let fit = |ring: &[(f64, f64)], middle: (f64, f64), center: (f64, f64), rotation: f64| {
        let name = NameBox {
            center,
            size,
            rotation,
        };
        let points = name.points();
        if !points.iter().all(|&p| inside_ring(p, ring))
            || around.placed.iter().any(|other| other.overlaps(&name))
        {
            return None;
        }
        let distances = around.distances(&points);
        let clear = distances.iter().copied().fold(f64::INFINITY, f64::min);
        let mean = distances.iter().sum::<f64>() / distances.len() as f64;
        let off = (center.0 - middle.0).hypot(center.1 - middle.1);
        Some((clear, clear + 0.01 * mean - 0.0001 * off))
    };
    let mut best: [Option<(f64, NameBox)>; 2] = [None, None];
    for ring in rings {
        let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
        let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(x, y) in ring.iter() {
            (min_x, max_x) = (min_x.min(x), max_x.max(x));
            (min_y, max_y) = (min_y.min(y), max_y.max(y));
        }
        // Too small for the name whichever way it is turned.
        if (max_x - min_x).hypot(max_y - min_y) < size.0 {
            continue;
        }
        let middle = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
        let axis = long_axis(ring);
        for (k, rotation) in [0.0, axis].into_iter().enumerate() {
            if k == 1 && axis.abs() < 10f64.to_radians() {
                continue;
            }
            // A grid over the ring, then finer and finer round the best
            // point of it.
            const STEPS: usize = 40;
            let mut step = (
                (max_x - min_x) / STEPS as f64,
                (max_y - min_y) / STEPS as f64,
            );
            let mut found: Option<((f64, f64), (f64, f64))> = None;
            for i in 0..=STEPS {
                for j in 0..=STEPS {
                    let center = (min_x + step.0 * i as f64, min_y + step.1 * j as f64);
                    if let Some(fits) = fit(ring, middle, center, rotation) {
                        if found.is_none_or(|(_, (_, score))| fits.1 > score) {
                            found = Some((center, fits));
                        }
                    }
                }
            }
            let Some((mut center, (mut clear, mut score))) = found else {
                continue;
            };
            for _ in 0..4 {
                step = (step.0 / 2.0, step.1 / 2.0);
                for i in -2..=2 {
                    for j in -2..=2 {
                        let at = (
                            center.0 + step.0 * f64::from(i),
                            center.1 + step.1 * f64::from(j),
                        );
                        if let Some((c, sc)) = fit(ring, middle, at, rotation) {
                            if sc > score {
                                (center, clear, score) = (at, c, sc);
                            }
                        }
                    }
                }
            }
            if clear >= NAME_CLEARANCE && best[k].as_ref().is_none_or(|(c, _)| clear > *c) {
                let name = NameBox {
                    center,
                    size,
                    rotation,
                };
                best[k] = Some((clear, name));
            }
        }
    }
    // Level, unless it only just fits so and turned it fits far better.
    match best {
        [Some((level, name)), Some((turned, _))]
            if level >= 2.0 * NAME_CLEARANCE || level >= turned =>
        {
            Some(name)
        }
        [_, Some((_, name))] | [Some((_, name)), None] => Some(name),
        [None, None] => None,
    }
}

/// A level name of `size` beside `rings`, just outside them: the first
/// side, of above, below, right, left and then the corners and the
/// directions between, where it is clear of the other maps' areas and
/// outlines, of the names already placed and of the map's edge, or else
/// the one with the least in the way.
fn place_beside(rings: &[Vec<(f64, f64)>], size: (f64, f64), around: &Around) -> NameBox {
    let largest = rings
        .iter()
        .max_by(|a, b| ring_area(a).total_cmp(&ring_area(b)))
        .expect("an area on the map");
    let n = largest.len() as f64 - 1.0;
    let origin = largest[..largest.len() - 1]
        .iter()
        .fold((0.0, 0.0), |(x, y), p| (x + p.0 / n, y + p.1 / n));
    let reach = largest
        .iter()
        .map(|p| (p.0 - origin.0).hypot(p.1 - origin.1))
        .fold(0.0, f64::max)
        + size.0
        + NAME_GAP;
    // Up first: the paper's y points down.
    let directions: Vec<f64> = [90.0, 270.0, 0.0, 180.0, 45.0, 135.0, 315.0, 225.0]
        .into_iter()
        .chain((0..8).map(|k| 22.5 + 45.0 * f64::from(k)))
        .collect();
    let mut best: Option<(usize, NameBox)> = None;
    for degrees in directions {
        let (sin, cos) = f64::to_radians(degrees).sin_cos();
        let direction = (cos, -sin);
        // Out from the middle until clear of the map's own areas.
        let mut distance = 0.0;
        let name = loop {
            let name = NameBox {
                center: (
                    origin.0 + direction.0 * distance,
                    origin.1 + direction.1 * distance,
                ),
                size,
                rotation: 0.0,
            };
            let points = name.points();
            let clear = points.iter().all(|&p| {
                rings
                    .iter()
                    .all(|r| !inside_ring(p, r) && distance_to_ring(p, r) >= NAME_GAP)
            });
            if clear || distance > reach {
                break name;
            }
            distance += 0.25;
        };
        let points = name.points();
        let in_way = points
            .iter()
            .filter(|&&p| {
                around.others.iter().any(|r| inside_ring(p, r))
                    || around.clearance(&[p]) < NAME_CLEARANCE
                    || !inside_ring(p, around.frame)
            })
            .count()
            + 1000 * around.placed.iter().filter(|o| o.overlaps(&name)).count();
        if in_way == 0 {
            return name;
        }
        if best.as_ref().is_none_or(|(w, _)| in_way < *w) {
            best = Some((in_way, name));
        }
    }
    best.expect("some direction").1
}

fn path_object(symbol: i32, coords: Vec<Coord>) -> Object {
    let mut object = Object::new(ObjectKind::Path(PathObject::default()));
    object.symbol_id = symbol;
    object.coords = coords;
    object
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rectangle as a closed ring, on the paper.
    fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]
    }

    #[test]
    fn a_map_name_goes_inside_an_area_it_fits_in_level() {
        let area = rectangle(0.0, 0.0, 100.0, 60.0);
        let frame = rectangle(-500.0, -500.0, 500.0, 500.0);
        let outlines = [area.as_slice()];
        let around = Around {
            outlines: &outlines,
            others: &[],
            placed: &[],
            frame: &frame,
        };
        let name = place_inside(std::slice::from_ref(&area), (30.0, 5.0), &around).unwrap();
        assert_eq!(name.rotation, 0.0);
        // As far from the outline as it can be: in the middle.
        assert!((name.center.0 - 50.0).abs() < 1.0, "{name:?}");
        assert!((name.center.1 - 30.0).abs() < 1.0, "{name:?}");
    }

    #[test]
    fn a_map_name_turns_along_a_long_thin_area() {
        // A strip 100 mm long and 8 wide, at 45°.
        let (c, s) = (
            std::f64::consts::FRAC_1_SQRT_2,
            std::f64::consts::FRAC_1_SQRT_2,
        );
        let at = |u: f64, v: f64| (u * c - v * s, u * s + v * c);
        let strip = vec![
            at(0.0, 0.0),
            at(100.0, 0.0),
            at(100.0, 8.0),
            at(0.0, 8.0),
            at(0.0, 0.0),
        ];
        let frame = rectangle(-500.0, -500.0, 500.0, 500.0);
        let outlines = [strip.as_slice()];
        let around = Around {
            outlines: &outlines,
            others: &[],
            placed: &[],
            frame: &frame,
        };
        let name = place_inside(std::slice::from_ref(&strip), (30.0, 5.0), &around).unwrap();
        // Down to the right on the paper, its y pointing down: turned
        // clockwise, a negative rotation.
        assert!(
            (name.rotation + std::f64::consts::FRAC_PI_4).abs() < 0.01,
            "{name:?}"
        );
    }

    #[test]
    fn a_map_name_too_long_for_its_area_goes_beside_it_clear_of_the_others() {
        let small = rectangle(0.0, 0.0, 10.0, 10.0);
        // Another map's area right above the small one.
        let other = rectangle(-50.0, -40.0, 60.0, -2.0);
        let frame = rectangle(-500.0, -500.0, 500.0, 500.0);
        let outlines = [small.as_slice(), other.as_slice()];
        let others = [other.as_slice()];
        let around = Around {
            outlines: &outlines,
            others: &others,
            placed: &[],
            frame: &frame,
        };
        assert!(place_inside(std::slice::from_ref(&small), (30.0, 5.0), &around).is_none());
        let name = place_beside(std::slice::from_ref(&small), (30.0, 5.0), &around);
        // Not above, where the other area is: below, just clear of it.
        assert!(name.center.1 > 10.0 && name.center.1 < 16.0, "{name:?}");
        assert!(name.points().iter().all(|&p| !inside_ring(p, &small)));
    }

    #[test]
    fn utm_matches_a_reference_point_mapper_wrote() {
        // maps/city_sample.omap's own georeferencing: the same point in
        // degrees and in UTM 32N, as Mapper (that is, PROJ) computed it.
        let (e, n) = utm(32, (11.64973926, 48.25195669));
        assert!((e - 696_686.398978).abs() < 0.01, "easting {e}");
        assert!((n - 5_347_699.134904).abs() < 0.01, "northing {n}");
    }

    #[test]
    fn the_zone_follows_the_longitude() {
        assert_eq!(utm_zone(11.34), 32);
        assert_eq!(utm_zone(12.01), 33);
        assert_eq!(utm_zone(-0.1), 30);
    }

    #[test]
    fn the_projection_puts_true_north_up() {
        // East of the zone's central meridian true north lies a degree and a
        // half west of grid north; the map turns that away.
        let projection = Projection::new((11.343, 44.4938), 6085);
        let (x0, y0) = projection.to_paper((11.343, 44.4938));
        assert!(x0.abs() < 1e-6 && y0.abs() < 1e-6);
        let (x, y) = projection.to_paper((11.343, 44.5038));
        assert!(x.abs() < 1e-3, "north drifts sideways by {x} mm");
        assert!(y < 0.0, "north is up the paper");
        // 0.01° of latitude is about 1112 m, at 1:6085 about 182.7 mm.
        assert!((-y - 182.7).abs() < 0.3, "{y}");
        assert!((projection.georeference.grivation + 1.64).abs() < 0.05);
        assert!((projection.georeference.grid_scale_factor - 1.0).abs() < 1e-3);
    }

    #[test]
    fn a_headway_follows_the_line_and_reads_upright() {
        // A line 300 mm long: headways at 40, 160 and 280 mm.
        let east = along(&[(0.0, 0.0), (300.0, 0.0)], &[]);
        let at: Vec<f64> = east.iter().map(|&(x, _, _)| x).collect();
        assert_eq!(at, vec![40.0, 160.0, 280.0]);
        assert!(east.iter().all(|&(_, _, r)| r == 0.0));
        // Running west, the text is still not upside down.
        let west = along(&[(300.0, 0.0), (0.0, 0.0)], &[]);
        assert!(west.iter().all(|&(_, _, r)| r == 0.0));
        // Going up the paper (y decreasing) is a quarter turn
        // counter-clockwise.
        let north = along(&[(0.0, 100.0), (0.0, 0.0)], &[]);
        let rotation = north[0].2;
        assert!((rotation.abs() - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        // Too short for the first spacing: one, in the middle.
        assert_eq!(along(&[(0.0, 0.0), (50.0, 0.0)], &[]).len(), 1);
        // A stop where the first would go moves it along, off the stop.
        let moved = along(&[(0.0, 0.0), (300.0, 0.0)], &[(40.0, 0.0, 7.5)]);
        assert_eq!(moved[0].0, 48.0);
        // Nowhere clear within reach: left out.
        let blocked: Vec<(f64, f64, f64)> =
            (0..=30).map(|i| (f64::from(i) * 10.0, 0.0, 9.0)).collect();
        assert!(along(&[(0.0, 0.0), (300.0, 0.0)], &blocked).is_empty());
        // Shorter than the text: none.
        assert!(along(&[(0.0, 0.0), (10.0, 0.0)], &[]).is_empty());
    }

    #[test]
    fn a_point_the_clip_put_on_the_edge_is_on_the_edge() {
        let bbox = BBox::parse("0,0,10,10").unwrap();
        let pieces = clip_polyline(&[(-5.0, 5.0), (5.0, 5.0), (5.0, 15.0)], &bbox);
        let piece = &pieces[0];
        assert!(on_edge(piece[0], &bbox));
        assert!(on_edge(piece[piece.len() - 1], &bbox));
        assert!(!on_edge((5.0, 5.0), &bbox));
    }

    #[test]
    fn a_legend_name_does_not_repeat_the_number() {
        assert_eq!(
            legend_name("13 : Pavese -> Malpighi", "13"),
            "Pavese -> Malpighi"
        );
        assert_eq!(legend_name("S1 (Pianoro)", "S1"), "Pianoro");
        assert_eq!(
            legend_name("13A : Cavour -> Carteria", "13"),
            "13A : Cavour -> Carteria"
        );
        assert_eq!(legend_name("Marconi Express", "MEX"), "Marconi Express");
    }

    #[test]
    fn a_polyline_is_cut_where_it_leaves_and_reenters() {
        let bbox = BBox::parse("0,0,10,10").unwrap();
        let pieces = clip_polyline(
            &[
                (-5.0, 5.0),
                (5.0, 5.0),
                (15.0, 5.0),
                (15.0, 8.0),
                (5.0, 8.0),
            ],
            &bbox,
        );
        assert_eq!(
            pieces,
            vec![
                vec![(0.0, 5.0), (5.0, 5.0), (10.0, 5.0)],
                vec![(10.0, 8.0), (5.0, 8.0)]
            ]
        );
        assert!(clip_polyline(&[(20.0, 20.0), (30.0, 30.0)], &bbox).is_empty());
    }

    #[test]
    fn an_area_keeps_its_hole_through_the_clip() {
        let bbox = BBox::parse("0,0,10,10").unwrap();
        let outer = vec![
            (-5.0, 2.0),
            (8.0, 2.0),
            (8.0, 8.0),
            (-5.0, 8.0),
            (-5.0, 2.0),
        ];
        let hole = vec![(3.0, 4.0), (6.0, 4.0), (6.0, 6.0), (3.0, 6.0), (3.0, 4.0)];
        let rings = clip_rings(vec![outer, hole], &bbox);
        assert_eq!(rings.len(), 2);
        assert!(rings.iter().flatten().all(|&p| bbox.contains(p)));
        // Wholly inside: left alone.
        let inside = vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 1.0)];
        assert_eq!(clip_rings(vec![inside.clone()], &bbox), vec![inside]);
    }
}
