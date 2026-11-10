//! Turns Karttapullautin's vector output into an orienteering map: the
//! contours, index contours, form lines and depression slope lines a mapper
//! would have drawn from the same lidar, drawn with ISOM symbols
//! (`101`, `101.1`, `102`, `103`, `103.1`) that [`crate::contour_symbols`]
//! classifies, so the map is something `contours_to_raster` can run on.
//!
//! Karttapullautin writes its contours to `temp/out2.dxf` and its form lines
//! to `temp/formlines.dxf` (see [`crate::dxf`] for the reader). What each
//! layer means was measured against the national DTM the lidar is also the
//! source of (`mds/DEM-Benchmark-Plan.md`, *Phase 0 findings*):
//!
//! | DXF layer | Level | Becomes |
//! |---|---|---|
//! | `contour`, `depression` | multiples of the interval (5 m) | `101` Contour |
//! | `contour_index`, `depression_index` | multiples of 5 intervals | `102` Index contour |
//! | `formline` (`formlines.dxf`) | half levels; no height of its own | `103` Form line |
//! | `depression_intermed` | half levels | a `103` ring, only where a `formline` runs along it |
//! | `slope_line` | | a `101.1`/`103.1` point on the ring it ticks |
//! | `contour_intermed`, `small_depression`, anything else | | skipped |
//!
//! Every line keeps the height Karttapullautin gave it -- its DXF elevation,
//! or for a form line, which has none, the elevation of the half-level line
//! it is a piece of -- in [`ImportedMap::levels`]. That is what a predicted
//! elevation model is scored against per contour.
//!
//! Coordinates go onto the paper unrotated (grid north is map north) with
//! the center of the imported box at the paper origin, and the georeferencing
//! says so, so a raster `contours_to_raster` writes from the map lands on the
//! same ground as the lidar.

use std::collections::HashMap;
use std::path::Path;

use crate::dxf::{Entity, EntityKind};
use crate::map::{
    coord_flag, Color, Coord, Element, LineSymbol, Object, ObjectKind, PointSymbol, Symbol,
};
use crate::osm::BBox;
use crate::osm_convert::clip_polyline;
use crate::xml_writer::{
    color_element, georeferencing_element, symbol_element, Georeference, MapFile,
};

/// The symbol id of `101` Contour.
pub const CONTOUR: i32 = 0;
/// The symbol id of `101.1` Slope line, contour.
pub const SLOPE_LINE_CONTOUR: i32 = 1;
/// The symbol id of `102` Index contour.
pub const INDEX_CONTOUR: i32 = 2;
/// The symbol id of `103` Form line.
pub const FORM_LINE: i32 = 3;
/// The symbol id of `103.1` Slope line, form line.
pub const SLOPE_LINE_FORM_LINE: i32 = 4;

/// How far (meters) a slope line's end may be from the ring it ticks.
const SLOPE_LINE_SNAP: f64 = 1.0;
/// How far (meters) a form line vertex may be from the half-level line it
/// is a piece of.
const FORM_LINE_SNAP: f64 = 0.5;
/// How many form line vertices must lie on a `depression_intermed` ring for
/// it to count as drawn.
const FORM_LINE_RING_MIN_VERTICES: usize = 3;

/// A rectangle on the ground, in the projected CRS's meters, `y` north.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundBox {
    /// The western edge.
    pub min_x: f64,
    /// The southern edge.
    pub min_y: f64,
    /// The eastern edge.
    pub max_x: f64,
    /// The northern edge.
    pub max_y: f64,
}

impl GroundBox {
    /// The box's center.
    pub fn center(&self) -> (f64, f64) {
        (
            (self.min_x + self.max_x) / 2.0,
            (self.min_y + self.max_y) / 2.0,
        )
    }

    /// Whether `(x, y)` is inside the box, edges included.
    pub fn contains(&self, (x, y): (f64, f64)) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }

    /// The same box as [`clip_polyline`] takes it. Its fields are named for
    /// degrees, but the clip is plain planar arithmetic.
    fn as_bbox(&self) -> BBox {
        BBox {
            min_lon: self.min_x,
            min_lat: self.min_y,
            max_lon: self.max_x,
            max_lat: self.max_y,
        }
    }
}

