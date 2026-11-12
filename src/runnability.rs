//! How fast a map is to run through, as a grid.
//!
//! A runnability raster answers, for every cell of a map, how quickly a
//! runner crosses it: `1.0` is a reference pace on open ground, lower is
//! slower, and `0.0` is impassable. It is what a route optimizer searches
//! over, and what a map looks like once the question "where can I run" is the
//! only one being asked of it.
//!
//! The values come from the caller. A mapping standard says what a symbol
//! means on the ground -- ISOM's 403 is rough open land, its 410 is fight --
//! and turning that into a speed is a judgement which belongs to the caller
//! and its standard, not to this crate. So [`Options::speeds`] is a list of
//! symbol codes and the speed each stands for, and everything here is about
//! putting those numbers onto a grid.
//!
//! ```no_run
//! use maur_o::{runnability, xml_reader};
//!
//! let (map, _) = xml_reader::read_xml_map_str("<map/>").unwrap();
//! let raster = runnability::build(
//!     &map,
//!     &runnability::Options {
//!         speeds: vec![("403".to_string(), 0.8), ("410".to_string(), 0.2)],
//!         pixel_size: 0.15,
//!         fill_value: 0.9,
//!         ..runnability::Options::default()
//!     },
//! )
//! .unwrap();
//! println!("{} x {} cells", raster.width, raster.height);
//! ```
//!
//! # How a symbol becomes a cell
//!
//! Every object drawn with a symbol the caller gave a speed for is drawn onto
//! the grid, and every cell it touches takes that speed; where shapes
//! overlap, the one drawn last wins. Areas go first, in the order the map
//! itself displays them -- by the topmost colour each fills with, so the area
//! a reader sees on top is the one whose speed a cell takes -- then points,
//! then lines, the slowest line last of all, so that a path over a marsh
//! reads as a path and the marsh does not win by being drawn later.
//!
//! A speed which is not a finite number claims its symbols without drawing
//! them: an entry `("201.1", f64::NAN)` ahead of `("201", 0.05)` leaves
//! `201.1` off the grid while `201` and its other variants keep their speed.
//!
//! A cell is claimed by any coverage at all, not by half of it: a raster of
//! running speeds is not a picture, and a cliff which clips the corner of a
//! cell is still a cliff in the way. Coverage is what the rasterizer sees, so
//! a shape grazing a cell by a thousandth of it may fall below what can be
//! measured -- the boundary is sub-cell either way, and a cell is the
//! smallest thing this answers about.
//!
//! # Overlaps
//!
//! A speed may be given for a *combination* of codes, written with a `+`:
//! `"403+410"` is the speed where rough open land and fight overlap. Those
//! are applied after the single codes, to the cells every one of their
//! components covers.
//!
//! # Widening lines and shrinking areas
//!
//! A map and a GPS track are both a little off, and a route which follows a
//! path on the map may sit a few metres beside it in the track. Two lists
//! let a caller allow for that, by code, matched the way speeds are:
//!
//! - [`Options::line_widths`] draws a line wider than the default, so a path
//!   claims a strip a runner on it can actually be inside.
//! - [`Options::shrink`] pulls an area in from its edge before it is drawn,
//!   so a thicket's fringe, where the map and the track disagree, reads as
//!   whatever lies under it rather than as thicket. Cells an area gives up
//!   keep what was drawn there before it, or the background.
//!
//! Neither is applied to anything unless the caller names its code, and both
//! are lengths in mm on the paper, like [`Options::pixel_size`].

use std::collections::HashMap;

use tiny_skia::{FillRule, LineCap, LineJoin, Mask, Stroke, Transform};

use crate::geometry::{add_ellipse, to_painter_path, Path, Rect};
use crate::map::{
    fill_pattern_type, is_color, symbol_leaves, Map, Object, Point, PointSymbol, Symbol,
};
use crate::renderer::to_skia_path;

/// The default ceiling on the number of cells, past which the cell size is
/// enlarged rather than the grid.
pub const DEFAULT_MAX_CELLS: usize = 2_500_000;

/// A margin given to a shape with no thickness of its own -- a straight line,
/// a single point -- so that its bounding box is not empty. In mm, and small
/// enough to be invisible at any cell size a map is rasterized at.
const DEGENERATE_MARGIN: f64 = 0.001;

