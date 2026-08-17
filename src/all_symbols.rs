//! Makes one map per symbol of a map, each carrying a grid of test objects
//! drawn with that symbol alone.
//!
//! This is where a benchmark suite comes from when the source is a single
//! map. A symbol set like ISOM or ISSprOM holds a few hundred symbols, and
//! rendering the whole set as one map says only whether the result looks
//! about right; rendering each symbol alone, on a grid that exercises it at
//! every size and rotation it has, says which symbol is wrong and in which of
//! its states. So the suite is one map per symbol, and a difference found in
//! a run points at a symbol rather than at a map.
//!
//! What each map contains is fixed rather than free. A suite is only
//! comparable with itself over time if the maps in it are the same maps, so
//! the shapes, the sizes, the rotations, the layout of the grid, the order
//! the objects come in and the names of the files are all decided here and
//! kept stable, down to the rounding.
//!
//! Each generated map holds
//!
//! * for a **line** personality: a straight line, a closed polygonal square,
//!   a closed bezier circle, an open bezier S and an open zigzag, each at
//!   three sizes, plus a pair of lines around the symbol's minimum length;
//! * for an **area** personality: a square and a circle at every rotation of
//!   the fill pattern, a square with a hole and a five-pointed star, each at
//!   three sizes, plus a pair of squares around the symbol's minimum area;
//! * for a **point** symbol: one object per rotation;
//! * for a **text** symbol: a sample text per rotation.
//!
//! A combined symbol gets the shapes of every personality it contains.
//!
//! The objects are arranged on a grid whose columns are as wide as their
//! widest object and whose rows are as tall as their tallest, which is what
//! makes the extent of an object — what it *draws*, not what its coordinates
//! say — part of the layout. So this module renders, in the sense that it
//! builds every object's renderables to measure them, and the maps it writes
//! are laid out by the same code which draws them.
//!
//! What it writes next to each map is a description of that grid, so that a
//! difference found in a benchmark run can be read as "the circle at 50 m"
//! rather than as "something in row three" — see [`Sheet`].

use std::f64::consts::PI;
use std::path::Path;

use crate::geometry::qround;
use crate::map::*;
use crate::renderer::Renderer;
use crate::xml_reader::{self, Fragment, Fragments};
use crate::xml_writer::MapFile;

/// The nominal object sizes, in meters on the ground.
const NOMINAL_SIZE: [f64; 3] = [5.0, 50.0, 100.0];

/// The number of rotation steps covering the full circle.
const NUM_ROTATIONS: usize = 8;

/// The sample text used for objects with a text symbol.
const SAMPLE_TEXT: &str = "Sample 123";

/// The gap between grid cells, relative to the largest cell of a row or column.
const RELATIVE_GAP: f64 = 0.15;

/// The minimum gap between grid cells, in mm on the paper.
const MINIMUM_GAP: f64 = 2.0;

/// The control point distance for approximating a quarter circle by a cubic
/// bezier curve.
const BEZIER_CIRCLE_CONSTANT: f64 = 0.5522847498307936;

/// How deep a combined symbol's parts are followed before giving up. A symbol
/// which is its own part is not a thing Mapper writes, but a file is a file.
const MAX_PART_DEPTH: usize = 16;

/// The rotation of the i-th step, in radians.
fn rotation_step(i: usize) -> f64 {
    i as f64 * 2.0 * PI / NUM_ROTATIONS as f64
}

/// The rotation of the i-th step, in degrees, for labels.
fn rotation_degrees(i: usize) -> i32 {
    (i * 360 / NUM_ROTATIONS) as i32
}

/// Builds the coordinate vector of a path object.
///
/// Input positions are given in meters on the ground, relative to the center
/// of the shape. They are converted to native map coordinates using the map
/// scale — and rounded to them, since a `MapCoord` is an integer number of
/// 1/1000 mm and everything downstream, the extents included, sees the
/// rounded shape rather than the exact one.
struct PathBuilder {
    mm_per_meter: f64,
    coords: CoordList,
    part_start: usize,
}

impl PathBuilder {
    fn new(mm_per_meter: f64) -> PathBuilder {
        PathBuilder {
            mm_per_meter,
            coords: Vec::new(),
            part_start: 0,
        }
    }

    fn to_coord(&self, point: Point) -> Coord {
        // MapCoord(qreal, qreal) rounds millimeters to native units, by the
        // rule qRound uses rather than by Rust's, see [`qround`].
        let x = qround(point.x * self.mm_per_meter * 1000.0) / 1000.0;
        let y = qround(point.y * self.mm_per_meter * 1000.0) / 1000.0;
        Coord::new(x, y, 0)
    }

    /// Starts a new part. Any previous part becomes a hole of this object.
    fn move_to(&mut self, point: Point) {
        if let Some(last) = self.coords.last_mut() {
            last.flags |= coord_flag::HOLE_POINT;
        }
        self.part_start = self.coords.len();
        let coord = self.to_coord(point);
        self.coords.push(coord);
    }

    /// Appends a straight segment.
    fn line_to(&mut self, point: Point) {
        let coord = self.to_coord(point);
        self.coords.push(coord);
    }

    /// Appends a cubic bezier segment.
    fn curve_to(&mut self, control_1: Point, control_2: Point, point: Point) {
        if let Some(last) = self.coords.last_mut() {
            last.flags |= coord_flag::CURVE_START;
        }
        for p in [control_1, control_2, point] {
            let coord = self.to_coord(p);
            self.coords.push(coord);
        }
    }