/// What to import, and where the map sits.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// The ground to import; everything is clipped to it.
    pub bounds: GroundBox,
    /// The map scale denominator, e.g. `10000`.
    pub scale: i32,
    /// The EPSG code of the CRS the DXF coordinates are in (3006 for
    /// Lantmäteriet's SWEREF99 TM).
    pub epsg: i32,
    /// The box's center as `(lat, lon)` in degrees, which Mapper keeps
    /// alongside the projected reference point.
    pub center_lat_lon: (f64, f64),
    /// Whether to place slope lines on depressions (a real map has them);
    /// `false` leaves every depression for the algorithm to discover.
    pub slope_lines: bool,
}

/// Counts of what [`import`] made, for a report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportStats {
    /// `101` lines.
    pub contours: usize,
    /// `102` lines.
    pub index_contours: usize,
    /// `103` lines (open pieces and rings).
    pub form_lines: usize,
    /// Of [`ImportStats::form_lines`], the closed rings.
    pub form_line_rings: usize,
    /// Slope line points placed.
    pub slope_lines: usize,
    /// Slope lines inside the box but not placed: their ring was clipped,
    /// skipped, or not found.
    pub slope_lines_dropped: usize,
}

/// A map made from Karttapullautin's output, ready to write.
pub struct ImportedMap {
    /// Where it sits on the earth.
    pub georeference: Georeference,
    /// The objects, lines first, then slope line points.
    pub objects: Vec<Object>,
    /// For each object of [`ImportedMap::objects`], the line's height in
    /// meters, or `None` for a point or a line whose height is unknown.
    pub levels: Vec<Option<f64>>,
    /// What was made.
    pub stats: ImportStats,
}

/// A vertex, and the index of the line it belongs to.
type OwnedVertex = ((f64, f64), usize);

/// A grid of vertices, for "which line has a vertex near here".
struct VertexIndex {
    cell: f64,
    cells: HashMap<(i64, i64), Vec<OwnedVertex>>,
}

impl VertexIndex {
    fn new(cell: f64) -> VertexIndex {
        VertexIndex {
            cell,
            cells: HashMap::new(),
        }
    }

    fn key(&self, (x, y): (f64, f64)) -> (i64, i64) {
        (
            (x / self.cell).floor() as i64,
            (y / self.cell).floor() as i64,
        )
    }

    fn insert(&mut self, p: (f64, f64), owner: usize) {
        let key = self.key(p);
        self.cells.entry(key).or_default().push((p, owner));
    }

    /// The owner of the nearest vertex within `radius` (at most one cell).
    fn nearest(&self, p: (f64, f64), radius: f64) -> Option<usize> {
        let (kx, ky) = self.key(p);
        let mut best: Option<(f64, usize)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &(q, owner) in self.cells.get(&(kx + dx, ky + dy)).into_iter().flatten() {
                    let d = (q.0 - p.0).hypot(q.1 - p.1);
                    if d <= radius && best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, owner));
                    }
                }
            }
        }
        best.map(|(_, owner)| owner)
    }
}

/// A depression ring that made it onto the map, for slope lines to find.
struct Ring {
    points: Vec<(f64, f64)>,
    slope_symbol: i32,
}