/// What to rasterize, and how finely.
#[derive(Debug, Clone)]
pub struct Options {
    /// Symbol codes and the speed each stands for, in the caller's own order.
    ///
    /// A code matches a symbol exactly, or by the part before its dot -- so
    /// `"403"` also covers `403.1` unless that has an entry of its own. Where
    /// two entries match, the first in this list wins, which is why the order
    /// is the caller's to decide.
    ///
    /// A code containing `+` names an overlap: see the module docs.
    pub speeds: Vec<(String, f64)>,
    /// The length of a cell's side, in mm on the paper.
    pub pixel_size: f64,
    /// The speed of a cell no symbol claims -- the map's background.
    pub fill_value: f64,
    /// Whether to mark everything outside the convex hull of the map's own
    /// objects as unknown, rather than leaving it at `fill_value`.
    pub mask_outside_convex_hull: bool,
    /// Whether to draw a line a little wider than one cell, so that a path
    /// crossing a grid diagonally stays connected on it.
    pub buffer_lines: bool,
    /// Line symbols to draw wider than one cell: a code and the full width,
    /// in mm on the paper. A line is never drawn narrower than it would be
    /// without an entry. Matched like [`speeds`](Self::speeds), the first
    /// matching entry winning; codes of areas and points are ignored.
    pub line_widths: Vec<(String, f64)>,
    /// Area symbols to pull in from their edge before they are drawn: a code
    /// and the distance, in mm on the paper. Matched like
    /// [`speeds`](Self::speeds); codes of lines and points are ignored, since
    /// a line or a point shrunk by any amount worth asking for would vanish.
    pub shrink: Vec<(String, f64)>,
    /// The ceiling on cells; past it the cell size grows instead.
    pub max_cells: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            speeds: Vec::new(),
            pixel_size: 0.15,
            fill_value: 0.9,
            mask_outside_convex_hull: true,
            buffer_lines: true,
            line_widths: Vec::new(),
            shrink: Vec::new(),
            max_cells: DEFAULT_MAX_CELLS,
        }
    }
}

/// One symbol code which actually reached the grid, and how much of it it took.
#[derive(Debug, Clone)]
pub struct CodeUse {
    /// The code, as the map writes it.
    pub code: String,
    /// The speed it was given.
    pub speed: f64,
    /// How many cells it claimed, counted as it was drawn -- so a code drawn
    /// over by a later one still counts the cells it had.
    pub cells: u64,
}

/// A map as a grid of running speeds.
#[derive(Debug, Clone)]
pub struct Raster {
    /// Cells across.
    pub width: u32,
    /// Cells down.
    pub height: u32,
    /// The length of a cell's side, in mm. Not always what was asked for: see
    /// [`Options::max_cells`].
    pub pixel_size: f64,
    /// What the grid covers, in mm on the paper.
    pub bounds: Rect,
    /// One speed per cell, row by row from the top left. `NaN` where the cell
    /// is outside the map.
    pub values: Vec<f32>,
    /// The code each cell got its speed from, as an index into
    /// [`codes`](Self::codes); `-1` where the cell kept the background.
    pub code_index: Vec<i32>,
    /// The codes referred to by [`code_index`](Self::code_index).
    pub codes: Vec<String>,
    /// Which codes reached the grid, in code order.
    pub used_codes: Vec<CodeUse>,
    /// What happened, for a caller with somewhere to show it.
    pub log: Vec<String>,
}

impl Raster {
    /// The speed at a point in mm on the paper, and the code it came from.
    ///
    /// `None` outside the grid, and for a cell the map does not cover.
    pub fn sample(&self, x: f64, y: f64) -> Option<(f32, Option<&str>)> {
        let col = ((x - self.bounds.left()) / self.pixel_size).floor();
        let row = ((y - self.bounds.top()) / self.pixel_size).floor();
        if col < 0.0 || row < 0.0 || col >= f64::from(self.width) || row >= f64::from(self.height) {
            return None;
        }
        let i = row as usize * self.width as usize + col as usize;
        let value = *self.values.get(i)?;
        if !value.is_finite() {
            return None;
        }
        let code = match self.code_index.get(i) {
            Some(&idx) if idx >= 0 => self.codes.get(idx as usize).map(String::as_str),
            _ => None,
        };
        Some((value, code))
    }
}

/// What an object contributes to the grid: a shape, a speed, and how the
/// shape is drawn.
struct Shape {
    code: String,
    speed: f64,
    kind: Kind,
    /// The outline to fill or stroke. Empty for a point symbol, which is
    /// drawn at [`origin`](Shape::origin) and has no path to speak of.
    path: Path,
    /// The object's first coordinate: where a point symbol goes.
    origin: Point,
    /// Which object this came from, for the coordinates the hull is built on.
    object_index: usize,
    bounds: Rect,
    /// The width to stroke a line at, in mm; `0` for the default.
    line_width: f64,
    /// How far to pull an area in from its edge, in mm; `0` for not at all.
    shrink: f64,
    /// Where an area sits in the map's own drawing order: the priority of the
    /// topmost colour it fills with, smaller being higher (see
    /// [`display_priority`]). Unused for lines and points.
    display: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Area,
    Line,
    Point,
}

/// The part of a code before its dot: `403.1` belongs to `403`.
fn base_code(code: &str) -> &str {
    match code.split_once('.') {
        Some((base, _)) => base,
        None => code,
    }
}

/// Whether a speed's code covers a symbol's: exactly, or by its base.
fn matches(symbol_code: &str, speed_code: &str) -> bool {
    symbol_code == speed_code || base_code(symbol_code) == speed_code
}

/// How an object's shape is drawn, from the kinds of symbol it draws with.
///
/// An area wins over a line and a line over a point, since a symbol which
/// fills also outlines, and what the grid wants is the ground it covers.
fn kind_of(symbol: &Symbol, map: &Map) -> Option<Kind> {
    let mut resolved = Vec::new();
    symbol_leaves(symbol, map, &mut resolved);
    let mut best = None;
    for leaf in resolved {
        match leaf {
            Symbol::Area(_) => return Some(Kind::Area),
            Symbol::Line(_) if best != Some(Kind::Line) => best = Some(Kind::Line),
            Symbol::Point(_) if best.is_none() => best = Some(Kind::Point),
            _ => {}
        }
    }
    best
}

