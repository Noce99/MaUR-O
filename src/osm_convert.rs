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
//! The scale is the one at which a zoom-16 tile pixel of openstreetmap.org
//! is [`osm_style::PIXEL_MM`](crate::osm_style::PIXEL_MM) on the paper, at
//! the map's own latitude ([`zoom_scale`]): about 1:6100 in Bologna.
//!
//! # Clipping
//!
//! Everything is cut to the bounding box, in degrees, before it is
//! projected. A line is cut wherever it leaves the box ([`clip_polyline`]);
//! an area which crosses the edge has each of its rings cut to it
//! ([`clip_rings`]), and one wholly inside is kept as it is. A large extract converts whole
//! with no bounding box given: [`crate::osm::OsmData::extent`] then decides
//! what "whole" means.

use std::path::Path;

use crate::bus::{BusLine, HEADWAY_HOURS};
use crate::map::{coord_flag, h_align, v_align, Coord, Object, ObjectKind, PathObject, TextObject};
use crate::osm::{BBox, OsmData};
use crate::osm_style::{Sample, Style, PIXEL_MM, ZOOM};
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

/// The scale denominator at which one openstreetmap.org tile pixel at
/// `zoom` is [`PIXEL_MM`] on the paper, at latitude `lat`.
///
/// A Web Mercator pixel covers `2πR cos(lat) / (256 · 2^zoom)` meters of
/// ground, `R` being the sphere the projection is defined on.
pub fn zoom_scale(lat: f64, zoom: u32) -> f64 {
    let meters_per_pixel = 2.0 * std::f64::consts::PI * WGS84_A * lat.to_radians().cos()
        / (256.0 * f64::from(1u32 << zoom));
    meters_per_pixel / (PIXEL_MM / 1000.0)
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

/// A stretch of a line's route on the paper, waiting for its headway to be
/// written along it once everything else of every line is placed.
struct PendingHeadway {
    route: Vec<(f64, f64)>,
    text: String,
    symbol: i32,
    stops: Vec<(f64, f64)>,
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
/// in mm: the length of `ogni 12 min`, which two such texts running along
/// the same street need between their centres, and a little more.
const HEADWAY_CLEARANCE: f64 = 15.0;

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
    /// rendering of the map matches openstreetmap.org's own tiles pixel for
    /// pixel.
    pub fn tile_resolution(&self) -> f64 {
        1000.0 / (f64::from(self.scale()) * PIXEL_MM)
    }

    /// Adds bus and train lines over the map: each line's route, its stops
    /// as circles, and its number at either end of the route (once, for a
    /// circular line), with the symbols `style` keys by the line's kind and
    /// number (see
    /// [`carto_style_with_bus_lines`](crate::osm_style::carto_style_with_bus_lines)).
    /// Everything is clipped to the map's area, like the rest of it; a
    /// number goes only at an end which lies inside.
    pub fn add_bus_lines(&mut self, style: &Style, lines: &[BusLine]) {
        let bbox = self.bbox;
        let mut placed: Vec<(f64, f64)> = Vec::new();
        // The stretches of route a headway is to be written along, with the
        // text, its symbol and the line's own stops, placed once every
        // route, stop and number is.
        let mut headways: Vec<PendingHeadway> = Vec::new();
        for line in lines {
            let key = format!("{} {}", line.mode.key(), line.reference);
            let (route, stop, label) = (
                style.id(&key),
                style.id(&format!("{key} stop")),
                style.id(&format!("{key} label")),
            );
            let headway_text = line.headway.map(headway_text);
            let headway_symbol = style.id(&format!("{key} headway"));
            for piece in &line.route {
                for clipped in clip_polyline(piece, &bbox) {
                    let paper: Vec<(f64, f64)> = clipped
                        .iter()
                        .map(|&p| self.projection.to_paper(p))
                        .collect();
                    if let Some(text) = &headway_text {
                        let stops: Vec<(f64, f64)> = line
                            .stops
                            .iter()
                            .map(|&p| self.projection.to_paper(p))
                            .collect();
                        headways.push(PendingHeadway {
                            route: paper.clone(),
                            text: text.clone(),
                            symbol: headway_symbol,
                            stops,
                        });
                    }
                    let coords = paper.iter().map(|&(x, y)| Coord::new(x, y, 0)).collect();
                    self.objects.push(path_object(route, coords));
                    self.counts.1 += 1;
                }
            }
            for &position in line.stops.iter().filter(|&&p| bbox.contains(p)) {
                let (x, y) = self.projection.to_paper(position);
                let mut object = Object::new(ObjectKind::Point);
                object.symbol_id = stop;
                object.coords = vec![Coord::new(x, y, 0)];
                self.objects.push(object);
                self.counts.2 += 1;
            }
            let Some((start, end)) = line.ends() else {
                continue;
            };
            let (sx, sy) = self.projection.to_paper(start);
            let (ex, ey) = self.projection.to_paper(end);
            // Closer than a label is wide, the two ends are one terminus.
            let ends = if (ex - sx).hypot(ey - sy) < 5.0 {
                vec![start]
            } else {
                vec![start, end]
            };
            for position in ends.into_iter().filter(|&p| bbox.contains(p)) {
                let (x, mut y) = self.projection.to_paper(position);
                // Lines ending at the same terminus — every train at the
                // central station — stack their numbers downwards rather
                // than print them over one another.
                while placed.iter().any(|&(px, py): &(f64, f64)| {
                    (px - x).abs() < LABEL_WIDTH && (py - y).abs() < LABEL_HEIGHT
                }) {
                    y += LABEL_HEIGHT;
                }
                placed.push((x, y));
                let mut object = Object::new(ObjectKind::Text(TextObject {
                    text: line.reference.clone(),
                    h_align: h_align::HCENTER,
                    v_align: v_align::VCENTER,
                    box_size: None,
                }));
                object.symbol_id = label;
                object.coords = vec![Coord::new(x, y, 0)];
                self.objects.push(object);
                self.counts.2 += 1;
            }
        }

        // The headways last, each kept off its line's stops, off every
        // number at a terminus, and off every headway already written —
        // two lines sharing a street would otherwise print theirs in the
        // same place.
        let mut written: Vec<(f64, f64, f64)> =
            placed.iter().map(|&(x, y)| (x, y, LABEL_WIDTH)).collect();
        for PendingHeadway {
            route: paper,
            text,
            symbol,
            stops,
        } in headways
        {
            let mut blocked = written.clone();
            blocked.extend(stops.iter().map(|&(x, y)| (x, y, HEADWAY_HALF + 1.5)));
            for (x, y, rotation) in along(&paper, &blocked) {
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

    /// Adds a legend under the map: a sample of every symbol the map uses,
    /// each with what it means, in columns as wide as the map allows.
    ///
    /// Under the map rather than over it: in Mapper a colour is drawn at the
    /// same level everywhere, so a panel laid over the map to hold the legend
    /// would hide the legend's own samples along with the map. The bus and
    /// train lines come first, each with its name from `lines`, then an
    /// entry for their stops and one for the numbers at their ends; then
    /// everything else, in the order the style defines it.
    pub fn add_legend(&mut self, style: &Style, lines: &[BusLine]) {
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
            if !used.contains(&id) {
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
        if let Some(stop) = first_stop {
            entries.push((
                stop,
                Sample::Point,
                "Fermata, stazione".to_string(),
                String::new(),
            ));
        }
        if let Some((label, reference)) = first_label {
            entries.push((
                label,
                Sample::Text,
                "Capolinea, con il numero della linea".to_string(),
                reference,
            ));
        }
        if let Some((headway, text)) = first_headway {
            entries.push((
                headway,
                Sample::Text,
                format!(
                    "Frequenza media {}–{} feriali",
                    HEADWAY_HOURS.0, HEADWAY_HOURS.1
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
    let scale = zoom_scale(center_lat, ZOOM).round() as i32;
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
    fn zoom_16_in_bologna_is_about_1_to_6100() {
        let scale = zoom_scale(44.4938, 16);
        assert!((scale - 6085.0).abs() < 2.0, "{scale}");
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
