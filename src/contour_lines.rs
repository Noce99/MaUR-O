//! Contour lines from an elevation raster: the other way round from
//! `mauro dem`, which reads a map's contours to work out the ground. This
//! draws the ground's contours, at a given equidistance, from a terrain
//! model (the GeoTIFF `mauro terrain` downloads, say), with the map's own
//! contour symbols, as a part of the map.
//!
//! The steps, in the order [`contour_objects`] takes them:
//!
//! - [`smooth`]: a Gaussian blur of the raster, so a LiDAR model's every
//!   stone and furrow does not become a wiggle of its own. Its width is in
//!   meters on the ground ([`Options::smooth`]); a pixel without an elevation
//!   stays without one, and lends nothing to its neighbours.
//! - [`levels`]: the elevations to draw, multiples of the equidistance, and
//!   what each is drawn as: a contour, an index contour (every
//!   [`Options::index_every`]th), or a form line halfway between two
//!   contours, where those are asked for.
//! - [`trace`]: marching squares over the raster's pixel centres. A saddle
//!   (a cell whose two diagonals are on different sides of the level) is
//!   decided by the cell's mean. A cell with a corner without an elevation
//!   is not crossed by anything, so a line ends where the model does, open.
//! - [`simplify`]: Douglas–Peucker, to [`TOLERANCE_MM`] on the paper, which
//!   is far below what a printed line shows and keeps the file small.
//!
//! The lines are placed on the map through its own georeferencing
//! ([`crate::geotiff::projected_to_ground`]); the raster must already be in
//! the map's CRS, as `mauro terrain` writes it. Lines shorter than
//! [`Options::min_length`] are left out: closed ones that short are the
//! blur's leftovers, not knolls a map would show.
//!
//! [`add_contour_part`] puts them into a map file as a part of their own,
//! by editing the file's text (like [`crate::image_template`], so nothing
//! Mapper keeps that [`crate::map`] does not is lost), and replaces a part
//! of the same name where the map has one, so running again replaces what
//! the last run drew.

use std::collections::{HashMap, HashSet};

use geo::Coord as GeoCoord;

use crate::dem_geo_tiff_evaluator::GeoRaster;
use crate::geotiff::projected_to_ground;
use crate::image_template::{attr, escape, find_element, set_attr};
use crate::map::{coord_flag, Coord, Georeferencing, Map, Object, ObjectKind, PathObject, Symbol};
use crate::xml_writer::object_element;

/// How far a simplified line may stray from the traced one, in mm on the
/// paper: a fifth of an ISOM contour's width.
pub const TOLERANCE_MM: f64 = 0.03;

/// What to draw, and how much to generalise it.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// The vertical distance between two contours, in meters.
    pub equidistance: f64,
    /// Every how many contours one is an index contour (counted from
    /// elevation 0); 0 for none.
    pub index_every: u32,
    /// Whether to draw form lines halfway between the contours.
    pub form_lines: bool,
    /// The standard deviation of the Gaussian blur, in meters on the
    /// ground; 0 for none.
    pub smooth: f64,
    /// The shortest line drawn, in meters on the ground.
    pub min_length: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            equidistance: 5.0,
            index_every: 5,
            form_lines: false,
            smooth: 2.0,
            min_length: 20.0,
        }
    }
}

/// What a level is drawn as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A contour (ISOM 101).
    Contour,
    /// An index contour (ISOM 102).
    Index,
    /// A form line (ISOM 103).
    FormLine,
}

/// An elevation to draw a line at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    /// In meters.
    pub elevation: f64,
    /// What it is drawn as.
    pub kind: Kind,
}

/// The levels between `low` and `high` (both included), lowest first.
pub fn levels(low: f64, high: f64, options: &Options) -> Vec<Level> {
    let step = if options.form_lines {
        options.equidistance / 2.0
    } else {
        options.equidistance
    };
    if !(step > 0.0 && low.is_finite() && high.is_finite() && low <= high) {
        return Vec::new();
    }
    let first = (low / step).ceil() as i64;
    let last = (high / step).floor() as i64;
    (first..=last)
        .map(|k| {
            let (contour, form_line) = if options.form_lines {
                (k.div_euclid(2), k.rem_euclid(2) == 1)
            } else {
                (k, false)
            };
            let kind = if form_line {
                Kind::FormLine
            } else if options.index_every > 0
                && contour.rem_euclid(i64::from(options.index_every)) == 0
            {
                Kind::Index
            } else {
                Kind::Contour
            };
            Level {
                elevation: k as f64 * step,
                kind,
            }
        })
        .collect()
}