    /// Closes the current part.
    ///
    /// The last coordinate of a closed part repeats the first one. If the
    /// part already ends at its start position, that coordinate is reused.
    fn close(&mut self) {
        let start = self.coords[self.part_start];
        let last = *self.coords.last().expect("a part has been started");
        // Compared as the file holds them, which is what
        // MapCoord::isPositionEqualTo compares.
        if last.x == start.x && last.y == start.y {
            self.coords.last_mut().unwrap().flags |= coord_flag::CLOSE_POINT;
        } else {
            let mut closing = start;
            closing.flags &= !coord_flag::CURVE_START;
            closing.flags |= coord_flag::CLOSE_POINT;
            self.coords.push(closing);
        }
    }
}

// ### Shapes ###
//
// All shapes are centered at the origin and fit into a box of the given size.
// Sizes are in meters on the ground.

/// A horizontal straight line.
fn add_straight_line(builder: &mut PathBuilder, size: f64) {
    builder.move_to(Point::new(-size / 2.0, 0.0));
    builder.line_to(Point::new(size / 2.0, 0.0));
}

/// A closed square made of straight segments.
fn add_square(builder: &mut PathBuilder, size: f64) {
    let h = size / 2.0;
    builder.move_to(Point::new(-h, -h));
    builder.line_to(Point::new(h, -h));
    builder.line_to(Point::new(h, h));
    builder.line_to(Point::new(-h, h));
    builder.close();
}

/// A closed circle made of four bezier segments.
fn add_circle(builder: &mut PathBuilder, size: f64) {
    let r = size / 2.0;
    let k = r * BEZIER_CIRCLE_CONSTANT;
    builder.move_to(Point::new(r, 0.0));
    builder.curve_to(Point::new(r, k), Point::new(k, r), Point::new(0.0, r));
    builder.curve_to(Point::new(-k, r), Point::new(-r, k), Point::new(-r, 0.0));
    builder.curve_to(Point::new(-r, -k), Point::new(-k, -r), Point::new(0.0, -r));
    builder.curve_to(Point::new(k, -r), Point::new(r, -k), Point::new(r, 0.0));
    builder.close();
}

/// An open S made of two bezier segments.
fn add_s_curve(builder: &mut PathBuilder, size: f64) {
    let h = size / 2.0;
    builder.move_to(Point::new(-h / 2.0, -h));
    builder.curve_to(Point::new(h, -h), Point::new(h, 0.0), Point::new(0.0, 0.0));
    builder.curve_to(
        Point::new(-h, 0.0),
        Point::new(-h, h),
        Point::new(h / 2.0, h),
    );
}

/// An open polyline with three sharp corners.
fn add_zigzag(builder: &mut PathBuilder, size: f64) {
    let h = size / 2.0;
    builder.move_to(Point::new(-h, h / 2.0));
    builder.line_to(Point::new(-h / 3.0, -h / 2.0));
    builder.line_to(Point::new(h / 3.0, h / 2.0));
    builder.line_to(Point::new(h, -h / 2.0));
}

/// A square with a circular hole of half the size.
fn add_square_with_hole(builder: &mut PathBuilder, size: f64) {
    add_square(builder, size);
    add_circle(builder, size / 2.0);
}

/// A closed five-pointed star.
fn add_star(builder: &mut PathBuilder, size: f64) {
    let outer = size / 2.0;
    let inner = size / 5.0;
    for i in 0..10 {
        let radius = if i % 2 == 0 { outer } else { inner };
        let angle = -PI / 2.0 + i as f64 * PI / 5.0;
        let point = Point::new(radius * angle.cos(), radius * angle.sin());
        if i == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }
    builder.close();
}

/// The signature shared by all shape functions.
type ShapeFunction = fn(&mut PathBuilder, f64);

/// A single object of the generated map, together with its place in the grid.
struct Cell {
    /// Which of [`Sheet::objects`] this is.
    object: usize,
    description: String,
    row: usize,
    column: usize,
}

/// The complete contents of one generated map file.
///
/// The objects are arranged on a grid. Columns hold the size variants, rows
/// hold the shape and rotation variants.
#[derive(Default)]
pub struct Sheet {
    objects: Vec<Object>,
    cells: Vec<Cell>,
    column_labels: Vec<String>,
    row_labels: Vec<String>,
    notes: Vec<String>,
}

impl Sheet {
    fn add_row(&mut self, label: impl Into<String>) -> usize {
        self.row_labels.push(label.into());
        self.row_labels.len() - 1
    }

    /// Adds an object and records its place in the grid.
    fn add(&mut self, object: Object, row: usize, column: usize, description: impl Into<String>) {
        self.cells.push(Cell {
            object: self.objects.len(),
            description: description.into(),
            row,
            column,
        });
        self.objects.push(object);
    }

    /// Whether anything could be generated for the symbol at all.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// What a symbol is, as the file format numbers it.
fn symbol_type(symbol: &Symbol) -> i32 {
    match symbol {
        Symbol::Point(_) => symbol_type::POINT,
        Symbol::Line(_) => symbol_type::LINE,
        Symbol::Area(_) => symbol_type::AREA,
        Symbol::Text(_) => symbol_type::TEXT,
        Symbol::Combined(_) => symbol_type::COMBINED,
    }
}

/// The human-readable name of a symbol type, as the file names use it.
fn type_name(symbol: &Symbol) -> &'static str {
    match symbol {
        Symbol::Point(_) => "point",
        Symbol::Line(_) => "line",
        Symbol::Area(_) => "area",
        Symbol::Text(_) => "text",
        Symbol::Combined(_) => "combined",
    }
}

fn symbol_name(symbol: &Symbol) -> &str {
    match symbol {
        Symbol::Point(s) => &s.name,
        Symbol::Line(s) => &s.name,
        Symbol::Area(s) => &s.name,
        Symbol::Text(s) => &s.name,
        Symbol::Combined(s) => &s.name,
    }
}