/// Builds the map: every contour layer of `out2` and every form line of
/// `formlines`, clipped to `options.bounds`, plus (if asked) a slope line
/// on every depression ring that lies wholly inside.
pub fn import(out2: &[Entity], formlines: &[Entity], options: &ImportOptions) -> ImportedMap {
    let (cx, cy) = options.bounds.center();
    let mm_per_meter = 1000.0 / f64::from(options.scale);
    let paper = |(x, y): (f64, f64)| ((x - cx) * mm_per_meter, (cy - y) * mm_per_meter);
    let bbox = options.bounds.as_bbox();

    let polylines = |entities: &'_ [Entity]| -> Vec<Entity> {
        entities
            .iter()
            .filter(|e| e.kind == EntityKind::Polyline && e.points.len() >= 2)
            .cloned()
            .collect()
    };
    let out2_lines = polylines(out2);
    let form_lines = polylines(formlines);

    // The half-level lines form lines are pieces of, for their heights.
    let mut half_levels = VertexIndex::new(2.0);
    for (i, e) in out2_lines.iter().enumerate() {
        if e.layer == "contour_intermed" || e.layer == "depression_intermed" {
            for &p in &e.points {
                half_levels.insert(p, i);
            }
        }
    }
    // And the form lines themselves, for which depression_intermed rings
    // Karttapullautin actually draws.
    let mut form_line_vertices = VertexIndex::new(2.0);
    for (i, e) in form_lines.iter().enumerate() {
        for &p in &e.points {
            form_line_vertices.insert(p, i);
        }
    }

    let mut objects = Vec::new();
    let mut levels = Vec::new();
    let mut stats = ImportStats::default();
    let mut rings: Vec<Ring> = Vec::new();

    let add_line = |symbol: i32,
                    points: &[(f64, f64)],
                    level: Option<f64>,
                    is_ring: bool,
                    objects: &mut Vec<Object>,
                    levels: &mut Vec<Option<f64>>,
                    stats: &mut ImportStats|
     -> bool {
        // A ring wholly inside stays a ring; anything else is cut at the
        // box's edge into open pieces.
        let whole_ring = is_ring && points.iter().all(|&p| options.bounds.contains(p));
        let pieces = if whole_ring {
            vec![points.to_vec()]
        } else {
            clip_polyline(points, &bbox)
        };
        for piece in &pieces {
            let last = piece.len() - 1;
            let coords = piece
                .iter()
                .enumerate()
                .map(|(j, &p)| {
                    let (x, y) = paper(p);
                    let flags = if whole_ring && j == last {
                        coord_flag::CLOSE_POINT
                    } else {
                        0
                    };
                    Coord::new(x, y, flags)
                })
                .collect();
            objects.push(Object {
                symbol_id: symbol,
                coords,
                ..Object::new(ObjectKind::Path(Default::default()))
            });
            levels.push(level);
            match symbol {
                CONTOUR => stats.contours += 1,
                INDEX_CONTOUR => stats.index_contours += 1,
                _ => {
                    stats.form_lines += 1;
                    if whole_ring {
                        stats.form_line_rings += 1;
                    }
                }
            }
        }
        whole_ring
    };

    for e in &out2_lines {
        let points = e.ring_or_path();
        let is_ring = e.is_ring();
        let (symbol, slope_symbol) = match e.layer.as_str() {
            "contour" | "depression" => (CONTOUR, SLOPE_LINE_CONTOUR),
            "contour_index" | "depression_index" => (INDEX_CONTOUR, SLOPE_LINE_CONTOUR),
            "depression_intermed" => {
                let drawn = points
                    .iter()
                    .filter(|&&p| form_line_vertices.nearest(p, FORM_LINE_SNAP).is_some())
                    .count();
                if !is_ring || drawn < FORM_LINE_RING_MIN_VERTICES {
                    continue;
                }
                (FORM_LINE, SLOPE_LINE_FORM_LINE)
            }
            _ => continue,
        };
        let whole_ring = add_line(
            symbol,
            &points,
            e.elevation,
            is_ring,
            &mut objects,
            &mut levels,
            &mut stats,
        );
        if whole_ring && e.layer.starts_with("depression") {
            rings.push(Ring {
                points,
                slope_symbol,
            });
        }
    }

    for e in &form_lines {
        // A form line on a depression ring that is already a form line ring
        // would only draw it twice.
        let on_ring = rings.iter().any(|r| {
            r.slope_symbol == SLOPE_LINE_FORM_LINE
                && e.points
                    .iter()
                    .all(|&p| distance_to_path(p, &r.points) <= FORM_LINE_SNAP)
        });
        if on_ring {
            continue;
        }
        let middle = e.points[e.points.len() / 2];
        let level = e.elevation.or_else(|| {
            half_levels
                .nearest(middle, FORM_LINE_SNAP)
                .and_then(|i| out2_lines[i].elevation)
        });
        add_line(
            FORM_LINE,
            &e.ring_or_path(),
            level,
            e.is_ring(),
            &mut objects,
            &mut levels,
            &mut stats,
        );
    }

    let ticks = out2
        .iter()
        .filter(|e| e.layer == "slope_line" && e.points.len() >= 2);
    for e in ticks {
        if !options.slope_lines {
            continue;
        }
        let (a, b) = (e.points[0], *e.points.last().unwrap());
        if !options.bounds.contains(a) && !options.bounds.contains(b) {
            continue;
        }
        let nearest = rings
            .iter()
            .flat_map(|r| {
                [(a, b), (b, a)].map(|(on, off)| (distance_to_path(on, &r.points), on, off, r))
            })
            .min_by(|x, y| x.0.total_cmp(&y.0));
        let Some((distance, on, off, ring)) = nearest else {
            stats.slope_lines_dropped += 1;
            continue;
        };
        if distance > SLOPE_LINE_SNAP {
            stats.slope_lines_dropped += 1;
            continue;
        }
        // Downhill is into the depression, whichever way the tick was drawn.
        let inward = if point_in_ring(off, &ring.points) {
            (off.0 - on.0, off.1 - on.1)
        } else {
            (on.0 - off.0, on.1 - off.1)
        };
        let (x, y) = paper(on);
        let mut point = Object {
            symbol_id: ring.slope_symbol,
            coords: vec![Coord::new(x, y, 0)],
            ..Object::new(ObjectKind::Point)
        };
        point.rotation = slope_line_rotation((inward.0, -inward.1));
        objects.push(point);
        levels.push(None);
        stats.slope_lines += 1;
    }
    if !options.slope_lines {
        stats.slope_lines_dropped = 0;
    }

    ImportedMap {
        georeference: Georeference {
            scale: options.scale,
            epsg: options.epsg,
            ref_point: (cx, cy),
            ref_lat_lon: options.center_lat_lon,
            grivation: 0.0,
            declination: 0.0,
            grid_scale_factor: 1.0,
        },
        objects,
        levels,
        stats,
    }
}