fn expanded(rect: Rect, by: f64) -> Rect {
    rect.adjusted(-by, -by, by, by)
}

/// Every object whose symbol the caller gave a speed for.
fn collect(map: &Map, options: &Options) -> Vec<Shape> {
    // An overlap entry is applied later, over the shapes of its components.
    let singles: Vec<&(String, f64)> = options
        .speeds
        .iter()
        .filter(|(code, _)| !code.contains('+'))
        .collect();

    let mut shapes = Vec::new();
    for (object_index, object) in map.objects.iter().enumerate() {
        let Some(symbol) = object.symbol_index.and_then(|i| map.symbols.get(i)) else {
            continue;
        };
        let code = symbol.code().trim();
        if code.is_empty() {
            continue;
        }
        let Some((_, speed)) = singles.iter().find(|(c, _)| matches(code, c)) else {
            continue;
        };
        if !speed.is_finite() {
            continue;
        }
        let Some(kind) = kind_of(symbol, map) else {
            continue;
        };
        let Some(bounds) = coords_bounds(object) else {
            continue;
        };
        // A point symbol is drawn as a disc at the object's position, so it
        // needs no outline -- and would have none to build, being one
        // coordinate long.
        let path = if kind == Kind::Point {
            Path::new()
        } else {
            let path = to_painter_path(&object.coords, false);
            if path.is_empty() {
                continue;
            }
            path
        };
        let lookup = |list: &[(String, f64)]| {
            list.iter()
                .filter(|(c, _)| !c.contains('+'))
                .find(|(c, _)| matches(code, c))
                .map(|(_, v)| *v)
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(0.0)
        };
        let line_width = if kind == Kind::Line {
            lookup(&options.line_widths)
        } else {
            0.0
        };
        let shrink = if kind == Kind::Area {
            lookup(&options.shrink)
        } else {
            0.0
        };
        shapes.push(Shape {
            code: code.to_string(),
            speed: *speed,
            kind,
            line_width,
            shrink,
            display: if kind == Kind::Area {
                display_priority(symbol, map)
            } else {
                i32::MAX
            },
            path,
            origin: object_origin(object),
            object_index,
            // A line or a point may have no extent at all in one direction.
            bounds: if kind == Kind::Area {
                bounds
            } else {
                expanded(bounds, DEGENERATE_MARGIN)
            },
        });
    }
    shapes
}

/// The box around every coordinate of an object, control points included.
fn coords_bounds(object: &Object) -> Option<Rect> {
    let mut coords = object.coords.iter();
    let first = coords.next()?;
    let (mut left, mut top, mut right, mut bottom) = (first.x, first.y, first.x, first.y);
    for coord in coords {
        left = left.min(coord.x);
        top = top.min(coord.y);
        right = right.max(coord.x);
        bottom = bottom.max(coord.y);
    }
    let rect = Rect::from_ltrb(left, top, right, bottom);
    (left.is_finite() && top.is_finite() && right.is_finite() && bottom.is_finite()).then_some(rect)
}

fn object_origin(object: &Object) -> Point {
    object
        .coords
        .first()
        .map(|c| c.pos())
        .unwrap_or(Point::new(0.0, 0.0))
}

/// The colour an area is seen in, for ordering: the topmost (smallest)
/// priority among its fills and patterns, or `i32::MAX` for an area drawing in
/// no colour at all. A combined symbol counts its area parts.
fn display_priority(symbol: &Symbol, map: &Map) -> i32 {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    let mut top = i32::MAX;
    for leaf in leaves {
        let Symbol::Area(area) = leaf else {
            continue;
        };
        let mut consider = |priority: i32| {
            if is_color(priority) {
                top = top.min(priority);
            }
        };
        consider(area.color);
        for pattern in &area.patterns {
            if pattern.pattern_type == fill_pattern_type::LINE {
                consider(pattern.line_color);
            } else if let Some(point) = &pattern.point {
                if let Some(priority) = point_color(point) {
                    consider(priority);
                }
            }
        }
    }
    top
}

/// The colour a point symbol is chiefly drawn in: its inner colour, else its
/// outer one, else that of its first element with a colour.
fn point_color(point: &PointSymbol) -> Option<i32> {
    if is_color(point.inner_color) {
        return Some(point.inner_color);
    }
    if is_color(point.outer_color) {
        return Some(point.outer_color);
    }
    point
        .elements
        .iter()
        .find_map(|element| match &element.symbol {
            Symbol::Area(area) if is_color(area.color) => Some(area.color),
            Symbol::Line(line) if is_color(line.color) => Some(line.color),
            Symbol::Point(point) => point_color(point),
            _ => None,
        })
}

