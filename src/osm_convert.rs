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
use crate::osm::{BBox, OsmData};
use crate::osm_style::{line_width_px, stop_circle_px, Sample, Style, ARROW_PX, PIXEL_MM};
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
/// a route the edge cuts is written; and the step it moves further in by,
/// up to eight times, to stay clear of the others.
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

/// How far apart, in mm on the paper, two line numbers have to be not to
/// overlap: about a badge's height, and the width of a short one.
const LABEL_HEIGHT: f64 = 4.4;
const LABEL_WIDTH: f64 = 8.0;

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
        for (l, line) in lines.iter().enumerate() {
            for piece in line.route.iter().chain(&line.detours) {
                for clipped in clip_polyline(piece, &bbox) {
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

        // The numbers at the termini: the ends of a line's drawn route the
        // edge of the map does not cut.
        let mut placed: Vec<(f64, f64)> = Vec::new();
        for (l, line) in lines.iter().enumerate() {
            let own: Vec<usize> = (0..routes.len()).filter(|&r| routes[r].line == l).collect();
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
            for (x, mut y) in ends {
                // Lines ending at the same terminus — every train at the
                // central station — stack their numbers downwards rather
                // than print them over one another.
                while placed.iter().any(|&(px, py): &(f64, f64)| {
                    (px - x).abs() < LABEL_WIDTH && (py - y).abs() < LABEL_HEIGHT
                }) {
                    y += LABEL_HEIGHT;
                }
                placed.push((x, y));
                self.push_label(label, &line.reference, x, y);
            }
        }

        // A route cut by the edge of the map gets its number just inside the
        // edge, on the line itself: as close to the edge as is clear of the
        // numbers already written, moving inwards along it.
        for (r, route) in routes.iter().enumerate() {
            let paper = &laid[r].points;
            let label = style.id(&format!("{} label", key_of(route.line)));
            for (at_start, is_cut) in [(true, cut[r].0), (false, cut[r].1)] {
                if !is_cut {
                    continue;
                }
                let length: f64 = paper
                    .windows(2)
                    .map(|p| (p[1].0 - p[0].0).hypot(p[1].1 - p[0].1))
                    .sum();
                let spot = (1..=8)
                    .map(|i| f64::from(i) * EXIT_STEP)
                    .take_while(|&d| d < length)
                    .map(|d| point_along(paper, if at_start { d } else { length - d }))
                    .find(|&(x, y)| {
                        placed.iter().all(|&(px, py)| {
                            (px - x).abs() >= LABEL_WIDTH || (py - y).abs() >= LABEL_HEIGHT
                        })
                    });
                if let Some((x, y)) = spot {
                    placed.push((x, y));
                    self.push_label(label, &lines[route.line].reference, x, y);
                }
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

fn path_object(symbol: i32, coords: Vec<Coord>) -> Object {
    let mut object = Object::new(ObjectKind::Path(PathObject::default()));
    object.symbol_id = symbol;
    object.coords = coords;
    object
}

#[cfg(test)]
mod tests {
    use super::*;

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