/// The rotation of a slope line point whose downhill direction on the paper
/// (`y` down) is `(dx, dy)`.
///
/// `step1_extract` reads a slope line's downhill direction as
/// `(-sin r, -cos r)` (its symbol's local "up", turned by the rotation), so
/// this is that, solved for `r`.
pub fn slope_line_rotation((dx, dy): (f64, f64)) -> f64 {
    (-dx).atan2(-dy)
}

/// The shortest distance from `p` to the polyline `path`.
fn distance_to_path(p: (f64, f64), path: &[(f64, f64)]) -> f64 {
    path.windows(2)
        .map(|s| distance_to_segment(p, s[0], s[1]))
        .fold(f64::INFINITY, f64::min)
}

fn distance_to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

/// Even-odd point-in-polygon.
fn point_in_ring(p: (f64, f64), ring: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            inside = !inside;
        }
    }
    inside
}

/// The one colour the contour symbols are drawn in.
fn brown() -> Color {
    Color {
        name: "Brown 100%".to_string(),
        rgb: (0.706, 0.369, 0.161),
        cmyk: (0.0, 0.56, 1.0, 0.18),
        opacity: 1.0,
    }
}

fn line(name: &str, code: &str, width: f64) -> LineSymbol {
    LineSymbol {
        name: name.to_string(),
        code: code.to_string(),
        color: 0,
        line_width: width,
        ..LineSymbol::default()
    }
}

/// A slope line: a rotatable tick from the point along its local "up",
/// which `step1_extract` reads as downhill.
fn slope_line(name: &str, code: &str, width: f64) -> PointSymbol {
    PointSymbol {
        name: name.to_string(),
        code: code.to_string(),
        is_rotatable: true,
        elements: vec![Element {
            symbol: Symbol::Line(line("", "", width)),
            object: Object {
                coords: vec![Coord::new(0.0, 0.0, 0), Coord::new(0.0, -0.8, 0)],
                ..Object::new(ObjectKind::Path(Default::default()))
            },
        }],
        ..PointSymbol::new()
    }
}