/// `values` (`width` × `height`, row by row, `NaN` for no elevation) blurred
/// by a Gaussian of standard deviation `sigma` pixels. A pixel with no
/// elevation keeps none and is left out of its neighbours' means. Below a
/// third of a pixel there is nothing to blur, and `values` come back as
/// they are.
pub fn smooth(values: &[f64], width: usize, height: usize, sigma: f64) -> Vec<f64> {
    if sigma.is_nan() || sigma < 1.0 / 3.0 {
        return values.to_vec();
    }
    let radius = (3.0 * sigma).ceil() as isize;
    let kernel: Vec<f64> = (-radius..=radius)
        .map(|d| (-((d * d) as f64) / (2.0 * sigma * sigma)).exp())
        .collect();
    let pass = |input: &[f64], along_rows: bool| -> Vec<f64> {
        let mut out = vec![f64::NAN; input.len()];
        for r in 0..height {
            for c in 0..width {
                let at = r * width + c;
                if input[at].is_nan() {
                    continue;
                }
                let (mut sum, mut weight) = (0.0, 0.0);
                for (k, w) in kernel.iter().enumerate() {
                    let d = k as isize - radius;
                    let (rr, cc) = if along_rows {
                        (r as isize, c as isize + d)
                    } else {
                        (r as isize + d, c as isize)
                    };
                    if rr < 0 || cc < 0 || rr >= height as isize || cc >= width as isize {
                        continue;
                    }
                    let v = input[rr as usize * width + cc as usize];
                    if !v.is_nan() {
                        sum += w * v;
                        weight += w;
                    }
                }
                out[at] = sum / weight;
            }
        }
        out
    };
    pass(&pass(values, true), false)
}

/// A line traced at one level.
#[derive(Clone, Debug, PartialEq)]
pub struct Isoline {
    /// The index of its level in the levels [`trace`] was given.
    pub level: usize,
    /// Its points, in pixels: `(col, row)`, with pixel `(c, r)`'s centre at
    /// `(c + 0.5, r + 0.5)` -- the raster's own pixel-corner convention.
    /// A closed line does not repeat its first point at the end.
    pub points: Vec<(f64, f64)>,
    /// Whether it closes on itself.
    pub closed: bool,
}

/// No edge: an empty slot in [`trace`]'s links.
const NONE: u64 = u64::MAX;