fn symbol_code(symbol: &Symbol) -> &str {
    match symbol {
        Symbol::Point(s) => &s.code,
        Symbol::Line(s) => &s.code,
        Symbol::Area(s) => &s.code,
        Symbol::Text(s) => &s.code,
        Symbol::Combined(s) => &s.code,
    }
}

fn is_rotatable(symbol: &Symbol) -> bool {
    match symbol {
        Symbol::Point(s) => s.is_rotatable,
        Symbol::Line(s) => s.is_rotatable,
        Symbol::Area(s) => s.is_rotatable,
        Symbol::Text(s) => s.is_rotatable,
        Symbol::Combined(s) => s.is_rotatable,
    }
}

/// The parts of a combined symbol, resolved to symbols.
fn parts<'m>(map: &'m Map, combined: &'m CombinedSymbol) -> Vec<&'m Symbol> {
    combined
        .parts
        .iter()
        .filter_map(|part| match part {
            PartRef::Shared(index) => map.symbols.get(*index),
            PartRef::Private(index) => combined.owned_parts.get(*index),
            PartRef::None => None,
        })
        .collect()
}

/// Every symbol personality this symbol draws with, as a bit set of
/// [`crate::map::symbol_type`] values. A combined symbol contains what its
/// parts contain.
fn contained_types(map: &Map, symbol: &Symbol, depth: usize) -> i32 {
    let mut types = symbol_type(symbol);
    if let Symbol::Combined(combined) = symbol {
        if depth < MAX_PART_DEPTH {
            for part in parts(map, combined) {
                types |= contained_types(map, part, depth + 1);
            }
        }
    }
    types
}

/// Whether the symbol fills an area with a pattern which can be turned.
fn has_rotatable_fill_pattern(map: &Map, symbol: &Symbol, depth: usize) -> bool {
    match symbol {
        Symbol::Area(area) => area.patterns.iter().any(|pattern| pattern.rotatable),
        Symbol::Combined(combined) if depth < MAX_PART_DEPTH => parts(map, combined)
            .iter()
            .any(|part| has_rotatable_fill_pattern(map, part, depth + 1)),
        _ => false,
    }
}

/// The symbol's minimum length, in mm on the paper. A combined symbol's is
/// the largest of its parts'.
fn minimum_length(map: &Map, symbol: &Symbol, depth: usize) -> f64 {
    match symbol {
        Symbol::Line(line) => line.minimum_length,
        Symbol::Combined(combined) if depth < MAX_PART_DEPTH => {
            parts(map, combined).iter().fold(0.0, |largest: f64, part| {
                largest.max(minimum_length(map, part, depth + 1))
            })
        }
        _ => 0.0,
    }
}

/// The symbol's minimum area, in the file's own unit (see
/// [`AreaSymbol::minimum_area`]). A combined symbol's is the largest of its
/// parts'.
fn minimum_area(map: &Map, symbol: &Symbol, depth: usize) -> i32 {
    match symbol {
        Symbol::Area(area) => area.minimum_area,
        Symbol::Combined(combined) if depth < MAX_PART_DEPTH => {
            parts(map, combined).iter().fold(0, |largest, part| {
                largest.max(minimum_area(map, part, depth + 1))
            })
        }
        _ => 0,
    }
}

/// The symbol number, as `Symbol::getNumberAsString()` builds it from the
/// `code` the file holds: up to three components, dot separated, stopping at
/// the first one the file does not have.
///
/// Not simply the code: the code is text and the number is numbers, so a code
/// of `07.1` is the number `7.1`, and one which is not a number at all is 0.
fn number_as_string(code: &str) -> String {
    const COMPONENTS: usize = 3;
    let mut number = [-1i32; COMPONENTS];
    if !code.is_empty() {
        let mut position = 0usize;
        for slot in number.iter_mut() {
            let dot = code[position + 1..].find('.').map(|at| at + position + 1);
            let component = match dot {
                Some(at) => &code[position..at],
                None => &code[position..],
            };
            // QString::toInt() on something which is not a number gives 0.
            *slot = leading_int(component);
            match dot {
                Some(at) => position = at + 1,
                None => break,
            }
        }
    }

    let mut text = String::new();
    for component in number {
        if component < 0 {
            break;
        }
        text.push_str(&component.to_string());
        text.push('.');
    }
    text.pop();
    text
}

/// `QString::toInt()`: the whole string as a number, or 0.
fn leading_int(text: &str) -> i32 {
    text.trim().parse().unwrap_or(0)
}