/// Sorts shapes into drawing order: areas by the map's display order, then
/// points, then lines, slowest last.
///
/// Which shape wins a cell is which is drawn last. Among areas that is the
/// one the map shows on top: the colour drawn later (a smaller priority) goes
/// later, and objects sharing a colour keep the file's order, as they do on
/// the map. A line is the thing a runner follows: a track across a marsh has
/// to survive the marsh. Among lines the slowest goes last, so a fence beats
/// the path it crosses.
fn drawing_order(shapes: &mut [Shape]) {
    let rank = |kind: Kind| match kind {
        Kind::Area => 0,
        Kind::Point => 1,
        Kind::Line => 2,
    };
    shapes.sort_by(|a, b| {
        rank(a.kind).cmp(&rank(b.kind)).then_with(|| match a.kind {
            // Descending priority: the topmost colour is drawn last and wins.
            Kind::Area => b.display.cmp(&a.display),
            // Descending speed: the slowest line is drawn last and wins.
            Kind::Line => b
                .speed
                .partial_cmp(&a.speed)
                .unwrap_or(std::cmp::Ordering::Equal),
            Kind::Point => std::cmp::Ordering::Equal,
        })
    });
}

/// Compares two symbol codes the way a reader expects: `40` before `403`
/// before `403.1`, with runs of digits compared as numbers.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.as_bytes(), b.as_bytes());
    loop {
        match (ai.first(), bi.first()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let a_end = ai
                        .iter()
                        .position(|c| !c.is_ascii_digit())
                        .unwrap_or(ai.len());
                    let b_end = bi
                        .iter()
                        .position(|c| !c.is_ascii_digit())
                        .unwrap_or(bi.len());
                    // Leading zeros do not make a number bigger.
                    let a_num = ai[..a_end].iter().skip_while(|&&c| c == b'0').count();
                    let b_num = bi[..b_end].iter().skip_while(|&&c| c == b'0').count();
                    let order = a_num
                        .cmp(&b_num)
                        .then_with(|| ai[..a_end].cmp(&bi[..b_end]));
                    if order != std::cmp::Ordering::Equal {
                        return order;
                    }
                    ai = &ai[a_end..];
                    bi = &bi[b_end..];
                } else {
                    let order = x.cmp(y);
                    if order != std::cmp::Ordering::Equal {
                        return order;
                    }
                    ai = &ai[1..];
                    bi = &bi[1..];
                }
            }
        }
    }
}

/// The convex hull of a set of points, counterclockwise, by Andrew's monotone
/// chain. Used to find where the map stops.
fn convex_hull(mut points: Vec<Point>) -> Vec<Point> {
    points.sort_by(|a, b| {
        a.x.partial_cmp(&b.x)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
    });
    points.dedup_by(|a, b| a.x == b.x && a.y == b.y);
    if points.len() <= 1 {
        return points;
    }
    let cross =
        |o: Point, a: Point, b: Point| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);

    let chain = |iter: &mut dyn Iterator<Item = Point>| -> Vec<Point> {
        let mut half: Vec<Point> = Vec::new();
        for p in iter {
            while half.len() >= 2 && cross(half[half.len() - 2], half[half.len() - 1], p) <= 0.0 {
                half.pop();
            }
            half.push(p);
        }
        half.pop();
        half
    };
    let mut lower = chain(&mut points.iter().copied());
    let upper = chain(&mut points.iter().rev().copied());
    lower.extend(upper);
    lower
}

/// The transform from mm on the paper to cells of the grid.
fn to_grid(bounds: Rect, pixel_size: f64) -> Transform {
    let s = (1.0 / pixel_size) as f32;
    Transform::from_row(
        s,
        0.0,
        0.0,
        s,
        (-bounds.left() / pixel_size) as f32,
        (-bounds.top() / pixel_size) as f32,
    )
}