/// The lines at each of `levels` (ascending) through `values` (`width` ×
/// `height`, row by row, `NaN` for no elevation), by marching squares over
/// the pixel centres. A pixel at exactly a level counts as above it.
///
/// Every crossing lies on an edge between two pixel centres, and each edge
/// is crossed by at most two cells' segments, so the segments are chained
/// into lines by the edges they share. Lines come out level by level, the
/// open ones (ending where the raster or its elevations do) first, in an
/// order that depends on the raster alone.
pub fn trace(values: &[f64], width: usize, height: usize, levels: &[f64]) -> Vec<Isoline> {
    assert_eq!(values.len(), width * height, "the raster's size");
    let w = width as u64;
    let horizontal = |r: usize, c: usize| (r as u64 * w + c as u64) << 1;
    let vertical = |r: usize, c: usize| ((r as u64 * w + c as u64) << 1) | 1;

    let mut segments: Vec<Vec<(u64, u64)>> = vec![Vec::new(); levels.len()];
    for r in 0..height.saturating_sub(1) {
        for c in 0..width.saturating_sub(1) {
            // Clockwise from the top left.
            let v = [
                values[r * width + c],
                values[r * width + c + 1],
                values[(r + 1) * width + c + 1],
                values[(r + 1) * width + c],
            ];
            if v.iter().any(|x| x.is_nan()) {
                continue;
            }
            let low = v.iter().copied().fold(f64::INFINITY, f64::min);
            let high = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            // Top, right, bottom, left: the sides between corners 0-1, 1-2,
            // 3-2 and 0-3.
            let edges = [
                horizontal(r, c),
                vertical(r, c + 1),
                horizontal(r + 1, c),
                vertical(r, c),
            ];
            let first = levels.partition_point(|&l| l <= low);
            for (i, &level) in levels.iter().enumerate().skip(first) {
                if level > high {
                    break;
                }
                let above = v.map(|x| x >= level);
                let crossed = [
                    above[0] != above[1],
                    above[1] != above[2],
                    above[3] != above[2],
                    above[0] != above[3],
                ];
                if crossed.iter().all(|&x| x) {
                    // A saddle: the cell's mean says which diagonal is joined.
                    let centre = v.iter().sum::<f64>() / 4.0 >= level;
                    if centre == above[0] {
                        segments[i].push((edges[0], edges[1]));
                        segments[i].push((edges[2], edges[3]));
                    } else {
                        segments[i].push((edges[0], edges[3]));
                        segments[i].push((edges[1], edges[2]));
                    }
                } else {
                    let mut ends = (0..4).filter(|&e| crossed[e]).map(|e| edges[e]);
                    if let (Some(a), Some(b)) = (ends.next(), ends.next()) {
                        segments[i].push((a, b));
                    }
                }
            }
        }
    }

    let point = |edge: u64, level: f64| -> (f64, f64) {
        let at = (edge >> 1) as usize;
        let (r, c) = (at / width, at % width);
        let (dr, dc) = if edge & 1 == 0 { (0, 1) } else { (1, 0) };
        let a = values[r * width + c];
        let b = values[(r + dr) * width + c + dc];
        let t = ((level - a) / (b - a)).clamp(0.0, 1.0);
        (
            c as f64 + 0.5 + t * dc as f64,
            r as f64 + 0.5 + t * dr as f64,
        )
    };

    let mut lines = Vec::new();
    for (i, level_segments) in segments.into_iter().enumerate() {
        let mut links: HashMap<u64, [u64; 2]> = HashMap::with_capacity(level_segments.len() * 2);
        for (a, b) in level_segments {
            for (from, to) in [(a, b), (b, a)] {
                let slots = links.entry(from).or_insert([NONE, NONE]);
                if slots[0] == NONE {
                    slots[0] = to;
                } else {
                    slots[1] = to;
                }
            }
        }
        let mut starts: Vec<u64> = links.keys().copied().collect();
        starts.sort_unstable();
        let mut seen = HashSet::with_capacity(links.len());
        // Ends first, so an open line is walked from one end to the other,
        // then whatever is left: closed lines.
        for open in [true, false] {
            for &start in &starts {
                if seen.contains(&start) || (open && links[&start][1] != NONE) {
                    continue;
                }
                let mut points = Vec::new();
                let (mut previous, mut current) = (NONE, start);
                let closed = loop {
                    points.push(point(current, levels[i]));
                    seen.insert(current);
                    let [a, b] = links[&current];
                    let next = if a != previous { a } else { b };
                    if next == NONE {
                        break false;
                    }
                    if next == start {
                        break true;
                    }
                    if seen.contains(&next) {
                        break false;
                    }
                    previous = current;
                    current = next;
                };
                points.dedup();
                lines.push(Isoline {
                    level: i,
                    points,
                    closed,
                });
            }
        }
    }
    lines
}

/// `points` simplified by Douglas–Peucker: the fewest of them, ends kept,
/// that the line strays no further than `tolerance` from.
pub fn simplify(points: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((first, last)) = stack.pop() {
        let (a, b) = (points[first], points[last]);
        let mut farthest = (0.0, first);
        for (i, &p) in points.iter().enumerate().take(last).skip(first + 1) {
            let d = distance_to_segment(p, a, b);
            if d > farthest.0 {
                farthest = (d, i);
            }
        }
        if farthest.0 > tolerance {
            keep[farthest.1] = true;
            stack.push((first, farthest.1));
            stack.push((farthest.1, last));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(p, _)| *p)
        .collect()
}

fn distance_to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length2 = dx * dx + dy * dy;
    let t = if length2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// The ids of the map's own symbols the lines are drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContourSymbols {
    /// Contour, ISOM/ISSprOM 101.
    pub contour: Option<i32>,
    /// Index contour, 102.
    pub index: Option<i32>,
    /// Form line, 103.
    pub form_line: Option<i32>,
}