/// The colours and symbols an imported map is drawn with, by id: the ISOM
/// 2017 contour symbols.
pub fn symbols() -> (Vec<Color>, Vec<(i32, Symbol)>) {
    let form_line = LineSymbol {
        dashed: true,
        dash_length: 2.5,
        break_length: 0.25,
        ..line("Form line", "103", 0.1)
    };
    (
        vec![brown()],
        vec![
            (CONTOUR, Symbol::Line(line("Contour", "101", 0.14))),
            (
                SLOPE_LINE_CONTOUR,
                Symbol::Point(slope_line("Slope line, contour", "101.1", 0.14)),
            ),
            (
                INDEX_CONTOUR,
                Symbol::Line(line("Index contour", "102", 0.25)),
            ),
            (FORM_LINE, Symbol::Line(form_line)),
            (
                SLOPE_LINE_FORM_LINE,
                Symbol::Point(slope_line("Slope line, form line", "103.1", 0.1)),
            ),
        ],
    )
}

impl ImportedMap {
    /// The map as Mapper's XML.
    pub fn to_xml(&self) -> String {
        let (colors, symbols) = symbols();
        let fragment = |id: i32, text: String| crate::xml_reader::Fragment { id, text };
        let colors: Vec<_> = colors
            .iter()
            .enumerate()
            .map(|(i, c)| fragment(-1, color_element(i, c)))
            .collect();
        let symbols: Vec<_> = symbols
            .iter()
            .map(|(id, s)| fragment(*id, symbol_element(*id, s)))
            .collect();
        MapFile {
            scale_denominator: self.georeference.scale,
            colors: &colors,
            symbols: &symbols,
            objects: &self.objects,
            rotatable: true,
            georeferencing: Some(georeferencing_element(&self.georeference)),
        }
        .to_xml()
    }

    /// Writes the map as an `.omap`.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        std::fs::write(path, self.to_xml())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// [`ImportedMap::levels`] as the text of a `levels.txt` sidecar: one
    /// `object-index level-in-meters` line per line object whose height is
    /// known, after a `#` comment. [`parse_levels`] reads it back.
    pub fn levels_text(&self) -> String {
        let mut out = String::from("# object_index level_m\n");
        for (i, level) in self.levels.iter().enumerate() {
            if let Some(level) = level {
                out.push_str(&format!("{i} {level}\n"));
            }
        }
        out
    }
}

