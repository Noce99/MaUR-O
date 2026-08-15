//! Turns the objects of a map into painter paths, and draws them. Ported
//! from `renderer.h`/`renderer.cpp`.
//!
//! A renderable is a path which is either filled or stroked with a single
//! map color. This mirrors what Mapper does: the drawing order of a map is
//! defined by the color priorities, not by the objects, so all renderables
//! have to be collected before anything can be drawn.
//!
//! All coordinates are in mm on the paper.

use tiny_skia::{FillRule, Mask, Paint, PixmapMut, Stroke, Transform};

use crate::geometry::*;
use crate::map::*;
use crate::text;

/// Mapper's own `LineSymbol::miterLimit()`, in units of half the pen width.
const MITER_LIMIT: f64 = 1.0;

/// Qt's `QPen` miter join, which Mapper's own renderer uses, does not fall
/// back to a bevel past the miter limit: it keeps the miter direction but
/// clips its tip at a fixed distance -- `2 * half_width * MITER_LIMIT` --
/// beyond the vertex, along the tangent (see `extent_include_join` in
/// geometry.rs, ported from the same Mapper source and validated against it
/// pixel-for-pixel down to 1-degree corners). That is tiny_skia's
/// `LineJoin::MiterClip`, not `LineJoin::Miter` (which clips by switching to
/// a bevel, a visibly different shape for sharp angles -- confirmed to
/// collapse small, thick-stroked shapes like a five-pointed star into a
/// round blob). `MiterClip`'s `miter_limit` uses the same
/// ratio-to-full-pen-width convention as the clip distance above.
const TINY_SKIA_MITER_LIMIT: f64 = 2.0 * MITER_LIMIT;

fn pen_cap(style: i32) -> PenCap {
    match style {
        cap_style::ROUND => PenCap::Round,
        cap_style::SQUARE => PenCap::Square,
        // A pointed cap tapers to zero width; it is approximated by a flat cap.
        _ => PenCap::Flat,
    }
}

fn pen_join(style: i32) -> PenJoin {
    match style {
        join_style::BEVEL => PenJoin::Bevel,
        join_style::ROUND => PenJoin::Round,
        _ => PenJoin::Miter,
    }
}

/// The rotation angle matching a tangent vector, in the raw coordinate
/// system, whose y axis points down, so that it can be applied by
/// [`Renderer::add_point`] directly.
fn tangent_rotation(tangent: Point) -> f64 {
    tangent.y.atan2(tangent.x)
}

fn unit(v: Point) -> Point {
    v.normalized()
}

/// The incoming and outgoing direction at a vertex of a polyline.
struct VertexTangents {
    incoming: Option<Point>,
    outgoing: Option<Point>,
}

fn vertex_tangents(part: &PathPart, i: usize) -> VertexTangents {
    const EPS2: f64 = 0.000625;
    let points = &part.points;
    let last = points.len() - 1;

    let mut incoming = None;
    let mut k = i;
    while k > 0 && incoming.is_none() {
        k -= 1;
        let tangent = points[i] - points[k];
        if tangent.dot(tangent) > EPS2 { incoming = Some(tangent); }
    }
    if incoming.is_none() && part.closed && last > i + 1 {
        let mut k = last;
        while k > i && incoming.is_none() {
            let tangent = points[i] - points[k];
            if tangent.dot(tangent) > EPS2 { incoming = Some(tangent); }
            k -= 1;
        }
    }

    let mut outgoing = None;
    let mut k = i + 1;
    while k <= last && outgoing.is_none() {
        let tangent = points[k] - points[i];
        if tangent.dot(tangent) > EPS2 { outgoing = Some(tangent); }
        k += 1;
    }
    if outgoing.is_none() && part.closed {
        let mut k = 0;
        while k < i && outgoing.is_none() {
            let tangent = points[k] - points[i];
            if tangent.dot(tangent) > EPS2 { outgoing = Some(tangent); }
            k += 1;
        }
    }

    VertexTangents { incoming, outgoing }
}

/// Converts our path IR to a `tiny_skia::Path`, casting mm coordinates
/// (f64) to tiny-skia's f32. Returns `None` for an empty or degenerate
/// path, matching `QPainterPath::isEmpty()` guards at the call sites.
fn to_skia_path(path: &Path) -> Option<tiny_skia::Path> {
    let mut builder = tiny_skia::PathBuilder::new();
    for cmd in &path.commands {
        match *cmd {
            PathCommand::MoveTo(p) => builder.move_to(p.x as f32, p.y as f32),
            PathCommand::LineTo(p) => builder.line_to(p.x as f32, p.y as f32),
            PathCommand::CubicTo(c1, c2, end) => builder.cubic_to(
                c1.x as f32, c1.y as f32, c2.x as f32, c2.y as f32, end.x as f32, end.y as f32,
            ),
            PathCommand::Close => builder.close(),
        }
    }
    builder.finish()
}

struct Renderable {
    path: Path,
    color: i32,
    clip: Option<usize>,
    /// Stroke width in mm, 0 for a filled path.
    pen_width: f64,
    cap: PenCap,
    join: PenJoin,
    /// Whether a `PenJoin::Miter` join is realized as tiny_skia's
    /// `LineJoin::MiterClip` (`true`, matching Mapper's own miter join --
    /// see `TINY_SKIA_MITER_LIMIT`) or its plain `LineJoin::Miter` (`false`).
    /// Set to `false` only for a border line whose flattened path contains a
    /// cusp (see `has_sharp_fold` in geometry.rs): a border is shifted off
    /// its main line by a fixed amount that can exceed the local radius of
    /// curvature on a tight curve, folding the shifted path onto itself, and
    /// `MiterClip` reacts to the resulting near-reversal join with a wildly
    /// overlong spike where plain `Miter` cleanly falls back to a bevel.
    miter_clip: bool,
    /// `QPainterPath` defaults to `Qt::OddEvenFill` (which is how a hole
    /// punched by a `HolePoint`-flagged inner loop works); text explicitly
    /// opts into `Qt::WindingFill` so that overlapping glyph contours don't
    /// punch accidental holes. Irrelevant for stroked renderables.
    fill_rule: FillRule,
    /// Applied at paint time, for text: the glyph outlines stay in font
    /// units, so that they are flattened at the size at which they are
    /// drawn, as in Mapper.
    transform: Option<Transform>,
}

/// Creates the renderables for all objects of the map, and draws them.
pub struct Renderer<'m> {
    map: &'m Map,
    renderables: Vec<Renderable>,
    clips: Vec<Path>,
    extent: Rect,
    /// The clip path applying to new renderables; set while filling an area.
    current_clip: Option<usize>,
}