impl ContourSymbols {
    /// The map's line symbols coded 101, 102 and 103 (or `101.0`, as OCAD
    /// writes them): the first of each, helper symbols left out.
    pub fn of(map: &Map) -> ContourSymbols {
        let find = |family: &str| {
            map.symbols
                .iter()
                .zip(&map.symbol_ids)
                .find(|(symbol, _)| {
                    let code = symbol.code();
                    let (head, rest) = code.split_once('.').unwrap_or((code, ""));
                    matches!(symbol, Symbol::Line(_))
                        && !symbol.is_helper_symbol()
                        && head == family
                        && rest.chars().all(|c| c == '0')
                })
                .map(|(_, &id)| id)
        };
        ContourSymbols {
            contour: find("101"),
            index: find("102"),
            form_line: find("103"),
        }
    }

    /// The symbol a level of this kind is drawn with.
    pub fn for_kind(&self, kind: Kind) -> Option<i32> {
        match kind {
            Kind::Contour => self.contour,
            Kind::Index => self.index,
            Kind::FormLine => self.form_line,
        }
    }
}

/// The contour lines of `raster` (in the map's CRS), as objects of a map
/// georeferenced by `georef` and drawn with `symbols`. An error where the
/// map is not placed in a projected CRS, or a symbol a level needs is
/// missing.
pub fn contour_objects(
    raster: &GeoRaster,
    georef: &Georeferencing,
    symbols: &ContourSymbols,
    options: &Options,
) -> Result<Vec<Object>, String> {
    if georef.epsg == 0 || georef.scale <= 0 {
        return Err("the map is not georeferenced in a projected CRS".to_string());
    }
    let t = &raster.transform;
    let pixel = (t.col_x.hypot(t.col_y) * t.row_x.hypot(t.row_y)).sqrt();
    let values = smooth(
        &raster.values,
        raster.width,
        raster.height,
        options.smooth / pixel,
    );
    let (low, high) = values
        .iter()
        .filter(|v| !v.is_nan())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    let levels = levels(low, high, options);
    let mut ids = Vec::with_capacity(levels.len());
    for level in &levels {
        let id = symbols.for_kind(level.kind).ok_or_else(|| {
            format!(
                "the map has no {} symbol ({})",
                match level.kind {
                    Kind::Contour => "contour",
                    Kind::Index => "index contour",
                    Kind::FormLine => "form line",
                },
                match level.kind {
                    Kind::Contour => "101",
                    Kind::Index => "102",
                    Kind::FormLine => "103",
                }
            )
        })?;
        ids.push(id);
    }
    let elevations: Vec<f64> = levels.iter().map(|l| l.elevation).collect();
    let mm_per_meter = 1000.0 / f64::from(georef.scale);

    let mut objects = Vec::new();
    for line in trace(&values, raster.width, raster.height, &elevations) {
        let mut paper = Vec::with_capacity(line.points.len() + 1);
        for &(col, row) in &line.points {
            let (x, y) = t.pixel_to_world(col, row);
            let ground = projected_to_ground(georef, GeoCoord { x, y })
                .ok_or("the map is not georeferenced in a projected CRS")?;
            paper.push((ground.x * mm_per_meter, ground.y * mm_per_meter));
        }
        if line.closed {
            paper.push(paper[0]);
        }
        let length_mm: f64 = paper
            .windows(2)
            .map(|p| (p[1].0 - p[0].0).hypot(p[1].1 - p[0].1))
            .sum();
        if length_mm < options.min_length * mm_per_meter {
            continue;
        }
        let paper = simplify(&paper, TOLERANCE_MM);
        if paper.len() < 2 || (line.closed && paper.len() < 4) {
            continue;
        }
        let last = paper.len() - 1;
        let mut object = Object::new(ObjectKind::Path(PathObject::default()));
        object.symbol_id = ids[line.level];
        object.coords = paper
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| {
                let flags = if line.closed && i == last {
                    coord_flag::CLOSE_POINT
                } else {
                    0
                };
                Coord::new(x, y, flags)
            })
            .collect();
        objects.push(object);
    }
    Ok(objects)
}

/// What [`add_contour_part`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartAdded {
    /// The map had no part of that name; it has one now, its last.
    Added,
    /// The map's part of that name holds the new objects instead of its own.
    Replaced,
}