/// Reads a `levels.txt` sidecar ([`ImportedMap::levels_text`]) back:
/// `(object index, level in meters)` pairs.
pub fn parse_levels(text: &str) -> Result<Vec<(usize, f64)>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let parsed = (|| {
            let index = fields.next()?.parse().ok()?;
            let level = fields.next()?.parse().ok()?;
            fields.next().is_none().then_some((index, level))
        })();
        out.push(parsed.ok_or_else(|| format!("line {}: {line:?} is not `index level`", i + 1))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour_symbols::{classify_symbol, contour_step, SymbolFamily};

    fn polyline(layer: &str, z: Option<f64>, points: &[(f64, f64)], closed: bool) -> Entity {
        Entity {
            layer: layer.to_string(),
            kind: EntityKind::Polyline,
            elevation: z,
            points: points.to_vec(),
            closed,
        }
    }

    fn square(cx: f64, cy: f64, r: f64) -> Vec<(f64, f64)> {
        vec![
            (cx - r, cy - r),
            (cx + r, cy - r),
            (cx + r, cy + r),
            (cx - r, cy + r),
        ]
    }

    fn options(slope_lines: bool) -> ImportOptions {
        ImportOptions {
            bounds: GroundBox {
                min_x: 1000.0,
                min_y: 2000.0,
                max_x: 1100.0,
                max_y: 2100.0,
            },
            scale: 10000,
            epsg: 3006,
            center_lat_lon: (58.0, 15.0),
            slope_lines,
        }
    }

    #[test]
    fn layers_become_the_isom_contour_symbols() {
        let out2 = vec![
            polyline(
                "contour",
                Some(5.0),
                &[(990.0, 2050.0), (1110.0, 2050.0)],
                false,
            ),
            polyline(
                "contour_index",
                Some(25.0),
                &square(1050.0, 2050.0, 30.0),
                true,
            ),
            polyline(
                "contour_intermed",
                Some(7.5),
                &[(1000.0, 2010.0), (1030.0, 2010.0), (1100.0, 2010.0)],
                false,
            ),
            polyline(
                "small_depression",
                Some(5.0),
                &[(1010.0, 2010.0), (1011.0, 2011.0)],
                false,
            ),
        ];
        let formlines = vec![polyline(
            "formline",
            None,
            &[(1020.0, 2010.0), (1030.0, 2010.0), (1040.0, 2010.0)],
            false,
        )];
        let map = import(&out2, &formlines, &options(true));
        let symbols: Vec<i32> = map.objects.iter().map(|o| o.symbol_id).collect();
        assert_eq!(symbols, vec![CONTOUR, INDEX_CONTOUR, FORM_LINE]);
        // The form line takes the height of the half-level line it is a piece
        // of, whose vertices it shares.
        assert_eq!(map.levels, vec![Some(5.0), Some(25.0), Some(7.5)]);
        assert_eq!(map.stats.contours, 1);
        assert_eq!(map.stats.index_contours, 1);
        assert_eq!(map.stats.form_lines, 1);
    }

    #[test]
    fn a_line_crossing_the_edge_is_clipped_and_a_ring_inside_stays_closed() {
        let out2 = vec![
            polyline(
                "contour",
                Some(5.0),
                &[(900.0, 2050.0), (1200.0, 2050.0)],
                false,
            ),
            polyline("contour", Some(10.0), &square(1050.0, 2050.0, 10.0), true),
        ];
        let map = import(&out2, &[], &options(false));
        let open = &map.objects[0].coords;
        assert_eq!(open.len(), 2);
        // 1000 m east is 50 m west of the center: 5 mm left at 1:10 000.
        assert!((open[0].x + 5.0).abs() < 1e-9 && open[0].y.abs() < 1e-9);
        assert!((open[1].x - 5.0).abs() < 1e-9);
        assert!(!open[1].is_close_point());

        let ring = &map.objects[1].coords;
        assert_eq!(ring.len(), 5);
        assert_eq!(ring[0].pos(), ring[4].pos());
        assert!(ring[4].is_close_point());
        // North is up on the paper: the ring's southern edge has the larger y.
        assert!((ring[0].y - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_depression_gets_a_slope_line_pointing_into_it() {
        let ring = square(1050.0, 2050.0, 10.0);
        // A tick on the southern edge, drawn pointing *out* (south): the
        // import still points it in.
        let out2 = vec![
            polyline("depression", Some(15.0), &ring, true),
            polyline(
                "slope_line",
                Some(15.0),
                &[(1050.0, 2040.0), (1050.0, 2034.0)],
                false,
            ),
        ];
        let map = import(&out2, &[], &options(true));
        assert_eq!(map.stats.slope_lines, 1);
        let tick = map.objects.last().unwrap();
        assert_eq!(tick.symbol_id, SLOPE_LINE_CONTOUR);
        assert!(matches!(tick.kind, ObjectKind::Point));
        // Into the ring is north: up on the paper, (0, -1), which
        // step1_extract reads back as (-sin r, -cos r).
        let r = tick.rotation;
        assert!(
            (-r.sin()).abs() < 1e-9 && (-r.cos() + 1.0).abs() < 1e-9,
            "{r}"
        );
        assert!((tick.coords[0].y - 1.0).abs() < 1e-9);

        let none = import(&out2, &[], &options(false));
        assert_eq!(none.stats.slope_lines, 0);
        assert!(none.objects.iter().all(|o| o.symbol_id == CONTOUR));
    }

    #[test]
    fn a_slope_line_whose_ring_was_clipped_is_dropped() {
        let ring = square(1000.0, 2050.0, 10.0);
        let out2 = vec![
            polyline("depression", Some(15.0), &ring, true),
            polyline(
                "slope_line",
                Some(15.0),
                &[(1000.0, 2040.0), (1000.0, 2046.0)],
                false,
            ),
        ];
        let map = import(&out2, &[], &options(true));
        assert_eq!(map.stats.slope_lines, 0);
        assert_eq!(map.stats.slope_lines_dropped, 1);
    }

    #[test]
    fn a_depression_intermed_ring_is_a_form_line_only_where_one_is_drawn() {
        let ring = square(1050.0, 2050.0, 10.0);
        let drawn = vec![polyline("formline", None, &ring, false)];
        let out2 = vec![polyline("depression_intermed", Some(12.5), &ring, true)];

        let map = import(&out2, &drawn, &options(true));
        assert_eq!(
            map.stats.form_lines, 1,
            "the ring, not the ring and the form line"
        );
        assert_eq!(map.stats.form_line_rings, 1);
        assert_eq!(map.levels, vec![Some(12.5)]);

        let map = import(&out2, &[], &options(true));
        assert_eq!(map.stats.form_lines, 0);
    }

    #[test]
    fn the_written_map_reads_back_as_contours_and_slope_lines() {
        let ring = square(1050.0, 2050.0, 10.0);
        let out2 = vec![
            polyline("depression_index", Some(25.0), &ring, true),
            polyline(
                "slope_line",
                Some(25.0),
                &[(1050.0, 2060.0), (1050.0, 2054.0)],
                false,
            ),
            polyline("contour", Some(30.0), &square(1050.0, 2050.0, 20.0), true),
        ];
        let imported = import(&out2, &[], &options(true));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.omap");
        imported.write(&path).unwrap();
        let (map, _warnings) = crate::render::read_map_file(&path).unwrap();

        assert_eq!(map.objects.len(), 3);
        let families: Vec<_> = map
            .objects
            .iter()
            .map(|o| classify_symbol(&map.symbols[o.symbol_index.unwrap()]))
            .collect();
        assert_eq!(
            families,
            vec![
                Some(SymbolFamily::Contour),
                Some(SymbolFamily::Contour),
                Some(SymbolFamily::SlopeLine),
            ]
        );
        let index = &map.symbols[map.objects[0].symbol_index.unwrap()];
        assert_eq!(contour_step(index), 1.0);
        let georef = map.georeferencing.as_ref().unwrap();
        assert_eq!(georef.epsg, 3006);
        assert_eq!(georef.scale, 10000);
        assert!((georef.ref_point_x - 1050.0).abs() < 1e-6);
        assert!((georef.ref_point_y - 2050.0).abs() < 1e-6);
        // The tick's rotation survives the round trip.
        assert!((map.objects[2].rotation - imported.objects[2].rotation).abs() < 1e-5);
    }

    #[test]
    fn levels_round_trip_through_their_sidecar() {
        let out2 = vec![
            polyline(
                "contour",
                Some(5.0),
                &[(1010.0, 2050.0), (1090.0, 2050.0)],
                false,
            ),
            polyline(
                "contour",
                Some(-2.5),
                &[(1010.0, 2060.0), (1090.0, 2060.0)],
                false,
            ),
        ];
        let map = import(&out2, &[], &options(true));
        let text = map.levels_text();
        assert_eq!(parse_levels(&text).unwrap(), vec![(0, 5.0), (1, -2.5)]);
        assert!(parse_levels("0 1 2\n").is_err());
    }

    #[test]
    fn a_slope_line_rotation_points_where_asked() {
        for (dx, dy) in [(1.0, 0.0), (0.0, 1.0), (-0.6, 0.8), (0.0, -1.0)] {
            let r = slope_line_rotation((dx, dy));
            assert!((-r.sin() - dx).abs() < 1e-12 && (-r.cos() - dy).abs() < 1e-12);
        }
    }
}