impl<'m> Renderer<'m> {
    /// Creates the renderables for all objects of the map.
    pub fn new(map: &'m Map) -> Renderer<'m> {
        let mut renderer = Renderer {
            map,
            renderables: Vec::new(),
            clips: Vec::new(),
            extent: Rect::default(),
            current_clip: None,
        };
        for object in &map.objects {
            renderer.add_object(object);
        }
        renderer
    }

    /// The bounding box of everything which will be drawn. Null for a map
    /// without visible objects.
    pub fn extent(&self) -> Rect {
        self.extent
    }

    fn include(&mut self, bounds: Rect) {
        // A clipped renderable is a fill pattern, which cannot reach beyond
        // the area carrying it. Mapper leaves it out of the extent for that
        // reason, and the size of the image depends on it.
        if self.current_clip.is_some() { return; }
        self.extent = self.extent.united(&bounds);
    }

    /// Fills with `Qt::OddEvenFill`, `QPainterPath`'s default -- correct
    /// for every filled shape except text (see `fill_winding`).
    pub(crate) fn fill(&mut self, path: Path, color: i32, bounds: Option<Rect>, transform: Option<Transform>) {
        self.fill_impl(path, color, bounds, transform, FillRule::EvenOdd);
    }

    /// Fills with `Qt::WindingFill`, as Mapper explicitly sets for text
    /// paths so overlapping glyph contours don't punch accidental holes.
    pub(crate) fn fill_winding(&mut self, path: Path, color: i32, bounds: Option<Rect>, transform: Option<Transform>) {
        self.fill_impl(path, color, bounds, transform, FillRule::Winding);
    }

    fn fill_impl(&mut self, path: Path, color: i32, bounds: Option<Rect>, transform: Option<Transform>, fill_rule: FillRule) {
        if self.map.color(color).is_none() || path.is_empty() { return; }
        let b = bounds.unwrap_or_else(|| path.control_point_rect());
        self.include(b);
        self.renderables.push(Renderable {
            path, color, clip: self.current_clip, pen_width: 0.0,
            cap: PenCap::Flat, join: PenJoin::Miter, miter_clip: true, fill_rule, transform,
        });
    }

    pub(crate) fn stroke(&mut self, path: Path, color: i32, width: f64, cap: PenCap, join: PenJoin,
              bounds: Option<Rect>, transform: Option<Transform>) {
        self.stroke_impl(path, color, width, cap, join, true, bounds, transform);
    }

    /// Like [`Self::stroke`], but for a border line shifted off a main line
    /// (see the `miter_clip` field doc on [`Renderable`]).
    fn stroke_no_miter_clip(&mut self, path: Path, color: i32, width: f64, cap: PenCap, join: PenJoin,
              bounds: Option<Rect>, transform: Option<Transform>) {
        self.stroke_impl(path, color, width, cap, join, false, bounds, transform);
    }

    fn stroke_impl(&mut self, path: Path, color: i32, width: f64, cap: PenCap, join: PenJoin, miter_clip: bool,
              bounds: Option<Rect>, transform: Option<Transform>) {
        if self.map.color(color).is_none() || width <= 0.0 || path.is_empty() { return; }
        if let Some(b) = bounds {
            self.include(b);
        } else {
            // Mapper measures a line of a negative color -- registration
            // black -- with a zero pen width.
            let half_width = if color >= 0 { width / 2.0 } else { 0.0 };
            let mut extent = Rect::default();
            for polygon in path.to_subpath_polygons() {
                let rect = stroked_polyline_extent(&polygon, half_width, cap, join);
                extent = if extent.is_null() { rect } else { extent.united(&rect) };
            }
            self.include(extent);
        }
        self.renderables.push(Renderable {
            path, color, clip: self.current_clip, pen_width: width, cap, join, miter_clip,
            fill_rule: FillRule::EvenOdd, transform,
        });
    }

    fn add_object(&mut self, object: &Object) {
        if let Some(idx) = object.symbol_index {
            let symbol = &self.map.symbols[idx];
            if symbol.is_visible() {
                self.add_symbol(symbol, object, &object.coords);
            }
        }
    }

    fn add_symbol(&mut self, symbol: &Symbol, object: &Object, coords: &CoordList) {
        match symbol {
            Symbol::Point(point) => {
                if let Some(first) = coords.first() {
                    // The rotation of an object turns the symbol clockwise
                    // on the paper, where the y axis points down, hence the
                    // negative sign.
                    let rotation = if point.is_rotatable { -object.rotation } else { 0.0 };
                    self.add_point(point, first.pos(), rotation, 1.0);
                }
            }
            Symbol::Line(line) => self.add_line(line, coords),
            Symbol::Area(area) => self.add_area(area, coords, object),
            Symbol::Text(text_symbol) => {
                if let ObjectKind::Text(text_object) = &object.kind {
                    self.add_text(text_symbol, object, text_object);
                }
            }
            Symbol::Combined(combined) => {
                for part_ref in &combined.parts {
                    let part = match part_ref {
                        PartRef::Shared(i) => Some(&self.map.symbols[*i]),
                        PartRef::Private(i) => Some(&combined.owned_parts[*i]),
                        PartRef::None => None,
                    };
                    if let Some(part) = part {
                        if part.is_visible() {
                            self.add_symbol(part, object, coords);
                        }
                    }
                }
            }
        }
    }

    fn add_point(&mut self, symbol: &PointSymbol, position: Point, rotation: f64, scale: f64) {
        if is_color(symbol.inner_color) && symbol.inner_radius > 0.0 {
            let mut path = Path::new();
            add_ellipse(&mut path, position, symbol.inner_radius, symbol.inner_radius);
            self.fill(path, symbol.inner_color, None, None);
        }

        if is_color(symbol.outer_color) && symbol.outer_width > 0.0 {
            let radius = symbol.inner_radius + symbol.outer_width / 2.0;
            let mut path = Path::new();
            add_ellipse(&mut path, position, radius, radius);
            // Mapper's extent of a circle is inflated by the full line
            // width, not by half of it.
            let reach = symbol.inner_radius + symbol.outer_width;
            let bounds = Rect::new(position.x - reach, position.y - reach, 2.0 * reach, 2.0 * reach);
            self.stroke(path, symbol.outer_color, symbol.outer_width, PenCap::Flat, PenJoin::Miter, Some(bounds), None);
        }

        if symbol.elements.is_empty() { return; }

        // The scale enlarges the element coordinates, but not the widths
        // and radii of their symbols. It is used for dash symbols at
        // corners of a line.
        let cos_r = rotation.cos();
        let sin_r = rotation.sin();
        for element in &symbol.elements {
            let mut coords = element.object.coords.clone();
            for coord in &mut coords {
                let x = coord.x * scale;
                let y = coord.y * scale;
                coord.x = x * cos_r - y * sin_r + position.x;
                coord.y = y * cos_r + x * sin_r + position.y;
            }
            self.add_symbol(&element.symbol, &element.object, &coords);
        }
    }

    /// Adds a point symbol of an unclipped fill pattern, part by part,
    /// checking each part against the outline of the area. Port of
    /// Mapper's `PointSymbol::createRenderablesIf*Inside()` family; `mode`
    /// is the `no_clipping` value of the pattern.
    fn add_point_checked(&mut self, symbol: &PointSymbol, position: Point, rotation: f64, outline: &Path, mode: i32) {
        let contains = |p: Point| -> bool { outline.contains_even_odd(p) };
        let completely = |center: Point, r: f64| {
            contains(Point::new(center.x - r, center.y))
                && contains(Point::new(center.x, center.y - r))
                && contains(Point::new(center.x + r, center.y))
                && contains(Point::new(center.x, center.y + r))
        };
        let partially = |center: Point, r: f64| {
            contains(Point::new(center.x - r, center.y))
                || contains(Point::new(center.x, center.y - r))
                || contains(Point::new(center.x + r, center.y))
                || contains(Point::new(center.x, center.y + r))
        };
        let primitives = |center: Point| match mode {
            1 => completely(center, symbol.inner_radius),
            3 => partially(center, symbol.inner_radius),
            _ => contains(center),
        };
        let circle_ok = |center: Point| {
            let r = symbol.inner_radius + symbol.outer_width / 2.0;
            match mode {
                1 => completely(center, r),
                3 => partially(center, r),
                _ => contains(center),
            }
        };

        if is_color(symbol.inner_color) && symbol.inner_radius > 0.0 && primitives(position) {
            let mut path = Path::new();
            add_ellipse(&mut path, position, symbol.inner_radius, symbol.inner_radius);
            self.fill(path, symbol.inner_color, None, None);
        }
        if is_color(symbol.outer_color) && symbol.outer_width > 0.0 && circle_ok(position) {
            let radius = symbol.inner_radius + symbol.outer_width / 2.0;
            let mut path = Path::new();
            add_ellipse(&mut path, position, radius, radius);
            let reach = symbol.inner_radius + symbol.outer_width;
            let bounds = Rect::new(position.x - reach, position.y - reach, 2.0 * reach, 2.0 * reach);
            self.stroke(path, symbol.outer_color, symbol.outer_width, PenCap::Flat, PenJoin::Miter, Some(bounds), None);
        }

        let cos_r = rotation.cos();
        let sin_r = rotation.sin();
        for element in &symbol.elements {
            let mut coords = element.object.coords.clone();
            let mut center = Point::ZERO;
            let mut inside = mode != 3;
            let mut broke = false;
            for coord in &mut coords {
                let x = coord.x;
                let y = coord.y;
                coord.x = x * cos_r - y * sin_r + position.x;
                coord.y = y * cos_r + x * sin_r + position.y;
                center += Point::new(coord.x, coord.y);
                if mode == 1 && !contains(Point::new(coord.x, coord.y)) {
                    inside = false;
                    broke = true;
                    break;
                }
                if mode == 3 && contains(Point::new(coord.x, coord.y)) {
                    inside = true;
                }
            }
            let _ = broke;
            if mode == 2 {
                inside = !coords.is_empty() && contains(center / coords.len() as f64);
            }
            if inside {
                self.add_symbol(&element.symbol, &element.object, &coords);
            }
        }
    }

    fn add_text(&mut self, symbol: &TextSymbol, object: &Object, text_object: &TextObject) {
        text::add_text(self, symbol, object, text_object);
    }
}

/// Returns the section boundaries of a path part.
///
/// A section is delimited by the ends of the part and by its dash points.
/// Both the dash pattern and the mid symbols of a line are laid out per
/// section.
fn section_boundaries(part: &PathPart) -> Vec<f64> {
    let mut boundaries = vec![0.0];
    for i in 1..part.dash_points.len().saturating_sub(1) {
        if part.dash_points[i] {
            boundaries.push(part.lengths[i]);
        }
    }
    boundaries.push(part.length());
    boundaries
}

/// Distributes the mid symbols of a solid line along one section.
///
/// Follows Mapper's `LineSymbol::createMidSymbolRenderables()`: the number
/// of segments which fits the length best is determined first, and the
/// segment and end lengths are then adjusted so that the symbols are
/// distributed evenly.
fn mid_symbol_positions_section(offset: f64, length: f64, symbol: &LineSymbol, positions: &mut Vec<f64>) {
    let num_gaps = symbol.mid_symbols_per_spot - 1;
    let symbols_length = num_gaps as f64 * symbol.mid_symbol_distance;
    let segment_length = symbol.segment_length;
    let end_length = symbol.end_length;

    let spot = |positions: &mut Vec<f64>, position: f64| {
        for i in 0..symbol.mid_symbols_per_spot {
            positions.push(offset + position + i as f64 * symbol.mid_symbol_distance);
        }
    };

    let segmented_length = (length - 2.0 * end_length).max(0.0) - symbols_length;
    let count_raw = (segmented_length / (segment_length + symbols_length))
        .max(if end_length <= 0.0 { 1.0 } else { 0.0 });
    let lower_count = count_raw.floor() as i32;
    let higher_count = count_raw.ceil() as i32;

    if end_length > 0.0 {
        if length <= symbols_length {
            if symbol.show_at_least_one_symbol {
                positions.push(offset);
                positions.push(offset + length);
            }
            return;
        }

        let deviation_of = |count: i32| -> f64 {
            (length - count as f64 * segment_length
                - (count + 1) as f64 * symbols_length - 2.0 * end_length).abs()
        };
        let lower_deviation = deviation_of(lower_count);
        let higher_deviation = deviation_of(higher_count);
        let count = if lower_deviation >= higher_deviation { higher_count } else { lower_count };
        let deviation = if lower_deviation >= higher_deviation { -higher_deviation } else { lower_deviation };

        let ideal_length = count as f64 * segment_length + 2.0 * end_length;
        if ideal_length <= 0.0 { return; }

        let adjusted_end = end_length + deviation * (end_length / ideal_length);
        let mut adjusted_segment = segment_length + deviation * (segment_length / ideal_length);
        if adjusted_segment < 0.0 { return; }
        if !symbol.show_at_least_one_symbol && higher_count <= 0
            && length <= 2.0 * end_length - (segment_length + symbols_length) / 2.0
        {
            return;
        }

        adjusted_segment += symbols_length;
        for i in 0..=count {
            spot(positions, adjusted_end + i as f64 * adjusted_segment);
        }
    } else {
        if length > symbols_length && lower_count > 0 && higher_count > 0 {
            let deviation_of = |count: i32| -> f64 {
                (length - count as f64 * segment_length - (count + 1) as f64 * symbols_length).abs() / count as f64
            };
            let count = if deviation_of(lower_count) > deviation_of(higher_count) { higher_count } else { lower_count };
            let adapted_segment = (length - (count + 1) as f64 * symbols_length) / count as f64 + symbols_length;
            if adapted_segment >= symbols_length {
                for i in 0..=count {
                    for s in 0..symbol.mid_symbols_per_spot {
                        // The outermost symbols are added outside of this loop.
                        if i == 0 && s == 0 { continue; }
                        if i == count && s == num_gaps { break; }
                        positions.push(offset + i as f64 * adapted_segment + s as f64 * symbol.mid_symbol_distance);
                    }
                }
            }
        }
        positions.push(offset + length);
    }
}

/// Distributes the mid symbols of a solid line along a whole path part.
fn mid_symbol_positions(part: &PathPart, symbol: &LineSymbol) -> Vec<f64> {
    let mut positions = Vec::new();

    // The symbol at the very start of an open part is added only once.
    if symbol.end_length <= 0.0 && !part.closed {
        positions.push(0.0);
    }

    let boundaries = section_boundaries(part);
    for i in 0..boundaries.len().saturating_sub(1) {
        mid_symbol_positions_section(boundaries[i], boundaries[i + 1] - boundaries[i], symbol, &mut positions);
    }
    positions
}

/// Finds the flattened index of the next dash point at or behind the given
/// length. Returns the last index of the part when there is none.
fn split_index_at(part: &PathPart, length: f64) -> usize {
    if length <= 0.0 || part.lengths.is_empty() { return 0; }
    part.lengths.partition_point(|&x| x < length)
}

fn find_next_dash_point(part: &PathPart, first: usize) -> usize {
    let last = part.points.len() - 1;
    for i in first + 1..last {
        if !part.curve_points[i] && part.dash_points[i] {
            return i;
        }
    }
    last
}

impl<'m> Renderer<'m> {
    fn place_mid_symbol(&mut self, symbol: &PointSymbol, coords: &CoordList, part: &PathPart, position: f64) {
        let location = locate_on_path(coords, part, position);
        let rotation = if symbol.is_rotatable { tangent_rotation(location.tangent) } else { 0.0 };
        self.add_point(symbol, location.pos, rotation, 1.0);
    }