/// Draws one shape onto a mask, in the way its kind calls for, and returns
/// the box of cells it can have touched (see [`Cells`]), or `None` if it drew
/// nothing.
fn draw(
    mask: &mut Mask,
    shape: &Shape,
    transform: Transform,
    pixel_size: f64,
    buffer_lines: bool,
) -> Option<Cells> {
    let (width, height) = (mask.width(), mask.height());
    let filled = |mask: &mut Mask, path: &tiny_skia::Path, rule: FillRule| {
        mask.fill_path(path, rule, true, transform);
        Cells::around(path.bounds(), transform, width, height)
    };
    match shape.kind {
        Kind::Area if shape.shrink > 0.0 => {
            let path = to_skia_path(&shape.path)?;
            draw_shrunk(mask, &path, transform, shape.shrink / pixel_size)
        }
        Kind::Area => {
            let path = to_skia_path(&shape.path)?;
            filled(mask, &path, FillRule::EvenOdd)
        }
        Kind::Line => {
            // A cell's diagonal, so that a line crossing the grid at an angle
            // still touches every cell along it.
            let default = if buffer_lines {
                pixel_size * std::f64::consts::SQRT_2
            } else {
                pixel_size
            };
            let width = default.max(shape.line_width);
            let stroke = Stroke {
                width: width as f32,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            let path = to_skia_path(&shape.path)?;
            // Stroked in mm and filled through the same transform, so the
            // width is the one asked for rather than one cell whatever the
            // scale. Its outline may cross itself at a sharp corner, which is
            // what the winding rule is for.
            let outline = path.stroke(&stroke, (1.0 / pixel_size) as f32)?;
            filled(mask, &outline, FillRule::Winding)
        }
        Kind::Point => {
            let radius = pixel_size / std::f64::consts::SQRT_2;
            let mut circle = Path::new();
            add_ellipse(&mut circle, shape.origin, radius, radius);
            let path = to_skia_path(&circle)?;
            filled(mask, &path, FillRule::Winding)
        }
    }
}

/// Fills an area pulled in by `inset` cells from its edge.
///
/// The area is filled on a mask of its own the size of its box, eroded there,
/// and what is left is added to `mask`, so whatever `mask` already holds is
/// untouched. A cell stays if its centre is more than `inset` from the
/// nearest cell the area does not cover, measured to that cell's edge.
fn draw_shrunk(
    mask: &mut Mask,
    path: &tiny_skia::Path,
    transform: Transform,
    inset: f64,
) -> Option<Cells> {
    let (width, height) = (mask.width(), mask.height());
    let cells = Cells::around(path.bounds(), transform, width, height)?;
    let (w, h) = (cells.x1 - cells.x0, cells.y1 - cells.y0);
    let mut local = Mask::new(w as u32, h as u32)?;
    local.fill_path(
        path,
        FillRule::EvenOdd,
        true,
        transform.post_translate(-(cells.x0 as f32), -(cells.y0 as f32)),
    );
    let inside: Vec<bool> = local.data().iter().map(|&c| c != 0).collect();
    let distance = squared_distance_to_outside(&inside, w, h);
    let keep = (inset + 0.5) * (inset + 0.5);
    let row_len = width as usize;
    let data = mask.data_mut();
    for y in 0..h {
        for x in 0..w {
            if distance[y * w + x] > keep {
                data[(cells.y0 + y) * row_len + cells.x0 + x] = 255;
            }
        }
    }
    Some(cells)
}

/// For every cell of a `w` x `h` grid, the squared distance from its centre
/// to the centre of the nearest cell which is not `inside`, everything past
/// the grid's edge counting as outside. Felzenszwalb and Huttenlocher's
/// exact transform, a column pass and then a row pass.
fn squared_distance_to_outside(inside: &[bool], w: usize, h: usize) -> Vec<f64> {
    // Outside the grid is one cell past its edge at most, so no distance is
    // larger than this; it stands in for "no outside cell along this line".
    let far = ((w + h + 2) * (w + h + 2)) as f64;
    let mut grid = vec![0.0; w * h];
    let mut column = vec![0.0; h + 2];
    for x in 0..w {
        // Padded with an outside cell at each end.
        column[0] = 0.0;
        column[h + 1] = 0.0;
        for y in 0..h {
            column[y + 1] = if inside[y * w + x] { far } else { 0.0 };
        }
        let d = distance_1d(&column);
        for y in 0..h {
            grid[y * w + x] = d[y + 1];
        }
    }
    let mut row = vec![0.0; w + 2];
    for y in 0..h {
        row[0] = 0.0;
        row[w + 1] = 0.0;
        row[1..=w].copy_from_slice(&grid[y * w..(y + 1) * w]);
        let d = distance_1d(&row);
        grid[y * w..(y + 1) * w].copy_from_slice(&d[1..=w]);
    }
    grid
}

/// The one-dimensional squared distance transform of `f`: for each `q`, the
/// least `(q - p)^2 + f[p]` over every `p`, by the lower envelope of parabolas.
fn distance_1d(f: &[f64]) -> Vec<f64> {
    let n = f.len();
    let mut d = vec![0.0; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f64; n + 1];
    let mut k = 0usize;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    let sq = |i: usize| (i * i) as f64;
    let meet = |q: usize, p: usize| ((f[q] + sq(q)) - (f[p] + sq(p))) / (2.0 * (q - p) as f64);
    for q in 1..n {
        // z[0] is minus infinity, so this stops at k == 0 at the latest.
        let mut s = meet(q, v[k]);
        while s <= z[k] {
            k -= 1;
            s = meet(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f64 - p as f64;
        *out = dq * dq + f[p];
    }
    d
}

/// A box of grid cells, `x0..x1` by `y0..y1`, inside the grid.
///
/// Each shape covers a small part of a large grid, so the main pass clears
/// and reads only the cells a shape can have touched rather than the whole
/// mask once per shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cells {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl Cells {
    /// The cells under `bounds` (in mm) once `transform` puts it on a
    /// `width` x `height` grid, one cell wider all round for antialiasing
    /// and rounding, or `None` if that misses the grid.
    ///
    /// `transform` is [`to_grid`]'s, a scale and a translation, so the box
    /// of the transformed path is the transformed box.
    fn around(
        bounds: tiny_skia::Rect,
        transform: Transform,
        width: u32,
        height: u32,
    ) -> Option<Cells> {
        let x = |v: f32| f64::from(v) * f64::from(transform.sx) + f64::from(transform.tx);
        let y = |v: f32| f64::from(v) * f64::from(transform.sy) + f64::from(transform.ty);
        let (ax, bx) = (x(bounds.left()), x(bounds.right()));
        let (ay, by) = (y(bounds.top()), y(bounds.bottom()));
        let clamp = |v: f64, max: u32| v.clamp(0.0, f64::from(max)) as usize;
        let cells = Cells {
            x0: clamp(ax.min(bx).floor() - 1.0, width),
            y0: clamp(ay.min(by).floor() - 1.0, height),
            x1: clamp(ax.max(bx).ceil() + 1.0, width),
            y1: clamp(ay.max(by).ceil() + 1.0, height),
        };
        (cells.x0 < cells.x1 && cells.y0 < cells.y1).then_some(cells)
    }
}

/// Rasterizes a map into a grid of running speeds.
///
/// Fails only where nothing can be rasterized: no object of the map uses a
/// symbol the caller gave a speed for, or the grid cannot be allocated.
pub fn build(map: &Map, options: &Options) -> Result<Raster, String> {
    let pixel_size = options.pixel_size.max(f64::MIN_POSITIVE);
    let mut shapes = collect(map, options);
    if shapes.is_empty() {
        return Err("No map objects use a symbol with a configured speed.".to_string());
    }
    drawing_order(&mut shapes);

    // A line is drawn wider than a cell, so the grid has to reach further
    // than the geometry does.
    let margin = |shape: &Shape| {
        let buffer = if options.buffer_lines && shape.kind != Kind::Area {
            pixel_size
        } else {
            0.0
        };
        buffer.max(shape.line_width / 2.0)
    };
    let mut bounds = expanded(shapes[0].bounds, margin(&shapes[0]));
    for shape in &shapes[1..] {
        bounds = bounds.united(&expanded(shape.bounds, margin(shape)));
    }

    let mut log = Vec::new();
    let cells_for = |size: f64| {
        (
            ((bounds.width() / size).ceil() as i64).max(1) as u32,
            ((bounds.height() / size).ceil() as i64).max(1) as u32,
        )
    };
    let (mut width, mut height) = cells_for(pixel_size);
    let mut effective_pixel_size = pixel_size;
    let cells = width as usize * height as usize;
    if cells > options.max_cells {
        // Keeping the cell count fixed rather than the cell size: a map twice
        // as large is rasterized twice as coarsely, and stays usable.
        let factor = (cells as f64 / options.max_cells as f64).sqrt();
        effective_pixel_size = pixel_size * factor;
        // That factor is exact on the area, but each side is rounded up to a
        // whole cell, so the result can still be a little over. Grow it until
        // it really fits.
        loop {
            let (w, h) = cells_for(effective_pixel_size);
            width = w;
            height = h;
            if (w as usize) * (h as usize) <= options.max_cells {
                break;
            }
            effective_pixel_size *= 1.01;
        }
        log.push("Adjusted pixel size to keep the raster responsive.".to_string());
    }

    log.push(format!(
        "Collected {} runnability geometries.",
        shapes.len()
    ));
    log.push(format!("Rasterizing {width} x {height} cells."));

    let cell_count = width as usize * height as usize;
    let mut values = vec![options.fill_value as f32; cell_count];
    let mut code_index = vec![-1i32; cell_count];
    let mut codes: Vec<String> = Vec::new();
    let mut code_to_index: HashMap<String, usize> = HashMap::new();
    let mut used: Vec<CodeUse> = Vec::new();
    let mut used_index: HashMap<String, usize> = HashMap::new();

    let transform = to_grid(bounds, effective_pixel_size);
    let mut mask = Mask::new(width, height)
        .ok_or_else(|| format!("Failed to allocate a {width} x {height} raster."))?;

    // The mask starts clear, and each shape clears what it drew as it is
    // read back, so it is clear again for the next one.
    let row_len = width as usize;
    for shape in &shapes {
        let cells = draw(
            &mut mask,
            shape,
            transform,
            effective_pixel_size,
            options.buffer_lines,
        );
        let idx = *code_to_index.entry(shape.code.clone()).or_insert_with(|| {
            codes.push(shape.code.clone());
            codes.len() - 1
        });
        let at = *used_index.entry(shape.code.clone()).or_insert_with(|| {
            used.push(CodeUse {
                code: shape.code.clone(),
                speed: shape.speed,
                cells: 0,
            });
            used.len() - 1
        });
        let speed = shape.speed as f32;
        let mut claimed = 0u64;
        if let Some(cells) = cells {
            let data = mask.data_mut();
            for row in cells.y0..cells.y1 {
                let start = row * row_len;
                for i in start + cells.x0..start + cells.x1 {
                    if data[i] == 0 {
                        continue;
                    }
                    data[i] = 0;
                    values[i] = speed;
                    code_index[i] = idx as i32;
                    claimed += 1;
                }
            }
        }
        used[at].cells += claimed;
    }

    apply_overlaps(
        options,
        &shapes,
        &mut Grid {
            width,
            height,
            pixel_size: effective_pixel_size,
            bounds,
            transform,
        },
        &mut values,
        &mut code_index,
        &mut codes,
        &mut code_to_index,
        &mut used,
        &mut used_index,
    )?;

    if options.mask_outside_convex_hull {
        mask_outside_hull(map, &shapes, width, height, transform, &mut values);
    }

    used.retain(|u| u.cells > 0);
    used.sort_by(|a, b| natural_cmp(&a.code, &b.code));

    Ok(Raster {
        width,
        height,
        pixel_size: effective_pixel_size,
        bounds,
        values,
        code_index,
        codes,
        used_codes: used,
        log,
    })
}

/// Everything the overlap pass needs to know about the grid it is drawing on.
struct Grid {
    width: u32,
    height: u32,
    pixel_size: f64,
    #[allow(dead_code)]
    bounds: Rect,
    transform: Transform,
}

/// Applies the speeds given for a combination of codes.
///
/// Each component's shapes are drawn into a mask of their own, and a cell
/// every one of them covers takes the combination's speed. A combination
/// naming a code no object on the map uses is skipped rather than treated as
/// covering nothing.
#[allow(clippy::too_many_arguments)]
fn apply_overlaps(
    options: &Options,
    shapes: &[Shape],
    grid: &mut Grid,
    values: &mut [f32],
    code_index: &mut [i32],
    codes: &mut Vec<String>,
    code_to_index: &mut HashMap<String, usize>,
    used: &mut Vec<CodeUse>,
    used_index: &mut HashMap<String, usize>,
) -> Result<(), String> {
    let overlaps: Vec<&(String, f64)> = options
        .speeds
        .iter()
        .filter(|(code, _)| code.contains('+'))
        .collect();
    if overlaps.is_empty() {
        return Ok(());
    }

    // Shapes by the base of their code, which is what a combination names.
    let mut by_base: HashMap<&str, Vec<&Shape>> = HashMap::new();
    for shape in shapes {
        by_base
            .entry(base_code(&shape.code))
            .or_default()
            .push(shape);
    }

    for (combo, speed) in overlaps {
        let components: Vec<&str> = combo
            .split('+')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect();
        let component_shapes: Vec<&Vec<&Shape>> = match components
            .iter()
            .map(|c| by_base.get(base_code(c)))
            .collect::<Option<Vec<_>>>()
        {
            Some(lists) => lists,
            // A component nothing on the map uses: the overlap cannot occur.
            None => continue,
        };

        let mut masks = Vec::with_capacity(component_shapes.len());
        for list in &component_shapes {
            let mut mask = Mask::new(grid.width, grid.height)
                .ok_or_else(|| "Failed to allocate an overlap raster.".to_string())?;
            for shape in list.iter() {
                draw(
                    &mut mask,
                    shape,
                    grid.transform,
                    grid.pixel_size,
                    options.buffer_lines,
                );
            }
            masks.push(mask);
        }

        let idx = *code_to_index.entry(combo.clone()).or_insert_with(|| {
            codes.push(combo.clone());
            codes.len() - 1
        });
        let at = *used_index.entry(combo.clone()).or_insert_with(|| {
            used.push(CodeUse {
                code: combo.clone(),
                speed: *speed,
                cells: 0,
            });
            used.len() - 1
        });

        let mut claimed = 0u64;
        for i in 0..values.len() {
            if masks.iter().all(|m| m.data()[i] != 0) {
                values[i] = *speed as f32;
                code_index[i] = idx as i32;
                claimed += 1;
            }
        }
        used[at].cells += claimed;
    }
    Ok(())
}

/// Marks everything outside the map as unknown.
///
/// A map is a shape, and the grid around it is a rectangle; without this the
/// corners would read as ordinary background and a route would happily leave
/// the map to get somewhere faster.
fn mask_outside_hull(
    map: &Map,
    shapes: &[Shape],
    width: u32,
    height: u32,
    transform: Transform,
    values: &mut [f32],
) {
    let mut points = Vec::new();
    for shape in shapes {
        if let Some(object) = map.objects.get(shape.object_index) {
            points.extend(object.coords.iter().map(|c| c.pos()));
        }
    }
    let hull = convex_hull(points);
    if hull.len() < 3 {
        return;
    }
    let mut path = Path::new();
    path.move_to(hull[0]);
    for p in &hull[1..] {
        path.line_to(*p);
    }
    path.close_subpath();
    let Some(skia) = to_skia_path(&path) else {
        return;
    };
    let Some(mut mask) = Mask::new(width, height) else {
        return;
    };
    mask.fill_path(&skia, FillRule::Winding, true, transform);
    for (i, &coverage) in mask.data().iter().enumerate() {
        if coverage == 0 {
            values[i] = f32::NAN;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    fn shape(kind: Kind, points: &[(f64, f64)]) -> Shape {
        let mut path = Path::new();
        if kind != Kind::Point {
            path.move_to(p(points[0].0, points[0].1));
            for &(x, y) in &points[1..] {
                path.line_to(p(x, y));
            }
            if kind == Kind::Area {
                path.close_subpath();
            }
        }
        Shape {
            code: "1".to_string(),
            speed: 1.0,
            kind,
            path,
            origin: p(points[0].0, points[0].1),
            object_index: 0,
            bounds: Rect::from_ltrb(0.0, 0.0, 0.0, 0.0),
            line_width: 0.0,
            shrink: 0.0,
            display: i32::MAX,
        }
    }

    /// The main pass reads, and clears, only the box `draw` reports: a cell
    /// drawn outside it would be missed for this shape and then handed to
    /// whichever later shape's box covers it. So every cell `draw` touches
    /// has to be inside, for every kind of shape, at any cell size, whether
    /// on the grid, half off it or across its corner.
    #[test]
    fn every_cell_a_shape_draws_is_inside_the_box_it_reports() {
        let bounds = Rect::from_ltrb(-3.0, 2.0, 17.0, 14.0);
        let shapes = [
            shape(Kind::Area, &[(1.0, 3.0), (9.3, 4.1), (6.2, 11.7)]),
            shape(
                Kind::Area,
                &[(-8.0, 0.0), (2.0, 0.0), (2.0, 5.0), (-8.0, 5.0)],
            ),
            shape(Kind::Line, &[(0.3, 13.9), (16.9, 2.2)]),
            shape(Kind::Line, &[(5.0, 5.0), (5.0, 5.0001)]),
            shape(Kind::Line, &[(-4.0, 8.0), (20.0, 8.0), (20.0, 15.0)]),
            shape(Kind::Point, &[(4.4, 7.7)]),
            shape(Kind::Point, &[(-3.0, 2.0)]),
            shape(Kind::Point, &[(17.0, 14.0)]),
        ];
        for pixel_size in [0.13, 0.5, 1.0, 3.7] {
            let width = (bounds.width() / pixel_size).ceil() as u32;
            let height = (bounds.height() / pixel_size).ceil() as u32;
            let transform = to_grid(bounds, pixel_size);
            for (n, shape) in shapes.iter().enumerate() {
                for buffer_lines in [false, true] {
                    let mut mask = Mask::new(width, height).unwrap();
                    let cells = draw(&mut mask, shape, transform, pixel_size, buffer_lines);
                    let drawn: Vec<(usize, usize)> = mask
                        .data()
                        .iter()
                        .enumerate()
                        .filter(|(_, &c)| c != 0)
                        .map(|(i, _)| (i % width as usize, i / width as usize))
                        .collect();
                    assert!(!drawn.is_empty(), "shape {n} at {pixel_size} drew nothing");
                    let cells = cells.unwrap();
                    for (x, y) in drawn {
                        assert!(
                            (cells.x0..cells.x1).contains(&x) && (cells.y0..cells.y1).contains(&y),
                            "shape {n} at {pixel_size}: ({x}, {y}) is outside {cells:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_shape_off_the_grid_reports_no_box() {
        let bounds = Rect::from_ltrb(0.0, 0.0, 10.0, 10.0);
        let mut mask = Mask::new(10, 10).unwrap();
        let far = shape(Kind::Area, &[(50.0, 50.0), (60.0, 50.0), (60.0, 60.0)]);
        assert_eq!(
            draw(&mut mask, &far, to_grid(bounds, 1.0), 1.0, false),
            None
        );
        assert!(mask.data().iter().all(|&c| c == 0));
    }

    #[test]
    fn the_distance_transform_matches_brute_force() {
        let (w, h) = (9, 7);
        let inside: Vec<bool> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                !(x == 4 && y == 3) && (1..8).contains(&x) && (1..6).contains(&y)
                    || x == 0 && y == 6
            })
            .collect();
        let got = squared_distance_to_outside(&inside, w, h);
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let mut best = f64::INFINITY;
                for oy in -1..=h as i64 {
                    for ox in -1..=w as i64 {
                        let in_grid = (0..w as i64).contains(&ox) && (0..h as i64).contains(&oy);
                        if in_grid && inside[(oy as usize) * w + ox as usize] {
                            continue;
                        }
                        best = best.min(((x - ox).pow(2) + (y - oy).pow(2)) as f64);
                    }
                }
                assert_eq!(got[y as usize * w + x as usize], best, "({x}, {y})");
            }
        }
    }

    fn covered(shape: &Shape, pixel_size: f64, buffer_lines: bool) -> usize {
        let bounds = Rect::from_ltrb(0.0, 0.0, 20.0, 20.0);
        let n = (20.0 / pixel_size).ceil() as u32;
        let mut mask = Mask::new(n, n).unwrap();
        draw(
            &mut mask,
            shape,
            to_grid(bounds, pixel_size),
            pixel_size,
            buffer_lines,
        );
        mask.data().iter().filter(|&&c| c != 0).count()
    }

    #[test]
    fn a_shrunk_area_loses_its_edge_and_keeps_its_middle() {
        let square = &[(5.0, 5.0), (15.0, 5.0), (15.0, 15.0), (5.0, 15.0)];
        let mut area = shape(Kind::Area, square);
        assert_eq!(covered(&area, 1.0, false), 100);
        area.shrink = 2.0;
        // Two cells off each side of a ten-cell square.
        assert_eq!(covered(&area, 1.0, false), 36);
        area.shrink = 6.0;
        assert_eq!(covered(&area, 1.0, false), 0);

        // A shrunk area leaves what was already on the mask alone.
        let bounds = Rect::from_ltrb(0.0, 0.0, 20.0, 20.0);
        let mut mask = Mask::new(20, 20).unwrap();
        mask.data_mut()[5 * 20 + 5] = 255;
        area.shrink = 2.0;
        draw(&mut mask, &area, to_grid(bounds, 1.0), 1.0, false);
        assert_eq!(mask.data()[5 * 20 + 5], 255);
        assert_eq!(mask.data().iter().filter(|&&c| c != 0).count(), 37);
    }

    #[test]
    fn a_wide_line_covers_more_and_never_less() {
        let mut line = shape(Kind::Line, &[(2.0, 10.0), (18.0, 10.0)]);
        let narrow = covered(&line, 0.5, true);
        line.line_width = 3.0;
        let wide = covered(&line, 0.5, true);
        // Six cells across instead of about three.
        assert!(wide >= narrow * 2, "{narrow} -> {wide}");
        line.line_width = 0.01;
        assert_eq!(covered(&line, 0.5, true), narrow);
    }
}