/// The symbol name with any markup taken out of it, as
/// `Symbol::getPlainTextName()` gives it.
///
/// Mapper only strips markup from a name which holds a `<`, and so does this.
fn plain_text_name(name: &str) -> String {
    if !name.contains('<') {
        return name.to_string();
    }
    let mut plain = String::with_capacity(name.len());
    let mut in_tag = false;
    for c in name.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    plain
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// A file name component which is safe on all supported platforms.
fn sanitized(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A path object of the given shape and size, drawn with `symbol`.
fn make_path(
    symbol_index: usize,
    symbol_id: i32,
    mm_per_meter: f64,
    shape: ShapeFunction,
    size: f64,
) -> Object {
    let mut builder = PathBuilder::new(mm_per_meter);
    shape(&mut builder, size);
    let mut object = Object::new(ObjectKind::Path(PathObject::default()));
    object.coords = builder.coords;
    object.symbol_index = Some(symbol_index);
    object.symbol_id = symbol_id;
    object
}

/// A number as `QString::number(double)` writes it into a label.
fn label_number(value: f64) -> String {
    crate::xml_writer::qt_number(value, 6)
}

/// A number as `QString::number(double, 'g', 3)` writes it, which is what the
/// minimum length and minimum area labels use.
fn label_number_3(value: f64) -> String {
    crate::xml_writer::qt_number(value, 3)
}

/// The rows which probe the symbol's minimum length.
///
/// Mapper stores the minimum length as an advisory property: it is reported
/// by the measure widget, but it does not suppress the object when rendering.
fn add_minimum_length_rows(sheet: &mut Sheet, symbol: &SymbolRef, mm_per_meter: f64) {
    let minimum_length_mm = symbol.minimum_length;
    if minimum_length_mm <= 0.0 {
        return;
    }

    let minimum_length_m = minimum_length_mm / mm_per_meter;
    let row = sheet.add_row(format!(
        "minimum length ({} m)",
        label_number_3(minimum_length_m)
    ));
    for (column, factor) in [0.5, 1.5].into_iter().enumerate() {
        sheet.add(
            make_path(
                symbol.index,
                symbol.id,
                mm_per_meter,
                add_straight_line,
                factor * minimum_length_m,
            ),
            row,
            column,
            format!(
                "straight line, {}% of the minimum length",
                label_number(factor * 100.0)
            ),
        );
    }
    sheet.notes.push(
        "The minimum length is advisory in Mapper: undersized lines are reported by the measure \
         widget and by the map issues report, but they are still drawn."
            .to_string(),
    );
}

/// The rows which probe the symbol's minimum area.
///
/// Like the minimum length, the minimum area does not suppress rendering.
fn add_minimum_area_rows(sheet: &mut Sheet, symbol: &SymbolRef, mm_per_meter: f64) {
    let minimum_area_mm2 = 0.001 * symbol.minimum_area as f64;
    if minimum_area_mm2 <= 0.0 {
        return;
    }

    let minimum_area_m2 = minimum_area_mm2 / (mm_per_meter * mm_per_meter);
    let row = sheet.add_row(format!(
        "minimum area ({} m2)",
        label_number_3(minimum_area_m2)
    ));
    for (column, factor) in [0.5, 1.5].into_iter().enumerate() {
        // A square of the requested area.
        sheet.add(
            make_path(
                symbol.index,
                symbol.id,
                mm_per_meter,
                add_square,
                (factor * minimum_area_m2).sqrt(),
            ),
            row,
            column,
            format!(
                "square, {}% of the minimum area",
                label_number(factor * 100.0)
            ),
        );
    }
    sheet.notes.push(
        "The minimum area is advisory in Mapper: undersized areas are reported by the map issues \
         report, but they are still drawn."
            .to_string(),
    );
}

/// The line personality shapes. Rows are the shapes, columns are the sizes.
fn add_line_shapes(sheet: &mut Sheet, symbol: &SymbolRef, mm_per_meter: f64) {
    const SHAPES: [(ShapeFunction, &str); 5] = [
        (add_straight_line, "straight line"),
        (add_square, "square, closed, polygonal"),
        (add_circle, "circle, closed, bezier"),
        (add_s_curve, "S curve, open, bezier"),
        (add_zigzag, "zigzag, open, sharp corners"),
    ];

    for (shape, label) in SHAPES {
        let row = sheet.add_row(label);
        for (column, size) in NOMINAL_SIZE.into_iter().enumerate() {
            sheet.add(
                make_path(symbol.index, symbol.id, mm_per_meter, shape, size),
                row,
                column,
                format!("{label}, {} m", label_number(size)),
            );
        }
    }

    add_minimum_length_rows(sheet, symbol, mm_per_meter);
}

/// The area personality shapes.
///
/// Square and circle are repeated for each pattern rotation; the additional
/// shapes are generated with an unrotated pattern only. Columns are the
/// nominal sizes.
fn add_area_shapes(sheet: &mut Sheet, symbol: &SymbolRef, mm_per_meter: f64) {
    let rotatable = symbol.has_rotatable_fill_pattern;
    let rotations = if rotatable { NUM_ROTATIONS } else { 1 };

    const SHAPES: [(ShapeFunction, &str, bool); 4] = [
        (add_square, "square, polygonal", true),
        (add_circle, "circle, bezier", true),
        (add_square_with_hole, "square with a hole", false),
        (add_star, "five-pointed star", false),
    ];

    for (shape, base_label, rotate) in SHAPES {
        let steps = if rotate { rotations } else { 1 };
        for step in 0..steps {
            let label = if rotate && rotatable {
                format!("{base_label}, pattern {} deg", rotation_degrees(step))
            } else {
                base_label.to_string()
            };
            let row = sheet.add_row(label.clone());
            for (column, size) in NOMINAL_SIZE.into_iter().enumerate() {
                let mut object = make_path(symbol.index, symbol.id, mm_per_meter, shape, size);
                if rotate && rotatable {
                    // A path object has one rotation, which is the rotation
                    // of its fill pattern: Mapper's `setPatternRotation` sets
                    // the object's own, and the file carries it twice, once
                    // as the object's attribute and once on its `<pattern>`.
                    // Kept as two fields here, since the reader has two.
                    object.rotation = rotation_step(step);
                    if let ObjectKind::Path(path) = &mut object.kind {
                        path.pattern_rotation = rotation_step(step);
                    }
                }
                sheet.add(
                    object,
                    row,
                    column,
                    format!("{label}, {} m", label_number(size)),
                );
            }
        }
    }

    if !rotatable {
        sheet.notes.push(
            "This symbol has no rotatable fill pattern, so the pattern rotation variants were \
             omitted."
                .to_string(),
        );
    }

    add_minimum_area_rows(sheet, symbol, mm_per_meter);
}

/// The point objects.
///
/// Point symbols have a fixed size, so there is a single column. Rows are the
/// rotations, which are only generated for rotatable symbols.
fn add_point_objects(sheet: &mut Sheet, symbol: &SymbolRef) {
    let rotatable = symbol.is_rotatable;
    let rotations = if rotatable { NUM_ROTATIONS } else { 1 };

    sheet.column_labels.push("nominal size".to_string());
    for step in 0..rotations {
        let label = if rotatable {
            format!("rotated {} deg", rotation_degrees(step))
        } else {
            "unrotated".to_string()
        };
        let row = sheet.add_row(label.clone());
        let mut object = Object::new(ObjectKind::Point);
        object.coords = vec![Coord::new(0.0, 0.0, 0)];
        object.symbol_index = Some(symbol.index);
        object.symbol_id = symbol.id;
        if rotatable {
            object.rotation = rotation_step(step);
        }
        sheet.add(object, row, 0, label);
    }

    if !rotatable {
        sheet.notes.push(
            "This point symbol is not rotatable, so a single object was generated.".to_string(),
        );
    }
}

/// The text objects. Text objects can always be rotated, independently of
/// the symbol.
fn add_text_objects(sheet: &mut Sheet, symbol: &SymbolRef) {
    sheet.column_labels.push("nominal size".to_string());
    for step in 0..NUM_ROTATIONS {
        let label = format!("rotated {} deg", rotation_degrees(step));
        let row = sheet.add_row(label.clone());
        let mut object = Object::new(ObjectKind::Text(TextObject {
            text: SAMPLE_TEXT.to_string(),
            // What TextObject's own constructor sets, rather than what a file
            // without the attributes would mean.
            h_align: h_align::HCENTER,
            v_align: v_align::VCENTER,
            box_size: None,
        }));
        object.coords = vec![Coord::new(0.0, 0.0, 0)];
        object.symbol_index = Some(symbol.index);
        object.symbol_id = symbol.id;
        object.rotation = rotation_step(step);
        sheet.add(object, row, 0, label);
    }
}

/// What the sheet builders need to know about the symbol they are building
/// for, worked out once so that a combined symbol's parts are walked once.
struct SymbolRef {
    /// Where the symbol is in the source map.
    index: usize,
    /// What the objects refer to it by in the file they are written to.
    id: i32,
    is_rotatable: bool,
    contained_types: i32,
    has_rotatable_fill_pattern: bool,
    /// In mm on the paper.
    minimum_length: f64,
    /// In the file's own unit, see [`AreaSymbol::minimum_area`].
    minimum_area: i32,
}

/// Builds the sheet for one symbol.
///
/// Returns a sheet without cells if the symbol has no personality this can
/// generate objects for.
fn make_sheet(map: &Map, symbol: &SymbolRef, mm_per_meter: f64) -> Sheet {
    let mut sheet = Sheet::default();
    let kind = &map.symbols[symbol.index];
    let types = symbol.contained_types;

    match kind {
        Symbol::Point(_) => add_point_objects(&mut sheet, symbol),
        Symbol::Text(_) => add_text_objects(&mut sheet, symbol),
        Symbol::Line(_) | Symbol::Area(_) | Symbol::Combined(_) => {
            // A combined symbol may have a line personality, an area
            // personality, or both. Every contained personality gets its own
            // set of shapes.
            for size in NOMINAL_SIZE {
                sheet
                    .column_labels
                    .push(format!("{} m", label_number(size)));
            }
            if types & symbol_type::LINE != 0 {
                add_line_shapes(&mut sheet, symbol, mm_per_meter);
            }
            if types & symbol_type::AREA != 0 {
                add_area_shapes(&mut sheet, symbol, mm_per_meter);
            }
            if types & (symbol_type::LINE | symbol_type::AREA) == 0 {
                // A combined symbol whose parts are all undefined.
                add_line_shapes(&mut sheet, symbol, mm_per_meter);
            }
        }
    }

    sheet
}

/// Arranges the objects of the sheet on a grid.
///
/// Column widths and row heights come from the objects' extents — what they
/// draw, line width and symbol elements included — so the grid is laid out
/// around the symbol rather than around its coordinates.
fn lay_out(map: &Map, sheet: &mut Sheet) {
    if sheet.cells.is_empty() {
        return;
    }

    let mut num_columns = 0;
    let mut num_rows = 0;
    for cell in &sheet.cells {
        num_columns = num_columns.max(cell.column + 1);
        num_rows = num_rows.max(cell.row + 1);
    }

    let extents: Vec<crate::geometry::Rect> = sheet
        .objects
        .iter()
        .map(|object| Renderer::object_extent(map, object))
        .collect();

    let mut column_width = vec![0.0f64; num_columns];
    let mut row_height = vec![0.0f64; num_rows];
    for cell in &sheet.cells {
        let extent = &extents[cell.object];
        column_width[cell.column] = column_width[cell.column].max(extent.width());
        row_height[cell.row] = row_height[cell.row].max(extent.height());
    }

    let widest = column_width.iter().copied().fold(f64::MIN, f64::max);
    let tallest = row_height.iter().copied().fold(f64::MIN, f64::max);
    let gap_x = MINIMUM_GAP.max(RELATIVE_GAP * widest);
    let gap_y = MINIMUM_GAP.max(RELATIVE_GAP * tallest);

    let mut column_left = vec![0.0f64; num_columns];
    for i in 1..num_columns {
        column_left[i] = column_left[i - 1] + column_width[i - 1] + gap_x;
    }
    let mut row_top = vec![0.0f64; num_rows];
    for i in 1..num_rows {
        row_top[i] = row_top[i - 1] + row_height[i - 1] + gap_y;
    }

    for cell in &sheet.cells {
        let extent = &extents[cell.object];
        let center_x = extent.left() + extent.width() / 2.0;
        let center_y = extent.top() + extent.height() / 2.0;
        let target_x = column_left[cell.column] + column_width[cell.column] / 2.0;
        let target_y = row_top[cell.row] + row_height[cell.row] / 2.0;
        // Object::move takes whole native units, so the shift is rounded once
        // for the whole object rather than once per coordinate.
        let dx = qround((target_x - center_x) * 1000.0) / 1000.0;
        let dy = qround((target_y - center_y) * 1000.0) / 1000.0;
        for coord in &mut sheet.objects[cell.object].coords {
            coord.x += dx;
            coord.y += dy;
        }
    }
}

impl Sheet {
    /// The companion file describing the grid.
    fn index(&self, symbol: &Symbol, source: &Path, scale_denominator: i32) -> String {
        let mut text = String::new();
        text.push_str(&format!(
            "Symbol:      {} {}\n",
            number_as_string(symbol_code(symbol)),
            plain_text_name(symbol_name(symbol))
        ));
        text.push_str(&format!("Type:        {}\n", type_name(symbol)));
        text.push_str(&format!("Source map:  {}\n", source.display()));
        text.push_str(&format!("Map scale:   1:{scale_denominator}\n"));
        text.push('\n');
        text.push_str(
            "The objects are arranged on a grid. The origin of the map is the top left\n",
        );
        text.push_str("corner of the first cell; rows run downwards, columns run to the right.\n");
        text.push_str("All sizes are given in meters on the ground.\n");
        text.push('\n');

        text.push_str("Columns (left to right):\n");
        for (i, label) in self.column_labels.iter().enumerate() {
            text.push_str(&format!("  c{}: {label}\n", i + 1));
        }
        text.push('\n');

        text.push_str("Rows (top to bottom):\n");
        for (i, label) in self.row_labels.iter().enumerate() {
            text.push_str(&format!("  r{}: {label}\n", i + 1));
        }
        text.push('\n');

        text.push_str("Cells:\n");
        for cell in &self.cells {
            text.push_str(&format!(
                "  r{}c{}: {}\n",
                cell.row + 1,
                cell.column + 1,
                cell.description
            ));
        }

        if !self.notes.is_empty() {
            text.push_str("\nNotes:\n");
            for note in &self.notes {
                text.push_str(&format!("  - {note}\n"));
            }
        }
        text
    }
}

/// What became of one symbol.
pub enum Outcome {
    /// The map file and the index file were written, under this base name.
    Written(String),
    /// No objects could be generated for this symbol.
    Skipped,
}

/// What a whole run came to.
pub struct Summary {
    /// How many maps were written, one per symbol.
    pub written: usize,
    /// The symbols nothing could be generated for, by number and name.
    pub skipped: Vec<String>,
    /// The scale of the source map, which every generated map inherits.
    pub scale_denominator: i32,
    /// Complaints from reading the source map.
    pub warnings: Vec<String>,
}

/// The symbols of `source`, each in a map of its own, written into `into`.
///
/// `into` is created if it is not there. Files of a previous run are
/// overwritten, but files belonging to symbols the source no longer has are
/// left where they are: this writes maps, and deciding that a file in a
/// folder it does not own has become stale is not its call to make.
pub fn create_maps(
    source: &Path,
    into: &Path,
    mut progress: impl FnMut(usize, usize),
) -> Result<Summary, String> {
    let (mut map, warnings) = xml_reader::read_xml_map(source)?;
    map.resolve_references();
    let fragments = xml_reader::read_fragments(source)?;

    if map.symbols.is_empty() {
        return Err(format!("{} does not contain any symbol", source.display()));
    }

    std::fs::create_dir_all(into).map_err(|e| format!("cannot make {}: {e}", into.display()))?;

    let mut summary = Summary {
        written: 0,
        skipped: Vec::new(),
        scale_denominator: map.scale_denominator,
        warnings,
    };
    let total = map.symbols.len();
    for index in 0..total {
        match write_symbol(&map, &fragments, index, source, into)? {
            Outcome::Written(_) => summary.written += 1,
            Outcome::Skipped => {
                let symbol = &map.symbols[index];
                summary.skipped.push(format!(
                    "{} {}",
                    number_as_string(symbol_code(symbol)),
                    plain_text_name(symbol_name(symbol))
                ));
            }
        }
        progress(index + 1, total);
    }
    Ok(summary)
}

/// The base name the files of one symbol get: its place in the source map,
/// its type, its number and its name.
pub fn base_name(map: &Map, index: usize) -> String {
    let symbol = &map.symbols[index];
    format!(
        "{:03}_{}_{}_{}",
        index + 1,
        type_name(symbol),
        sanitized(&number_as_string(symbol_code(symbol))),
        sanitized(&plain_text_name(symbol_name(symbol)))
    )
}

/// Generates the map file and the index file for a single symbol.
fn write_symbol(
    map: &Map,
    fragments: &Fragments,
    index: usize,
    source: &Path,
    into: &Path,
) -> Result<Outcome, String> {
    let symbol = &map.symbols[index];
    let id = map.symbol_ids.get(index).copied().unwrap_or(-1);

    // Map coordinates are in mm on the paper; the shapes are defined in
    // meters on the ground.
    let mm_per_meter = 1000.0 / map.scale_denominator as f64;

    let reference = SymbolRef {
        index,
        id,
        is_rotatable: is_rotatable(symbol),
        contained_types: contained_types(map, symbol, 0),
        has_rotatable_fill_pattern: has_rotatable_fill_pattern(map, symbol, 0),
        minimum_length: minimum_length(map, symbol, 0),
        minimum_area: minimum_area(map, symbol, 0),
    };

    let mut sheet = make_sheet(map, &reference, mm_per_meter);
    if sheet.is_empty() {
        return Ok(Outcome::Skipped);
    }
    lay_out(map, &mut sheet);

    // The symbol, and everything it is drawn out of: a combined symbol's
    // parts are symbols of the map in their own right, and the file has to
    // carry them for the parts to resolve. Renumbered from zero, since a
    // part is named by its position in the list.
    let wanted = wanted_symbols(fragments, id);
    let mapping: std::collections::HashMap<i32, i32> = wanted
        .iter()
        .enumerate()
        .map(|(new, &old)| (old, new as i32))
        .collect();
    let symbols: Vec<Fragment> = wanted
        .iter()
        .filter_map(|old| fragments.symbol(*old))
        .enumerate()
        .map(|(new, fragment)| renumbered(fragment, new as i32, &mapping))
        .collect();
    if symbols.is_empty() {
        return Err(format!(
            "{}: symbol {} is not in the file as the reader found it",
            source.display(),
            base_name(map, index)
        ));
    }
    // The objects are drawn with the symbol the file is for, wherever the
    // renumbering has just put it.
    let written_id = mapping.get(&id).copied().unwrap_or(0);
    for object in &mut sheet.objects {
        object.symbol_id = written_id;
    }

    let name = base_name(map, index);
    let file = MapFile {
        scale_denominator: map.scale_denominator,
        // Every colour, in the file's own order: that order is the drawing
        // order of the whole map, and a symbol names a colour by its place in
        // it, so dropping the unused ones would mean renumbering the rest.
        colors: &fragments.colors,
        symbols: &symbols,
        objects: &sheet.objects,
        rotatable: reference.is_rotatable,
    };
    file.write(&into.join(format!("{name}.omap")))?;

    let index_text = sheet.index(symbol, source, map.scale_denominator);
    let index_path = into.join(format!("{name}.txt"));
    std::fs::write(&index_path, index_text)
        .map_err(|e| format!("cannot write {}: {e}", index_path.display()))?;

    Ok(Outcome::Written(name))
}

/// How a combined symbol's fragment names one of its parts.
const PART_REFERENCE: &str = "<part symbol=\"";

/// The symbols a combined symbol's fragment refers to.
///
/// A part is named by its *position* in the map's symbol list — which
/// `CombinedSymbol::loadingFinishedEvent` looks up directly, and rejects the
/// whole file over when it is out of range. In a file Mapper wrote that
/// position is also the symbol's `id`, which is how the reference is followed
/// here.
fn part_references(text: &str) -> Vec<i32> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(PART_REFERENCE) {
        rest = &rest[at + PART_REFERENCE.len()..];
        let value = rest.split('"').next().unwrap_or_default();
        if let Ok(index) = value.parse() {
            found.push(index);
        }
    }
    found
}

/// The symbols one generated map needs: the one it is for, and everything
/// that one is built out of, transitively.
///
/// They come out in the order the source map had them, which is the order
/// `Map::importMap` puts them in, and so is not always the wanted symbol
/// first: a combined symbol whose parts are defined above it in the symbol
/// set is the second or the third symbol of its own map file.
fn wanted_symbols(fragments: &Fragments, id: i32) -> Vec<i32> {
    let mut wanted = vec![id];
    let mut at = 0;
    while at < wanted.len() {
        let Some(fragment) = fragments.symbol(wanted[at]) else {
            at += 1;
            continue;
        };
        for part in part_references(&fragment.text) {
            if !wanted.contains(&part) && fragments.symbol(part).is_some() {
                wanted.push(part);
            }
        }
        at += 1;
    }
    wanted.sort_unstable();
    wanted
}

/// A symbol's fragment with its own id and its parts' references renumbered
/// to where they are in the file being written.
///
/// The renumbering is not cosmetic: a part reference is a position in the
/// symbol list, so a symbol which keeps the number it had in a map of 169
/// symbols refers to nothing at all in a map of three.
fn renumbered(
    fragment: &Fragment,
    new_id: i32,
    mapping: &std::collections::HashMap<i32, i32>,
) -> Fragment {
    let text = &fragment.text;
    let head_end = text.find('>').map_or(text.len(), |at| at + 1);
    let head = &text[..head_end];
    let mut out = String::with_capacity(text.len());

    match head.find(" id=\"") {
        Some(at) => {
            let from = at + " id=\"".len();
            let to = from + head[from..].find('"').unwrap_or(0);
            out.push_str(&head[..from]);
            out.push_str(&new_id.to_string());
            out.push_str(&head[to..]);
        }
        // A symbol which came without an id gets one: the objects have to be
        // able to name it.
        None => {
            let after = "<symbol".len().min(head.len());
            out.push_str(&head[..after]);
            out.push_str(&format!(" id=\"{new_id}\""));
            out.push_str(&head[after..]);
        }
    }

    let mut rest = &text[head_end..];
    while let Some(at) = rest.find(PART_REFERENCE) {
        let value_at = at + PART_REFERENCE.len();
        out.push_str(&rest[..value_at]);
        rest = &rest[value_at..];
        let end = rest.find('"').unwrap_or(0);
        let old: i32 = rest[..end].parse().unwrap_or(-1);
        out.push_str(&mapping.get(&old).copied().unwrap_or(old).to_string());
        rest = &rest[end..];
    }
    out.push_str(rest);

    Fragment {
        id: new_id,
        text: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symbol_number_is_numbers_not_text() {
        assert_eq!(number_as_string("101.1"), "101.1");
        assert_eq!(number_as_string("101"), "101");
        assert_eq!(number_as_string("07.1"), "7.1");
        assert_eq!(number_as_string(""), "");
        // Not a number at all: QString::toInt() gives 0.
        assert_eq!(number_as_string("A"), "0");
        // Only three components are kept.
        assert_eq!(number_as_string("1.2.3.4"), "1.2.3");
    }

    #[test]
    fn a_file_name_component_keeps_only_what_every_platform_allows() {
        assert_eq!(sanitized("Slope line, contour"), "Slope_line__contour");
        assert_eq!(sanitized("101.1"), "101.1");
        assert_eq!(sanitized("Road (paved)"), "Road__paved_");
        assert_eq!(sanitized("well-known"), "well-known");
    }

    #[test]
    fn markup_is_taken_out_of_a_name_which_has_any() {
        assert_eq!(plain_text_name("Contour"), "Contour");
        assert_eq!(plain_text_name("<b>Contour</b>"), "Contour");
        // A name with no markup is left exactly as it is, entities and all.
        assert_eq!(plain_text_name("Rock &amp; scree"), "Rock &amp; scree");
    }

    #[test]
    fn a_label_rounds_the_way_qt_rounds_it() {
        assert_eq!(label_number(5.0), "5");
        assert_eq!(label_number(100.0), "100");
        assert_eq!(label_number(50.0), "50");
        assert_eq!(label_number_3(0.0234567), "0.0235");
        assert_eq!(label_number_3(12.3456), "12.3");
        assert_eq!(label_number_3(0.5), "0.5");
    }

    #[test]
    fn a_closed_square_repeats_its_first_coordinate() {
        let mut builder = PathBuilder::new(0.1);
        add_square(&mut builder, 100.0);
        // Four corners plus the repeat, which carries the close flag.
        assert_eq!(builder.coords.len(), 5);
        assert_eq!(builder.coords[4].flags, coord_flag::CLOSE_POINT);
        assert_eq!(builder.coords[0].x, builder.coords[4].x);
        assert_eq!(builder.coords[0].y, builder.coords[4].y);
    }

    #[test]
    fn a_circle_is_four_bezier_segments_which_close_on_their_start() {
        let mut builder = PathBuilder::new(0.1);
        add_circle(&mut builder, 100.0);
        // One start plus three coordinates per segment, the last of which is
        // the start again, so it carries the close flag rather than repeating.
        assert_eq!(builder.coords.len(), 13);
        assert_eq!(builder.coords[0].flags, coord_flag::CURVE_START);
        assert!(builder.coords[12].flags & coord_flag::CLOSE_POINT != 0);
    }

    #[test]
    fn a_square_with_a_hole_is_two_parts() {
        let mut builder = PathBuilder::new(0.1);
        add_square_with_hole(&mut builder, 100.0);
        let holes = builder.coords.iter().filter(|c| c.is_hole_point()).count();
        assert_eq!(holes, 1);
    }

    /// A symbol set of four symbols, the last of which is combined out of two
    /// of the others, spelled the way a file spells them.
    fn symbol_set() -> Fragments {
        let symbols = [
            (0, "<symbol type=\"2\" id=\"0\" code=\"101\" name=\"A\"><line_symbol/></symbol>"),
            (1, "<symbol type=\"4\" id=\"1\" code=\"102\" name=\"B\"><area_symbol/></symbol>"),
            (2, "<symbol type=\"2\" id=\"2\" code=\"103\" name=\"C\"><line_symbol/></symbol>"),
            (
                3,
                "<symbol type=\"16\" id=\"3\" code=\"104\" name=\"D\"><combined_symbol parts=\"2\">\
                 <part symbol=\"2\"/><part symbol=\"1\"/></combined_symbol></symbol>",
            ),
        ];
        Fragments {
            colors: Vec::new(),
            symbols: symbols
                .iter()
                .map(|(id, text)| Fragment {
                    id: *id,
                    text: (*text).to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_plain_symbol_needs_nothing_but_itself() {
        assert_eq!(wanted_symbols(&symbol_set(), 1), [1]);
    }

    #[test]
    fn a_combined_symbol_brings_its_parts_in_the_order_the_source_had_them() {
        // Its parts are 2 and 1, and it is 3: the file holds 1, 2, 3.
        assert_eq!(wanted_symbols(&symbol_set(), 3), [1, 2, 3]);
    }

    #[test]
    fn the_parts_are_renumbered_to_where_they_end_up() {
        let set = symbol_set();
        let wanted = wanted_symbols(&set, 3);
        let mapping: std::collections::HashMap<i32, i32> = wanted
            .iter()
            .enumerate()
            .map(|(new, &old)| (old, new as i32))
            .collect();
        let combined = renumbered(set.symbol(3).unwrap(), 2, &mapping);
        // A part is a position in the symbol list, so 2 and 1 of a map of 169
        // symbols become 1 and 0 of a map of three.
        assert!(combined.text.contains("id=\"2\""), "{}", combined.text);
        assert!(
            combined
                .text
                .contains("<part symbol=\"1\"/><part symbol=\"0\"/>"),
            "{}",
            combined.text
        );
        // Nothing else about the symbol is touched.
        assert!(
            combined.text.contains("code=\"104\" name=\"D\""),
            "{}",
            combined.text
        );
    }

    #[test]
    fn a_symbol_which_came_without_an_id_is_given_one() {
        let fragment = Fragment {
            id: -1,
            text: "<symbol type=\"2\" code=\"1\"></symbol>".to_string(),
        };
        let renamed = renumbered(&fragment, 0, &std::collections::HashMap::new());
        assert_eq!(
            renamed.text,
            "<symbol id=\"0\" type=\"2\" code=\"1\"></symbol>"
        );
    }

    #[test]
    fn coordinates_are_rounded_to_what_the_file_can_hold() {
        // 1:15000 gives 1/15 mm per meter, which is not a whole number of
        // native units for most sizes.
        let mut builder = PathBuilder::new(1000.0 / 15000.0);
        add_straight_line(&mut builder, 5.0);
        for coord in &builder.coords {
            assert_eq!(coord.x * 1000.0, (coord.x * 1000.0).round(), "{}", coord.x);
        }
    }
}