    /// Copies a continuous piece of the path, ending it with a gap flag,
    /// and places the mid symbols centered on the piece. Port of Mapper's
    /// `LineSymbol::processContinuousLine()`.
    fn dashed_continuous_line(&mut self, coords: &CoordList, part: &PathPart, symbol: &LineSymbol,
                               start: f64, end: f64, set_mid_symbols: bool, out: &mut CoordList) {
        copy_path_slice(coords, part, start, end, out);
        if let Some(last) = out.last_mut() {
            last.flags |= coord_flag::GAP_POINT;
        }

        let mid_symbol = match symbol.mid_symbol.as_deref() {
            Some(m) if set_mid_symbols && !m.is_empty() && symbol.mid_symbols_per_spot > 0 => m,
            _ => return,
        };

        let distance = symbol.mid_symbol_distance;
        let mid_symbols_length = (symbol.mid_symbols_per_spot.max(1) - 1) as f64 * distance;
        if mid_symbols_length > end - start { return; }

        let mut position = (start + end - mid_symbols_length) / 2.0;
        self.place_mid_symbol(mid_symbol, coords, part, position);
        for _ in 2..=symbol.mid_symbols_per_spot {
            position += distance;
            self.place_mid_symbol(mid_symbol, coords, part, position);
        }
    }