/// `xml` (a whole `.omap`/`.xmap` file) with `objects` in a part named
/// `name`: a new one after the others, or the one already of that name,
/// emptied first. Only the `<parts>` list changes (its `count` where a part
/// is added). An error where the file has no `<parts>` list.
pub fn add_contour_part(
    xml: &str,
    name: &str,
    objects: &[Object],
) -> Result<(String, PartAdded), String> {
    let parts = find_element(xml, "parts", 0).ok_or("the file has no map parts")?;
    let parts_end = parts.end.ok_or("the file's map parts list is empty")?;

    let mut part = format!(
        "<part name=\"{}\"><objects count=\"{}\">\n",
        escape(name),
        objects.len()
    );
    for object in objects {
        part.push_str(&object_element(object, false));
        part.push('\n');
    }
    part.push_str("</objects></part>");

    // A part of the same name, to replace.
    let mut from = parts.open_end;
    while let Some(existing) = find_element(xml, "part", from) {
        if existing.start >= parts_end {
            break;
        }
        let open_tag = &xml[existing.start..existing.open_end];
        let end = match existing.end {
            Some(end) => end + "</part>".len(),
            None => existing.open_end,
        };
        if attr(open_tag, "name") == Some(escape(name).as_str()) {
            let out = format!("{}{part}{}", &xml[..existing.start], &xml[end..]);
            return Ok((out, PartAdded::Replaced));
        }
        from = end;
    }

    let open_tag = &xml[parts.start..parts.open_end];
    let count = attr(open_tag, "count")
        .and_then(|c| c.parse::<u32>().ok())
        .unwrap_or(0);
    let out = format!(
        "{}{}{}{part}\n{}",
        &xml[..parts.start],
        set_attr(open_tag, "count", &(count + 1).to_string()),
        &xml[parts.open_end..parts_end],
        &xml[parts_end..]
    );
    Ok((out, PartAdded::Added))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cone `size` pixels square, `top` meters high in the middle,
    /// falling one meter per pixel.
    fn cone(size: usize, top: f64) -> Vec<f64> {
        let centre = (size as f64 - 1.0) / 2.0;
        (0..size * size)
            .map(|i| {
                let (r, c) = ((i / size) as f64, (i % size) as f64);
                top - (r - centre).hypot(c - centre)
            })
            .collect()
    }

    #[test]
    fn a_cone_has_one_closed_ring_per_level_at_its_radius() {
        let values = cone(41, 100.0);
        let lines = trace(&values, 41, 41, &[90.0, 95.0]);
        assert_eq!(lines.len(), 2);
        for (line, radius) in lines.iter().zip([10.0, 5.0]) {
            assert!(line.closed);
            for &(x, y) in &line.points {
                // Pixel centres sit at +0.5; the cone's top at 20.5.
                let r = (x - 20.5).hypot(y - 20.5);
                assert!((r - radius).abs() < 0.1, "{r} vs {radius}");
            }
        }
    }

    #[test]
    fn a_slope_has_one_open_line_across_the_raster() {
        // Rising one meter per column.
        let values: Vec<f64> = (0..10 * 5).map(|i| (i % 10) as f64).collect();
        let lines = trace(&values, 10, 5, &[4.5]);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert!(!line.closed);
        assert_eq!(line.points.len(), 5);
        assert!(line.points.iter().all(|&(x, _)| (x - 5.0).abs() < 1e-9));
    }

    #[test]
    fn a_gap_in_the_elevations_breaks_a_line() {
        let mut values: Vec<f64> = (0..10 * 9).map(|i| (i % 10) as f64).collect();
        values[4 * 10 + 4] = f64::NAN;
        let lines = trace(&values, 10, 9, &[4.5]);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| !l.closed));
    }

    #[test]
    fn a_saddle_follows_the_cells_mean() {
        // High corners top left and bottom right; the mean is 5.
        let values = [10.0, 0.0, 0.0, 10.0];
        let near = |lines: &[Isoline], corner: fn(f64, f64) -> bool| {
            lines
                .iter()
                .any(|l| l.points.iter().all(|&(x, y)| corner(x, y)))
        };
        // At 4 the mean is above: the high corners are joined, and the low
        // top right one is cut off by a line of its own.
        let at4 = trace(&values, 2, 2, &[4.0]);
        assert_eq!(at4.len(), 2);
        assert!(near(&at4, |x, y| x >= 1.0 && y <= 1.0));
        // At 6 it is below: the low corners are joined, the high top left
        // one cut off.
        let at6 = trace(&values, 2, 2, &[6.0]);
        assert_eq!(at6.len(), 2);
        assert!(near(&at6, |x, y| x <= 1.0 && y <= 1.0));
    }

    #[test]
    fn levels_mark_index_contours_and_form_lines() {
        let options = Options {
            equidistance: 5.0,
            index_every: 5,
            ..Options::default()
        };
        let got = levels(12.0, 31.0, &options);
        let elevations: Vec<f64> = got.iter().map(|l| l.elevation).collect();
        assert_eq!(elevations, [15.0, 20.0, 25.0, 30.0]);
        assert_eq!(got[2].kind, Kind::Index);
        assert_eq!(got[0].kind, Kind::Contour);

        let with_form = Options {
            form_lines: true,
            ..options
        };
        let got = levels(-5.0, 5.0, &with_form);
        let kinds: Vec<Kind> = got.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Contour,
                Kind::FormLine,
                Kind::Index,
                Kind::FormLine,
                Kind::Contour
            ]
        );
        assert_eq!(got[2].elevation, 0.0);
    }

    #[test]
    fn smoothing_keeps_a_plane_and_its_gaps() {
        let mut values: Vec<f64> = (0..20 * 20).map(|i| (i % 20) as f64).collect();
        values[0] = f64::NAN;
        let smoothed = smooth(&values, 20, 20, 1.5);
        assert!(smoothed[0].is_nan());
        // Away from the edges a plane is its own blur.
        assert!((smoothed[10 * 20 + 10] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn simplifying_drops_points_on_a_straight_line() {
        let points: Vec<(f64, f64)> = (0..10).map(|i| (i as f64, 0.0)).collect();
        assert_eq!(simplify(&points, 0.01), [(0.0, 0.0), (9.0, 0.0)]);
        let bent = [(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)];
        assert_eq!(simplify(&bent, 0.5), bent);
    }

    const MAP: &str = "<map><barrier><parts count=\"1\" current=\"0\">\n\
         <part name=\"default part\"><objects count=\"0\">\n</objects></part>\n</parts>\
         <templates count=\"0\"/></barrier></map>";

    fn line() -> Object {
        let mut object = Object::new(ObjectKind::Path(PathObject::default()));
        object.symbol_id = 3;
        object.coords = vec![Coord::new(0.0, 0.0, 0), Coord::new(1.0, 2.0, 0)];
        object
    }

    #[test]
    fn a_part_is_added_after_the_others_and_replaced_after() {
        let (once, added) = add_contour_part(MAP, "Contours 5 m", &[line()]).unwrap();
        assert_eq!(added, PartAdded::Added);
        assert!(once.contains("<parts count=\"2\" current=\"0\">"));
        let ours = once.find("<part name=\"Contours 5 m\">").unwrap();
        assert!(ours > once.find("default part").unwrap());
        assert!(once[ours..].contains("<objects count=\"1\">"));
        assert!(once.contains("<coords count=\"2\">0 0;1000 2000;</coords>"));

        let (twice, replaced) = add_contour_part(&once, "Contours 5 m", &[]).unwrap();
        assert_eq!(replaced, PartAdded::Replaced);
        assert!(twice.contains("<parts count=\"2\""));
        assert_eq!(twice.matches("Contours 5 m").count(), 1);
        assert!(!twice.contains("<coords"));
        assert!(twice.ends_with("<templates count=\"0\"/></barrier></map>"));
    }

    #[test]
    fn a_file_without_parts_is_refused() {
        assert!(add_contour_part("<map></map>", "x", &[]).is_err());
    }

    #[test]
    fn contour_symbols_are_found_by_code() {
        let xml = std::fs::read_to_string("tests/data/contours.xmap").unwrap();
        let (map, _) = crate::xml_reader::read_xml_map_str(&xml).unwrap();
        let symbols = ContourSymbols::of(&map);
        // 101.0 is the contour; 101.1, a slope line, is a point symbol.
        assert_eq!(symbols.contour, Some(0));
        assert_eq!(symbols.index, None);
    }
}
