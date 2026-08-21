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
//! the grid, and every cell it touches takes that speed. Objects are drawn in
//! the order they are collected, with lines last and the slowest line last of
//! all, so that a path over a marsh reads as a path and the marsh does not
//! win by being drawn later.
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

use std::collections::HashMap;

use tiny_skia::{FillRule, LineCap, LineJoin, Mask, Stroke, Transform};

use crate::geometry::{add_ellipse, to_painter_path, Path, Rect};
use crate::map::{Map, Object, PartRef, Point, Symbol};
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

/// The symbols a symbol actually draws with: itself, unless it is a
/// combination, in which case its parts, recursively.
fn leaves<'m>(symbol: &'m Symbol, map: &'m Map, out: &mut Vec<&'m Symbol>) {
    let Symbol::Combined(combined) = symbol else {
        out.push(symbol);
        return;
    };
    for part in &combined.parts {
        match *part {
            PartRef::Shared(i) => {
                if let Some(part) = map.symbols.get(i) {
                    leaves(part, map, out);
                }
            }
            PartRef::Private(i) => {
                if let Some(part) = combined.owned_parts.get(i) {
                    leaves(part, map, out);
                }
            }
            PartRef::None => {}
        }
    }
}

/// How an object's shape is drawn, from the kinds of symbol it draws with.
///
/// An area wins over a line and a line over a point, since a symbol which
/// fills also outlines, and what the grid wants is the ground it covers.
fn kind_of(symbol: &Symbol, map: &Map) -> Option<Kind> {
    let mut resolved = Vec::new();
    leaves(symbol, map, &mut resolved);
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
        shapes.push(Shape {
            code: code.to_string(),
            speed: *speed,
            kind,
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

/// Sorts lines to the end, slowest last.
///
/// Which shape wins a cell is which is drawn last, and a line is the thing a
/// runner follows: a track across a marsh has to survive the marsh. Among
/// lines the slowest goes last, so a fence beats the path it crosses.
fn lines_last(shapes: &mut [Shape]) {
    shapes.sort_by(|a, b| {
        let (a_line, b_line) = (a.kind == Kind::Line, b.kind == Kind::Line);
        match (a_line, b_line) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (true, true) => a
                .speed
                .partial_cmp(&b.speed)
                .unwrap_or(std::cmp::Ordering::Equal),
            (false, false) => std::cmp::Ordering::Equal,
        }
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

/// Draws one shape onto a mask, in the way its kind calls for.
fn draw(mask: &mut Mask, shape: &Shape, transform: Transform, pixel_size: f64, buffer_lines: bool) {
    match shape.kind {
        Kind::Area => {
            if let Some(path) = to_skia_path(&shape.path) {
                mask.fill_path(&path, FillRule::EvenOdd, true, transform);
            }
        }
        Kind::Line => {
            // A cell's diagonal, so that a line crossing the grid at an angle
            // still touches every cell along it.
            let width = if buffer_lines {
                pixel_size * std::f64::consts::SQRT_2
            } else {
                pixel_size
            };
            let stroke = Stroke {
                width: width as f32,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            let Some(path) = to_skia_path(&shape.path) else {
                return;
            };
            // Stroked in mm and filled through the same transform, so the
            // width is the one asked for rather than one cell whatever the
            // scale. Its outline may cross itself at a sharp corner, which is
            // what the winding rule is for.
            if let Some(outline) = path.stroke(&stroke, (1.0 / pixel_size) as f32) {
                mask.fill_path(&outline, FillRule::Winding, true, transform);
            }
        }
        Kind::Point => {
            let radius = pixel_size / std::f64::consts::SQRT_2;
            let mut circle = Path::new();
            add_ellipse(&mut circle, shape.origin, radius, radius);
            if let Some(path) = to_skia_path(&circle) {
                mask.fill_path(&path, FillRule::Winding, true, transform);
            }
        }
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
    lines_last(&mut shapes);

    // A line is drawn wider than a cell, so the grid has to reach further
    // than the geometry does.
    let margin = |shape: &Shape| {
        if options.buffer_lines && shape.kind != Kind::Area {
            pixel_size
        } else {
            0.0
        }
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

    for shape in &shapes {
        mask.clear();
        draw(
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
        for (i, &coverage) in mask.data().iter().enumerate() {
            if coverage == 0 {
                continue;
            }
            values[i] = speed;
            code_index[i] = idx as i32;
            claimed += 1;
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