    /// Lays out the dash groups of one section of a dashed line. Port of
    /// Mapper's `LineSymbol::createDashGroups()`. The group and dash counts
    /// are computed in single precision, as Mapper measures paths in
    /// floats, and this decides which side of a tie the layout falls on.
    ///
    /// Returns where the next section has to take over: a section too
    /// short for dashes, and the last dash before a dash point, are handed
    /// over so that a dash can run continuously across a corner.
    fn create_dash_groups(&mut self, coords: &CoordList, part: &PathPart, symbol: &LineSymbol, out: &mut CoordList,
                           line_start: f64, start: f64, end: f64, is_part_start: bool, is_part_end: bool) -> f64 {
        let mid_symbol = symbol.mid_symbol.as_deref();
        let mid_symbols = if symbol.mid_symbols_per_spot > 0 && mid_symbol.is_some_and(|m| !m.is_empty()) {
            symbol.mid_symbol_placement
        } else {
            mid_symbol_placement::NO_MID_SYMBOLS
        };
        let mid_symbol_distance_f = symbol.mid_symbol_distance as f32;
        let per_spot = symbol.mid_symbols_per_spot;

        let dashes_in_group = symbol.dashes_in_group;
        let start_is_dash_point = !is_part_start;

        let half_first_group = if is_part_start {
            symbol.half_outer_dashes || part.closed
        } else {
            start_is_dash_point && dashes_in_group == 1
        };
        let ends_with_dashpoint = if is_part_end { part.closed } else { true };
        let half_last_group = if is_part_end {
            symbol.half_outer_dashes || part.closed
        } else {
            ends_with_dashpoint && dashes_in_group == 1
        };

        let dash_length_f = symbol.dash_length as f32;
        let break_length_f = symbol.break_length as f32;
        let in_group_break_length_f = symbol.in_group_break_length as f32;

        let total_in_group_dash_length = dashes_in_group as f32 * dash_length_f;
        let total_in_group_break_length = (dashes_in_group - 1) as f32 * in_group_break_length_f;
        let total_group_length = total_in_group_dash_length + total_in_group_break_length;
        let total_group_and_break_length = total_group_length + break_length_f;

        let length = (end - start) as f32;
        let length_plus_break = length + break_length_f;

        let num_half_groups = (half_first_group as i32) + (half_last_group as i32);
        let num_dashgroups_f = num_half_groups as f32
            + (length_plus_break - num_half_groups as f32 * dash_length_f / 2.0) / total_group_and_break_length;
        let lower_dashgroup_count = num_dashgroups_f.floor() as i32;

        let minimum_optimum_num_dashes = dashes_in_group * 2 - num_half_groups / 2;
        let minimum_optimum_length = 2.0 * total_group_and_break_length;
        let switch_deviation = 0.2 * total_group_and_break_length / dashes_in_group as f32;

        let mut set_mid_symbols = mid_symbols != mid_symbol_placement::NO_MID_SYMBOLS
            && (length >= dash_length_f - switch_deviation || symbol.show_at_least_one_symbol);
        if mid_symbols == mid_symbol_placement::CENTER_OF_DASH {
            if let Some(mid_symbol) = mid_symbol {
                if line_start < start || start_is_dash_point {
                    set_mid_symbols = false;
                    // Mid symbols at the begin, for explicit dash points.
                    let mut position = start - (per_spot - 1) as f64 * mid_symbol_distance_f as f64 / 2.0;
                    let mut s = 0;
                    while s < per_spot {
                        if position >= 0.0 {
                            self.place_mid_symbol(mid_symbol, coords, part, position);
                        }
                        position += mid_symbol_distance_f as f64;
                        s += 2;
                    }
                }
                if half_first_group {
                    // Mid symbols at the start, for the closing point or dash points.
                    let mut position = start + (per_spot % 2 + 1) as f64 * mid_symbol_distance_f as f64 / 2.0;
                    let mut s = 1;
                    while s < per_spot && position <= part.length() {
                        self.place_mid_symbol(mid_symbol, coords, part, position);
                        position += mid_symbol_distance_f as f64;
                        s += 2;
                    }
                }
            }
        }

        if length <= 0.0
            || (lower_dashgroup_count <= 1
                && length < minimum_optimum_length - minimum_optimum_num_dashes as f32 * switch_deviation)
        {
            // Too short for dashes.
            if is_part_end {
                self.dashed_continuous_line(coords, part, symbol, line_start, end, set_mid_symbols, out);
                return end;
            }
            return line_start;
        }

        let higher_dashgroup_count = num_dashgroups_f.ceil() as i32;
        let lower_dashgroup_deviation =
            (length_plus_break - lower_dashgroup_count as f32 * total_group_and_break_length) / lower_dashgroup_count as f32;
        let higher_dashgroup_deviation =
            (higher_dashgroup_count as f32 * total_group_and_break_length - length_plus_break) / higher_dashgroup_count as f32;
        let num_dashgroups = if lower_dashgroup_deviation > higher_dashgroup_deviation { higher_dashgroup_count } else { lower_dashgroup_count };

        let num_half_dashes = 2 * num_dashgroups * dashes_in_group - num_half_groups;
        let mut adjusted_dash_length =
            (length - (num_dashgroups - 1) as f32 * break_length_f
                - num_dashgroups as f32 * total_in_group_break_length) * 2.0 / num_half_dashes as f32;
        adjusted_dash_length = adjusted_dash_length.max(0.0);

        let mut cur_length = start as f32;
        let mut dash_start = line_start;

        for dashgroup in 1..=num_dashgroups {
            let is_first_dashgroup = dashgroup == 1;
            let is_last_dashgroup = dashgroup == num_dashgroups;

            if mid_symbols == mid_symbol_placement::CENTER_OF_DASH_GROUP {
                if let Some(mid_symbol) = mid_symbol {
                    let mut position = cur_length;
                    if is_last_dashgroup && half_last_group {
                        position = end as f32;
                    } else if !(is_first_dashgroup && half_first_group) {
                        position += (adjusted_dash_length * dashes_in_group as f32
                            + in_group_break_length_f * (dashes_in_group - 1) as f32) / 2.0;
                    }
                    position -= (mid_symbol_distance_f * (per_spot - 1) as f32) / 2.0;
                    let mut i = 0;
                    while i < per_spot && position as f64 <= end {
                        if position as f64 > start {
                            self.place_mid_symbol(mid_symbol, coords, part, position as f64);
                        }
                        i += 1;
                        position += mid_symbol_distance_f;
                    }
                }
            }

            for dash in 1..=dashes_in_group {
                let is_first_dash = is_first_dashgroup && dash == 1;
                let is_last_dash = is_last_dashgroup && dash == dashes_in_group;

                // A group at the outside or at a dash point loses the outer
                // half of its outer dash.
                let has_start = !(is_first_dash && half_first_group);
                let has_end = !(is_last_dash && half_last_group);
                let is_half_dash = has_start != has_end;
                let cur_dash_length = if is_half_dash { adjusted_dash_length / 2.0 } else { adjusted_dash_length };
                let dash_mid_symbols = mid_symbols == mid_symbol_placement::CENTER_OF_DASH && !is_half_dash;

                if !is_first_dash {
                    // The gap before this dash.
                    copy_path_slice(coords, part, dash_start, cur_length as f64, out);
                    if let Some(last) = out.last_mut() {
                        last.flags |= coord_flag::GAP_POINT;
                    }
                    dash_start = cur_length as f64;
                }
                if is_last_dash && !is_part_end {
                    // The final half dash continues across the dash point;
                    // the next section draws it.
                    return dash_start;
                }

                let dash_end = (cur_length + cur_dash_length) as f64;
                self.dashed_continuous_line(coords, part, symbol, dash_start, dash_end, dash_mid_symbols, out);
                cur_length += cur_dash_length;
                dash_start = dash_end;

                if dash < dashes_in_group {
                    cur_length += in_group_break_length_f;
                }
            }

            if dashgroup < num_dashgroups {
                if mid_symbols == mid_symbol_placement::CENTER_OF_GAP {
                    if let Some(mid_symbol) = mid_symbol {
                        let mut position = cur_length + (break_length_f - mid_symbol_distance_f * (per_spot - 1) as f32) / 2.0;
                        for _ in 0..per_spot {
                            if (position as f64) < start { position += mid_symbol_distance_f; continue; }
                            if position as f64 > end { break; }
                            self.place_mid_symbol(mid_symbol, coords, part, position as f64);
                            position += mid_symbol_distance_f;
                        }
                    }
                }
                cur_length += break_length_f;
            }
        }

        if half_last_group && mid_symbols == mid_symbol_placement::CENTER_OF_DASH {
            if let Some(mid_symbol) = mid_symbol {
                // Mid symbols at the end, for the closing point or dash points.
                let mut position = end as f32 - (per_spot - 1) as f32 * mid_symbol_distance_f / 2.0;
                let mut s = 0;
                while s < per_spot {
                    self.place_mid_symbol(mid_symbol, coords, part, position as f64);
                    position += mid_symbol_distance_f;
                    s += 2;
                }
            }
        }

        end
    }

    /// Builds the dashed version of one part: the same geometry, cut into
    /// dashes by gap flags. Port of Mapper's
    /// `LineSymbol::processDashedLine()`.
    fn dashed_path(&mut self, coords: &CoordList, part: &PathPart, symbol: &LineSymbol,
                    start_length: f64, end_length: f64) -> CoordList {
        let mut out = CoordList::new();
        let mut groups_start_index = split_index_at(part, start_length).max(1) - 1;
        let mut groups_start = start_length;
        let mut line_start = start_length;
        let mut is_part_start = true;
        let mut is_part_end = false;
        while !is_part_end {
            let next_index = find_next_dash_point(part, groups_start_index);
            let mut groups_end = part.lengths[next_index];
            is_part_end = next_index >= part.points.len() - 1 || groups_end >= end_length;
            if is_part_end { groups_end = end_length; }

            line_start = self.create_dash_groups(coords, part, symbol, &mut out, line_start,
                groups_start, groups_end, is_part_start, is_part_end);

            groups_start_index = next_index;
            groups_start = groups_end;
            is_part_start = false;
        }
        out
    }

    fn add_line(&mut self, symbol: &LineSymbol, coords: &CoordList) {
        let parts = flatten(coords);
        if parts.is_empty() { return; }

        let cap = pen_cap(symbol.cap_style);
        let join = pen_join(symbol.join_style);

        let create_line = is_color(symbol.color) && symbol.line_width > 0.0;
        let create_border = symbol.border.is_visible() || symbol.right_border.is_visible();
        let is_dashed = symbol.dashed;

        for part in &parts {
            // Start and end offsets shorten the line; a pointed cap fills
            // the offset with a wedge tapering to zero width.
            let mut start_length = 0.0;
            let mut end_length = part.length();
            let mut line_fits = true;
            if !part.closed && (symbol.start_offset > 0.0 || symbol.end_offset > 0.0) {
                let mut start_offset = symbol.start_offset.max(0.0);
                let mut end_offset = symbol.end_offset.max(0.0);
                let total = start_offset + end_offset;
                let available = end_length.max(0.0);
                if total > available {
                    line_fits = false;
                    start_offset *= available / total;
                    end_offset *= available / total;
                }
                start_length += start_offset;
                end_length = start_length.max(end_length - end_offset);

                if symbol.cap_style == cap_style::POINTED && is_color(symbol.color) {
                    let cap_fn = |renderer: &mut Self, from: f64, to: f64, is_end: bool, cap_len: f64| {
                        let outline = pointed_cap_outline(coords, part, from, to, is_end, symbol.line_width / 2.0, cap_len);
                        let path = to_painter_path(&outline, false);
                        if path.is_empty() { return; }
                        let bounds = flattened_extent(&flatten(&outline));
                        renderer.include(bounds);
                        renderer.fill(path, symbol.color, Some(bounds), None);
                    };
                    // The taper is computed from the offset as configured,
                    // even where the offsets had to be shrunk to fit the
                    // line, so that a very short line keeps a pointed shape.
                    if symbol.start_offset > 0.0 {
                        cap_fn(self, 0.0, start_length, false, symbol.start_offset);
                    }
                    if symbol.end_offset > 0.0 {
                        cap_fn(self, end_length, part.length(), true, symbol.end_offset);
                    }
                }

                if !line_fits { continue; }
            }

            let part_coords: CoordList;
            if is_dashed {
                // A dashed line is one path with gaps cut into it: the dash
                // layout -- with its mid symbols -- is Mapper's, ported in
                // dashed_path().
                if symbol.dash_length > 0.0 && (create_line || create_border) {
                    part_coords = self.dashed_path(coords, part, symbol, start_length, end_length);
                } else {
                    continue;
                }
            } else if start_length > 0.0 || end_length < part.length() {
                let mut sliced = CoordList::new();
                copy_path_slice(coords, part, start_length, end_length, &mut sliced);
                part_coords = sliced;
            } else {
                part_coords = coords[part.first_coord..=part.last_coord].to_vec();
            }
            self.render_line_part(symbol, &part_coords, cap, join, create_line, create_border);
        }

        // The point symbols along the line.
        let place = |renderer: &mut Self, point: Option<&PointSymbol>, part: &PathPart, position: f64| {
            let point = match point {
                Some(p) if !p.is_empty() => p,
                _ => return,
            };
            let location = locate_on_path(coords, part, position);
            let rotation = if point.is_rotatable { tangent_rotation(location.tangent) } else { 0.0 };
            renderer.add_point(point, location.pos, rotation, 1.0);
        };

        // Mid symbols of a dashed line are laid out with the dashes, above.
        if !is_dashed {
            if let Some(mid_symbol) = symbol.mid_symbol.as_deref() {
                if !mid_symbol.is_empty() && symbol.segment_length > 0.0 && symbol.mid_symbols_per_spot > 0 {
                    for part in &parts {
                        for position in mid_symbol_positions(part, symbol) {
                            place(self, Some(mid_symbol), part, position);
                        }
                    }
                }
            }
        }

        // The symbols at explicit dash points. The symbol turns with the
        // average of the two tangents at the vertex, and is stretched to
        // span a miter corner, exactly as Mapper lays it out.
        if let Some(dash_point_symbol) = symbol.dash_symbol.as_deref() {
            if !dash_point_symbol.is_empty() {
                for part in &parts {
                    for i in 0..part.dash_points.len() {
                        if !part.dash_points[i] { continue; }
                        let is_end = i == 0 || i + 1 == part.dash_points.len();
                        if symbol.suppress_dash_symbol_at_ends {
                            if is_end { continue; }
                        } else if part.closed && i == 0 {
                            continue;
                        }

                        let tangents = vertex_tangents(part, i);
                        #[allow(unused_assignments)]
                        let mut direction = Point::ZERO;
                        let mut scaling = 1.0f64;
                        match (tangents.incoming, tangents.outgoing) {
                            (inc, None) => { direction = inc.unwrap_or(Point::ZERO); }
                            (None, Some(out)) => { direction = out; }
                            (Some(inc), Some(out)) => {
                                let to_coord = unit(inc);
                                let to_next = unit(out);
                                if to_next == -to_coord {
                                    direction = Point::new(-to_next.y, to_next.x);
                                    scaling = f64::INFINITY;
                                } else {
                                    direction = unit(to_coord + to_next);
                                    scaling = 1.0 / (direction.x * to_coord.x + direction.y * to_coord.y);
                                }
                            }
                        }

                        let rotation = if dash_point_symbol.is_rotatable { tangent_rotation(direction) } else { 0.0 };
                        let scale = if symbol.scale_dash_symbol { scaling.min(2.0 * MITER_LIMIT) } else { 1.0 };
                        self.add_point(dash_point_symbol, part.points[i], rotation, scale);
                    }
                }
            }
        }

        // Start and end symbols turn with the one-sided tangent, unlike the
        // symbols along the line.
        if let Some(start_symbol) = symbol.start_symbol.as_deref() {
            if !start_symbol.is_empty() {
                let part = &parts[0];
                let mut rotation = 0.0;
                if start_symbol.is_rotatable {
                    if let Some(tangent) = coord_outgoing_tangent(coords, part.first_coord, part.last_coord, part.closed, part.first_coord) {
                        rotation = tangent_rotation(tangent);
                    }
                }
                self.add_point(start_symbol, coords[part.first_coord].pos(), rotation, 1.0);
            }
        }
        if let Some(end_symbol) = symbol.end_symbol.as_deref() {
            if !end_symbol.is_empty() {
                let part = &parts[parts.len() - 1];
                let mut rotation = 0.0;
                if end_symbol.is_rotatable {
                    if let Some(tangent) = coord_incoming_tangent(coords, part.first_coord, part.last_coord, part.closed, part.last_coord) {
                        rotation = tangent_rotation(tangent);
                    }
                }
                self.add_point(end_symbol, coords[part.last_coord].pos(), rotation, 1.0);
            }
        }
    }

    /// Renders one part (or its dashed version) with its border lines.
    fn render_line_part(&mut self, symbol: &LineSymbol, part_coords: &CoordList, cap: PenCap, join: PenJoin,
                         create_line: bool, create_border: bool) {
        if part_coords.len() < 2 { return; }

        let render_parts = flatten(part_coords);

        if create_line {
            // Bezier curves are kept, so that the painter can flatten them
            // at the resolution of the output. The extent, however,
            // follows the flattening and tangents of Mapper, which decide
            // the image size.
            let half_width = if symbol.color >= 0 { symbol.line_width / 2.0 } else { 0.0 };
            let bounds = stroked_path_extent(part_coords, &render_parts, half_width, cap, join);
            self.stroke(to_painter_path(part_coords, true), symbol.color, symbol.line_width, cap, join, Some(bounds), None);
        }

        if !create_border { return; }

        self.add_border(symbol, part_coords, &render_parts, &symbol.border, -1.0);
        self.add_border(symbol, part_coords, &render_parts, &symbol.right_border, 1.0);
    }

    fn stroke_border(&mut self, border: &Border, border_join: PenJoin, border_coords: &CoordList) {
        if border_coords.len() < 2 { return; }
        let half_width = if border.color >= 0 { border.width / 2.0 } else { 0.0 };
        let flattened = flatten(border_coords);
        let bounds = stroked_path_extent(border_coords, &flattened, half_width, PenCap::Flat, border_join);
        let path = to_painter_path(border_coords, true);
        if has_sharp_fold(&flattened, half_width) {
            self.stroke_no_miter_clip(path, border.color, border.width, PenCap::Flat, border_join, Some(bounds), None);
        } else {
            self.stroke(path, border.color, border.width, PenCap::Flat, border_join, Some(bounds), None);
        }
    }

    fn add_border(&mut self, symbol: &LineSymbol, part_coords: &CoordList, render_parts: &[PathPart], border: &Border, sign: f64) {
        if !border.is_visible() { return; }
        let border_join = if symbol.join_style == join_style::ROUND { PenJoin::Round } else { PenJoin::Miter };
        let main_shift = sign * symbol.line_width / 2.0;
        let border_shift = sign * border.shift;

        if border.dashed && border.dash_length > 0.0 && border.break_length > 0.0 {
            // A dashed border: the dashes are laid out along the carrying
            // line, and the result is shifted sideways.
            let border_symbol = LineSymbol {
                dash_length: border.dash_length,
                break_length: border.break_length,
                ..LineSymbol::default()
            };
            for render_part in render_parts {
                let dashed = self.dashed_path(part_coords, render_part, &border_symbol, 0.0, render_part.length());
                for dashed_part in flatten(&dashed) {
                    let shifted = shift_coordinates(&dashed, dashed_part.first_coord, dashed_part.last_coord,
                        dashed_part.closed, main_shift, border_shift, symbol.join_style);
                    self.stroke_border(border, border_join, &shifted);
                }
            }
        } else {
            for render_part in render_parts {
                let shifted = shift_coordinates(part_coords, render_part.first_coord, render_part.last_coord,
                    render_part.closed, main_shift, border_shift, symbol.join_style);
                self.stroke_border(border, border_join, &shifted);
            }
        }
    }

    fn add_area(&mut self, symbol: &AreaSymbol, coords: &CoordList, object: &Object) {
        if coords.len() < 3 { return; }
        let path = to_painter_path(coords, false);
        if path.is_empty() { return; }
        // Mapper measures an area by its flattened outline, not by the
        // exact bezier bounds, and it counts the shape towards the extent
        // even when the area has no color: it is the clip path of the
        // fill patterns.
        let bounds = flattened_extent(&flatten(coords));
        self.include(bounds);
        self.fill(path.clone(), symbol.color, Some(bounds), None);

        if symbol.patterns.is_empty() { return; }

        let (delta_rotation, origin) = match &object.kind {
            ObjectKind::Path(p) => (p.pattern_rotation, p.pattern_origin),
            _ => (0.0, Point::ZERO),
        };
        let canvas = bounds;

        self.clips.push(path);
        let clip = self.clips.len() - 1;
        let outer_clip = self.current_clip;
        for pattern in &symbol.patterns {
            self.current_clip = if pattern.no_clipping != 0 { outer_clip } else { Some(clip) };
            self.add_pattern(pattern, canvas, clip, delta_rotation, origin);
        }
        self.current_clip = outer_clip;
    }

    /// The extent of a point symbol: the extent of its renderables,
    /// measured by creating them and throwing them away again.
    fn point_extent(&mut self, symbol: &PointSymbol, rotation: f64) -> Rect {
        let saved_extent = self.extent;
        let saved_clip = self.current_clip;
        let saved_renderables = self.renderables.len();
        let saved_clips = self.clips.len();

        self.extent = Rect::default();
        self.current_clip = None;
        self.add_point(symbol, Point::ZERO, rotation, 1.0);
        let result = self.extent;

        self.renderables.truncate(saved_renderables);
        self.clips.truncate(saved_clips);
        self.extent = saved_extent;
        self.current_clip = saved_clip;
        result
    }

    fn add_pattern_line(&mut self, pattern: &FillPattern, first: Point, second: Point, delta_offset: f64,
                         delta_rotation: f64, outline: &Path) {
        if pattern.pattern_type == fill_pattern_type::LINE {
            let mut line = Path::new();
            line.move_to(first);
            line.line_to(second);
            self.stroke(line, pattern.line_color, pattern.line_width, PenCap::Flat, PenJoin::Miter, None, None);
            return;
        }

        let direction_raw = second - first;
        let length = direction_raw.length();
        if length <= 0.0 { return; }
        let direction = direction_raw / length;

        let step = pattern.point_distance;
        let along = direction.dot(first) - pattern.offset_along_line - delta_offset;
        let start = (along / step).ceil() * step - along;
        // A pattern with a tiny distance on a huge area could produce a
        // practically unbounded number of renderables.
        if (length - start) / step > 100000.0 { return; }

        let point_symbol = match pattern.point.as_deref() {
            Some(p) => p,
            None => return,
        };
        let mut coord = first + direction * start;
        let mut current = start;
        while current < length {
            if pattern.no_clipping != 0 {
                self.add_point_checked(point_symbol, coord, -delta_rotation, outline, pattern.no_clipping);
            } else {
                self.add_point(point_symbol, coord, -delta_rotation, 1.0);
            }
            current += step;
            coord = coord + direction * step;
        }
    }

    fn add_pattern(&mut self, pattern: &FillPattern, mut canvas: Rect, clip: usize, delta_rotation_in: f64, origin: Point) {
        // Taken by value up front where needed below: placing symbols may
        // grow the clip vector.
        let outline = self.clips.get(clip).cloned().unwrap_or_default();
        if pattern.line_spacing <= 0.0 { return; }
        if pattern.pattern_type == fill_pattern_type::POINT && (pattern.point.is_none() || pattern.point_distance <= 0.0) { return; }
        if pattern.pattern_type == fill_pattern_type::LINE && (!is_color(pattern.line_color) || pattern.line_width <= 0.0) { return; }

        let delta_rotation = if pattern.rotatable { delta_rotation_in } else { 0.0 };

        // The pattern is symmetric with a period of pi.
        let mut rotation = (pattern.angle + delta_rotation) % std::f64::consts::PI;
        if rotation < 0.0 { rotation += std::f64::consts::PI; }

        // Enlarge the canvas so that symbols reaching into the area are not lost.
        let point_extent = if pattern.pattern_type == fill_pattern_type::POINT {
            let point = pattern.point.as_deref().unwrap();
            self.point_extent(point, -delta_rotation)
        } else {
            let margin = pattern.line_width / 2.0;
            Rect::new(-margin, -margin, pattern.line_width, pattern.line_width)
        };
        canvas = canvas.adjusted(-point_extent.right(), -point_extent.bottom(), -point_extent.left(), -point_extent.top());

        // The origin of the object moves the pattern, across the lines and
        // along them. The y axis points down, hence the reflections.
        let dot = |a: Point, b: Point| a.x * b.x + a.y * b.y;
        let mut delta_line_offset = 0.0;
        let mut delta_along_line_offset = 0.0;
        if pattern.rotatable {
            let line_normal = Point::new(rotation.sin(), rotation.cos());
            let line_tangent = Point::new(rotation.cos(), -rotation.sin());
            delta_line_offset = dot(line_normal, origin);
            delta_along_line_offset = dot(line_tangent, origin);
        }
        let offset = pattern.line_offset + delta_line_offset;

        // The lines are laid out along the edges of the canvas, which
        // keeps their start points -- and with them the phase of a point
        // pattern -- the same as in Mapper.
        if (rotation - std::f64::consts::FRAC_PI_2).abs() < 0.0001 {
            // Special case: vertical lines.
            let signed_offset = -delta_along_line_offset;
            let first_offset = offset + ((canvas.left() - offset) / pattern.line_spacing).ceil() * pattern.line_spacing;
            let mut current = first_offset;
            while current < canvas.right() {
                self.add_pattern_line(pattern, Point::new(current, canvas.top()), Point::new(current, canvas.bottom()), signed_offset, delta_rotation, &outline);
                current += pattern.line_spacing;
            }
        } else if rotation.abs() < 0.0001 {
            // Special case: horizontal lines.
            let first_offset = offset + ((canvas.top() - offset) / pattern.line_spacing).ceil() * pattern.line_spacing;
            let mut current = first_offset;
            while current < canvas.bottom() {
                self.add_pattern_line(pattern, Point::new(canvas.left(), current), Point::new(canvas.right(), current), delta_along_line_offset, delta_rotation, &outline);
                current += pattern.line_spacing;
            }
        } else {
            let signed_offset = if rotation < std::f64::consts::FRAC_PI_2 { -delta_along_line_offset } else { delta_along_line_offset };

            let xfactor = 1.0 / rotation.sin();
            let yfactor = 1.0 / rotation.cos();
            let dist_x = xfactor * pattern.line_spacing;
            let dist_y = yfactor * pattern.line_spacing;
            let mut offset_x = xfactor * offset;
            let mut offset_y = yfactor * offset;

            let upwards = rotation < std::f64::consts::FRAC_PI_2;
            let corner_y = if upwards { canvas.top() } else { canvas.bottom() };
            offset_x += -corner_y / rotation.tan();
            offset_y -= canvas.left() * rotation.tan();

            let mut start_x = offset_x + ((canvas.left() - offset_x) / dist_x).ceil() * dist_x;
            let mut start_y = corner_y;
            let mut end_x = canvas.left();
            let mut end_y = offset_y + ((corner_y - offset_y) / dist_y).ceil() * dist_y;

            for _guard in 0..1_000_000 {
                if start_x > canvas.right() {
                    start_y += ((start_x - canvas.right()) / dist_x) * dist_y;
                    start_x = canvas.right();
                }
                if if upwards { end_y > canvas.bottom() } else { end_y < canvas.top() } {
                    let limit = if upwards { canvas.bottom() } else { canvas.top() };
                    end_x += ((end_y - limit) / dist_y) * dist_x;
                    end_y = limit;
                }

                if if upwards { start_y > canvas.bottom() } else { start_y < canvas.top() } { break; }

                self.add_pattern_line(pattern, Point::new(start_x, start_y), Point::new(end_x, end_y), signed_offset, delta_rotation, &outline);

                start_x += dist_x;
                end_y += dist_y;
            }
        }
    }

    /// Draws the map, in the order given by the color priorities: the
    /// color with the highest priority value is drawn first, color 0 comes
    /// last. `page_transform` maps mm-on-paper coordinates to pixels.
    pub fn paint(&self, pixmap: &mut PixmapMut, page_transform: Transform) {
        let mut order: Vec<usize> = (0..self.renderables.len()).collect();
        order.sort_by(|&a, &b| self.renderables[b].color.cmp(&self.renderables[a].color));

        let mut active_clip: Option<usize> = None;
        let mut has_active_clip = false;
        let mut mask: Option<Mask> = None;

        for &index in &order {
            let renderable = &self.renderables[index];
            let map_color = match self.map.color(renderable.color) {
                Some(c) => c,
                None => continue,
            };

            if renderable.clip != active_clip || !has_active_clip {
                active_clip = renderable.clip;
                has_active_clip = true;
                mask = active_clip.and_then(|clip_idx| {
                    let mut m = Mask::new(pixmap.width(), pixmap.height())?;
                    if let Some(skia_path) = to_skia_path(&self.clips[clip_idx]) {
                        // The clip path is an area outline (from
                        // to_painter_path()), which -- like all area fills
                        // -- uses Qt's default OddEvenFill, not Winding.
                        m.fill_path(&skia_path, FillRule::EvenOdd, true, page_transform);
                    }
                    Some(m)
                });
            }

            let (r, g, b) = map_color.rgb;
            let a = if map_color.opacity < 1.0 { map_color.opacity as f32 } else { 1.0 };
            let color = tiny_skia::Color::from_rgba(r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0), a.clamp(0.0, 1.0))
                .unwrap_or(tiny_skia::Color::from_rgba(0.0, 0.0, 0.0, 1.0).unwrap());

            let mut paint = Paint::default();
            paint.set_color(color);
            paint.anti_alias = true;

            let final_transform = match renderable.transform {
                Some(t) => t.post_concat(page_transform),
                None => page_transform,
            };

            let skia_path = match to_skia_path(&renderable.path) {
                Some(p) => p,
                None => continue,
            };

            if renderable.pen_width > 0.0 {
                let stroke = Stroke {
                    width: renderable.pen_width as f32,
                    miter_limit: TINY_SKIA_MITER_LIMIT as f32,
                    line_cap: match renderable.cap {
                        PenCap::Flat => tiny_skia::LineCap::Butt,
                        PenCap::Round => tiny_skia::LineCap::Round,
                        PenCap::Square => tiny_skia::LineCap::Square,
                    },
                    line_join: match renderable.join {
                        PenJoin::Miter if renderable.miter_clip => tiny_skia::LineJoin::MiterClip,
                        PenJoin::Miter => tiny_skia::LineJoin::Miter,
                        PenJoin::Bevel => tiny_skia::LineJoin::Bevel,
                        PenJoin::Round => tiny_skia::LineJoin::Round,
                    },
                    dash: None,
                };
                pixmap.stroke_path(&skia_path, &paint, &stroke, final_transform, mask.as_ref());
            } else {
                pixmap.fill_path(&skia_path, &paint, renderable.fill_rule, final_transform, mask.as_ref());
            }
        }
    }
}
