//! What a map's objects are made into before they are stored in a standard's
//! symbols, so that the standard draws them as the map drew them.
//!
//! A map drawn with its own symbols is stored with the standard's closest ones
//! ([`SymbolTable::closest`]), and where the two are not drawn alike the objects
//! are adapted to the standard's symbol, not stored as they are:
//!
//! - **Cut** ([`crate::mot_cut`]): where the two colour orders disagree, the
//!   areas and lines the standard would draw over what the map draws over them.
//! - **Turned round**: a line symbol with something on one side only (a tunnel
//!   entrance's triangles, a fence's ticks, a one-sided border) is drawn on the
//!   side of the line's direction its symbol has it on. Where the map's symbol
//!   has it on the left and the standard's on the right, or the other way, the
//!   line is reversed: the standard then draws it on the side the map did.
//!   Which side a symbol draws on is worked out from the symbol itself
//!   ([`line_side`]), so no table has to say it.
//! - **Moved aside**: a line which draws nothing along its middle, only symbols
//!   beside it (a tunnel entrance's triangles), where the standard's symbols
//!   stand further from the line or nearer to it than the map's (the club's
//!   triangles straddle the line, ISSprOM's stand on it): the line is moved
//!   sideways by the difference, so that the standard's symbols stand where the
//!   map's did. A line which draws its middle stays where it was drawn.
//! - **Made an area**: a line symbol of the map which paints only its middle,
//!   with no outline (the club's "Traffico", a dark brown band), stored as a
//!   standard symbol of the same colour which outlines it (ISSprOM's
//!   footprints of heavy traffic, with black borders), would gain an outline
//!   the map does not have. Such a line is stored as the area of its footprint
//!   instead, of the standard's plain area of the closest colour.
//! - **Made a small area**: a point symbol of the map which the standard has
//!   no point for, only an area of the same thing (Mapper's sets carry a
//!   single dot of broken ground or of stony ground; ISSprOM has the areas
//!   only, of three dots at least), is stored as the smallest square of that
//!   area the standard allows, round the point
//!   ([`SymbolEntry::least_side`](crate::mot_symbols::SymbolEntry::least_side)).
//! - **Filled under**: a line symbol of the map which paints its middle in a
//!   colour (a stairway in a paved area's brown, with its steps on it), stored
//!   as a standard symbol which leaves the middle empty (ISSprOM's stairways,
//!   which are drawn over a paved area), would let what is under it show. Such
//!   a line gets an area under it, of its footprint: the standard's plain area
//!   whose colour is closest to the one the map's symbol paints
//!   ([`SymbolTable::closest_plain_area`]), where one is close. Again from the
//!   symbols alone.

use std::collections::BTreeMap;

use geo::algorithm::bool_ops::unary_union;
use geo::algorithm::buffer::{BufferStyle, LineCap};
use geo::{Buffer, Coord as GeoCoord, LineString, MultiPolygon};

use crate::geometry::flatten;
use crate::map::{
    cap_style, coord_flag, symbol_leaves, Coord, CoordList, Map, Object, ObjectKind, PathObject,
    PointSymbol, Symbol,
};
use crate::mot_cut::{area_coords, cut_hidden};
use crate::mot_symbols::{SymbolKind, SymbolTable};

/// How far, in mm, a symbol's one-sided parts must reach further on one side
/// than on the other for it to have a side.
const SIDED: f64 = 0.05;

/// What [`adapt`] made of a map's objects.
#[derive(Default)]
pub struct Adapted {
    /// The new coordinates of the objects cut or turned round, by index in
    /// [`Map::objects`]: empty for one taken away whole.
    pub coords: BTreeMap<usize, CoordList>,
    /// Objects stored as well as the map's: each with the id of its symbol in
    /// the table. Its `symbol_index` is the map's symbol it was made for.
    pub extras: Vec<(Object, u16)>,
    /// One line per symbol whose objects were adapted.
    pub warnings: Vec<String>,
}

/// Adapts `map`'s objects to `table`'s symbols. See the module's documentation.
pub fn adapt(map: &Map, table: &SymbolTable) -> Adapted {
    let cuts = cut_hidden(map, table);
    let mut adapted = Adapted {
        coords: cuts.coords,
        extras: Vec::new(),
        warnings: cuts.warnings,
    };
    turn_round(map, table, &mut adapted);
    make_areas(map, table, &mut adapted);
    points_to_areas(map, table, &mut adapted);
    fill_under(map, table, &mut adapted);
    adapted
}

/// The standard's symbol `symbol`, one of `map`'s, is stored as, where it is a
/// line both in the map and in the standard.
fn line_target(symbol: &Symbol, map: &Map, table: &SymbolTable) -> Option<u16> {
    if !symbol.is_visible() || SymbolKind::of(symbol, map) != SymbolKind::Line {
        return None;
    }
    let (id, _) = table.closest(symbol, map)?;
    (table.get(id)?.kind == SymbolKind::Line).then_some(id)
}

/// Reverses the lines whose symbol draws on the other side of the line from the
/// standard's.
fn turn_round(map: &Map, table: &SymbolTable, adapted: &mut Adapted) {
    let mut turned = vec![0usize; map.symbols.len()];
    // For each symbol: whether its lines are reversed, and how far they are
    // moved to the right of their (new) direction, in mm.
    let moves: Vec<(bool, f64)> = map
        .symbols
        .iter()
        .map(|symbol| {
            let Some(entry) = line_target(symbol, map, table).and_then(|id| table.get(id)) else {
                return (false, 0.0);
            };
            let own = line_side(symbol, map);
            if own.abs() < SIDED || entry.side.abs() < SIDED {
                return (false, 0.0);
            }
            let reverse = own * entry.side < 0.0;
            // Where the map's symbols stand, seen along the line as stored.
            let own = if reverse { -own } else { own };
            let aside = if entry.body || body(symbol, map).is_some() {
                0.0
            } else {
                own - entry.side
            };
            (reverse, if aside.abs() >= SIDED { aside } else { 0.0 })
        })
        .collect();
    for (index, object) in map.objects.iter().enumerate() {
        let (Some(s), ObjectKind::Path(_)) = (object.symbol_index, &object.kind) else {
            continue;
        };
        let (reverse, aside) = moves[s];
        if !reverse && aside == 0.0 {
            continue;
        }
        let coords = adapted.coords.get(&index).unwrap_or(&object.coords);
        if coords.len() < 2 {
            continue;
        }
        let mut moved = if reverse {
            reversed(coords)
        } else {
            coords.clone()
        };
        if aside != 0.0 {
            moved = moved_aside(&moved, aside);
        }
        adapted.coords.insert(index, moved);
        turned[s] += 1;
    }
    for (s, n) in turned.into_iter().enumerate() {
        if n == 0 {
            continue;
        }
        let symbol = &map.symbols[s];
        let target = line_target(symbol, map, table)
            .and_then(|id| table.get(id))
            .expect("a symbol turned round has a target");
        let (reverse, aside) = moves[s];
        let what = match (reverse, aside != 0.0) {
            (true, true) => format!(
                "reversed and moved {:.2} mm aside: {} \"{}\" draws on the other side of the \
                 line, {:.2} mm from where the map's symbol does",
                aside.abs(),
                target.code,
                target.name,
                aside.abs()
            ),
            (true, false) => format!(
                "reversed: {} \"{}\" draws on the other side of the line",
                target.code, target.name
            ),
            _ => format!(
                "moved {:.2} mm aside: {} \"{}\" draws {:.2} mm from where the map's symbol does",
                aside.abs(),
                target.code,
                target.name,
                aside.abs()
            ),
        };
        adapted.warnings.push(format!(
            "lines of symbol {} \"{}\" {what}: {n} objects",
            symbol.code(),
            symbol.name(),
        ));
    }
}

/// `coords` moved `by` mm to the right of the line's direction (to its left
/// where negative): each point, curves' control points too, along the normal
/// there, as far again at a corner as keeps the sides `by` apart (up to twice).
pub fn moved_aside(coords: &CoordList, by: f64) -> CoordList {
    let mut out = coords.clone();
    let mut begin = 0;
    for i in 0..coords.len() {
        if !(coords[i].is_hole_point() || i + 1 == coords.len()) {
            continue;
        }
        let part = &coords[begin..=i];
        let n = part.len();
        let closed = n > 2 && part[n - 1].is_close_point();
        // The unit direction from `a` to `b`, if they are apart.
        let unit = |a: &Coord, b: &Coord| {
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let len = dx.hypot(dy);
            (len > 1e-9).then(|| (dx / len, dy / len))
        };
        for j in 0..n {
            // The nearest neighbour apart from it on either side.
            let before = (0..j)
                .rev()
                .find_map(|k| unit(&part[k], &part[j]))
                .or_else(|| {
                    closed
                        .then(|| (0..n - 1).rev().find_map(|k| unit(&part[k], &part[n - 1])))
                        .flatten()
                });
            let after = (j + 1..n)
                .find_map(|k| unit(&part[j], &part[k]))
                .or_else(|| {
                    closed
                        .then(|| (1..n).find_map(|k| unit(&part[0], &part[k])))
                        .flatten()
                });
            let (tx, ty, stretch) = match (before, after) {
                (Some(a), Some(b)) => {
                    let (sx, sy) = (a.0 + b.0, a.1 + b.1);
                    let len = sx.hypot(sy);
                    if len < 1e-9 {
                        (b.0, b.1, 1.0)
                    } else {
                        // cos of half the turn: the sides of a corner are `by`
                        // apart when its point moves by / cos.
                        let cos = (len / 2.0).max(0.5);
                        (sx / len, sy / len, 1.0 / cos)
                    }
                }
                (Some(t), None) | (None, Some(t)) => (t.0, t.1, 1.0),
                (None, None) => continue,
            };
            // Paper `y` grows down: the right of (tx, ty) is (-ty, tx).
            out[begin + j].x += -ty * by * stretch;
            out[begin + j].y += tx * by * stretch;
        }
        begin = i + 1;
    }
    out
}

/// Puts an area under the lines whose symbol paints its middle where the
/// standard's leaves it empty.
fn fill_under(map: &Map, table: &SymbolTable, adapted: &mut Adapted) {
    // For each symbol: the area to put under its lines, and how wide and how
    // ended they are.
    let fills: Vec<Option<(u16, f64, i32)>> = map
        .symbols
        .iter()
        .map(|symbol| {
            let entry = table.get(line_target(symbol, map, table)?)?;
            if entry.body {
                return None;
            }
            let (color, width, cap) = body(symbol, map)?;
            // A footprint of the standard's of a like width (its widest may be
            // narrower than a map's widest): the middle it leaves empty is the
            // one the map's symbol fills.
            if entry.width < 0.5 * width {
                return None;
            }
            let rgb = map.color(color)?.rgb;
            Some((table.closest_plain_area(rgb)?, width, cap))
        })
        .collect();
    let mut filled = vec![0usize; map.symbols.len()];
    for (index, object) in map.objects.iter().enumerate() {
        let (Some(s), ObjectKind::Path(_)) = (object.symbol_index, &object.kind) else {
            continue;
        };
        let Some((area, width, cap)) = fills[s] else {
            continue;
        };
        let coords = adapted.coords.get(&index).unwrap_or(&object.coords);
        let Some(extra) = footprint_object(object, s, coords, width, cap) else {
            continue;
        };
        adapted.extras.push((extra, area));
        filled[s] += 1;
    }
    for (s, n) in filled.into_iter().enumerate() {
        let Some((area, _, _)) = fills[s].filter(|_| n > 0) else {
            continue;
        };
        let symbol = &map.symbols[s];
        let target = line_target(symbol, map, table)
            .and_then(|id| table.get(id))
            .expect("a symbol filled under has a target");
        let area = table.get(area).expect("an id of the table");
        adapted.warnings.push(format!(
            "lines of symbol {} \"{}\" filled under with {} \"{}\": {} \"{}\" leaves the middle \
             the map's symbol paints empty: {n} objects",
            symbol.code(),
            symbol.name(),
            area.code,
            area.name,
            target.code,
            target.name,
        ));
    }
}

/// Stores as the area of their footprint the lines whose symbol paints only its
/// middle where the standard's, of the same colour, outlines it.
fn make_areas(map: &Map, table: &SymbolTable, adapted: &mut Adapted) {
    // Out of 1 in each of red, green and blue, as for the plain areas.
    const SAME: f32 = 0.1;
    let distance = |a: (f32, f32, f32), b: (f32, f32, f32)| {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
    };
    // For each symbol: the area its lines become, and how wide and how ended
    // they are.
    let areas: Vec<Option<(u16, f64, i32)>> = map
        .symbols
        .iter()
        .map(|symbol| {
            let entry = table.get(line_target(symbol, map, table)?)?;
            if !entry.edged || edged(symbol, map) || spaced(symbol, map) {
                return None;
            }
            let (color, width, cap) = body(symbol, map)?;
            let rgb = map.color(color)?.rgb;
            // The same thing, outlined: a middle of the same colour.
            if distance(entry.body_rgb?, rgb) > SAME {
                return None;
            }
            Some((table.closest_plain_area(rgb)?, width, cap))
        })
        .collect();
    let mut made = vec![0usize; map.symbols.len()];
    for (index, object) in map.objects.iter().enumerate() {
        let (Some(s), ObjectKind::Path(_)) = (object.symbol_index, &object.kind) else {
            continue;
        };
        let Some((area, width, cap)) = areas[s] else {
            continue;
        };
        let coords = adapted.coords.get(&index).unwrap_or(&object.coords);
        let extra = footprint_object(object, s, coords, width, cap);
        // The line itself is not stored: the area is what the map drew.
        adapted.coords.insert(index, CoordList::new());
        if let Some(extra) = extra {
            adapted.extras.push((extra, area));
        }
        made[s] += 1;
    }
    for (s, n) in made.into_iter().enumerate() {
        let Some((area, _, _)) = areas[s].filter(|_| n > 0) else {
            continue;
        };
        let symbol = &map.symbols[s];
        let target = line_target(symbol, map, table)
            .and_then(|id| table.get(id))
            .expect("a symbol made an area has a target");
        let area = table.get(area).expect("an id of the table");
        adapted.warnings.push(format!(
            "lines of symbol {} \"{}\" stored as areas of {} \"{}\": {} \"{}\" outlines the \
             line, which the map's symbol does not: {n} objects",
            symbol.code(),
            symbol.name(),
            area.code,
            area.name,
            target.code,
            target.name,
        ));
    }
}

/// Stores the points whose symbol the standard has as an area only as the
/// smallest square of that area, round each point. Squares of the same area
/// which overlap (single dots drawn close together, as a map draws an area of
/// them) are one area: as dense as the standard draws it, not three dots for
/// each of the map's.
fn points_to_areas(map: &Map, table: &SymbolTable, adapted: &mut Adapted) {
    let areas: Vec<Option<(u16, f64)>> = map
        .symbols
        .iter()
        .map(|symbol| {
            let (id, _) = table.area_for_point(symbol, map)?;
            Some((id, table.get(id)?.least_side?))
        })
        .collect();
    // By the area they become: the points (index of the object, where).
    let mut points: BTreeMap<u16, Vec<(usize, Coord)>> = BTreeMap::new();
    let mut made = vec![0usize; map.symbols.len()];
    for (index, object) in map.objects.iter().enumerate() {
        let (Some(s), ObjectKind::Point) = (object.symbol_index, &object.kind) else {
            continue;
        };
        let (Some((area, _)), Some(&at)) = (areas[s], object.coords.first()) else {
            continue;
        };
        points.entry(area).or_default().push((index, at));
        adapted.coords.insert(index, CoordList::new());
        made[s] += 1;
    }
    for (area, points) in points {
        let side = table
            .get(area)
            .and_then(|e| e.least_side)
            .expect("an area points are made into has a side");
        let h = side / 2.0;
        // Which group each point is in: those whose squares overlap, and so on.
        let mut group: Vec<usize> = (0..points.len()).collect();
        fn root(group: &mut [usize], mut i: usize) -> usize {
            while group[i] != i {
                group[i] = group[group[i]];
                i = group[i];
            }
            i
        }
        for i in 0..points.len() {
            for j in i + 1..points.len() {
                let (a, b) = (points[i].1, points[j].1);
                if (a.x - b.x).abs() < side && (a.y - b.y).abs() < side {
                    let (ri, rj) = (root(&mut group, i), root(&mut group, j));
                    group[rj] = ri;
                }
            }
        }
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..points.len() {
            let r = root(&mut group, i);
            groups.entry(r).or_default().push(i);
        }
        for members in groups.values() {
            let squares: Vec<geo::Polygon> = members
                .iter()
                .map(|&i| {
                    let at = points[i].1;
                    geo::Rect::new(
                        GeoCoord { x: at.x - h, y: at.y - h },
                        GeoCoord { x: at.x + h, y: at.y + h },
                    )
                    .to_polygon()
                })
                .collect();
            let (index, first) = points[members[0]];
            let object = &map.objects[index];
            let mut extra = Object::new(ObjectKind::Path(PathObject {
                pattern_rotation: 0.0,
                // The patterns laid out from a point, as the side was worked
                // out: its square shows the fewest dots at least.
                pattern_origin: crate::map::Point::new(first.x, first.y),
            }));
            extra.symbol_id = object.symbol_id;
            extra.symbol_index = object.symbol_index;
            extra.coords = area_coords(&unary_union(squares.iter()));
            adapted.extras.push((extra, area));
        }
    }
    for (s, n) in made.into_iter().enumerate() {
        let Some((area, side)) = areas[s].filter(|_| n > 0) else {
            continue;
        };
        let symbol = &map.symbols[s];
        let area = table.get(area).expect("an id of the table");
        adapted.warnings.push(format!(
            "points of symbol {} \"{}\" stored as areas of {} \"{}\", {side:.2} mm squares, \
             those which overlap as one: {} has no point symbol for them, only the area: {n} \
             objects",
            symbol.code(),
            symbol.name(),
            area.code,
            area.name,
            table.standard,
        ));
    }
}

/// An area of the footprint of the line `coords` (of `object`, of the map's
/// symbol `symbol`), `width` wide and ended by `cap`: `None` for no area.
fn footprint_object(
    object: &Object,
    symbol: usize,
    coords: &CoordList,
    width: f64,
    cap: i32,
) -> Option<Object> {
    let style = match cap {
        cap_style::ROUND => BufferStyle::new(width / 2.0),
        cap_style::SQUARE => BufferStyle::new(width / 2.0).line_cap(LineCap::Square),
        _ => BufferStyle::new(width / 2.0).line_cap(LineCap::Butt),
    };
    let pieces: Vec<MultiPolygon> = flatten(coords)
        .iter()
        .filter(|part| part.points.len() >= 2)
        .map(|part| {
            LineString::from(
                part.points
                    .iter()
                    .map(|p| GeoCoord { x: p.x, y: p.y })
                    .collect::<Vec<_>>(),
            )
            .buffer_with_style(style.clone())
        })
        .collect();
    let footprint = area_coords(&unary_union(pieces.iter()));
    if footprint.is_empty() {
        return None;
    }
    let mut extra = Object::new(ObjectKind::Path(PathObject::default()));
    extra.symbol_id = object.symbol_id;
    extra.symbol_index = Some(symbol);
    extra.coords = footprint;
    Some(extra)
}

/// Whether a line `symbol`, one of `map`'s, draws an outline: a border on
/// either side.
pub(crate) fn edged(symbol: &Symbol, map: &Map) -> bool {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    leaves.iter().any(|leaf| match leaf {
        Symbol::Line(l) => l.border.is_visible() || l.right_border.is_visible(),
        _ => false,
    })
}

/// Whether a line `symbol`, one of `map`'s, lays anything out along it: dashes,
/// or symbols at its ends or along it.
fn spaced(symbol: &Symbol, map: &Map) -> bool {
    crate::mot_symbols::spaces_out(symbol, map, true)
}

/// The middle of a line `symbol`, one of `map`'s, paints: the colour, width and
/// cap of its widest line in a colour. `None` for a symbol which paints none
/// (only borders, dashes or symbols along it).
pub(crate) fn body(symbol: &Symbol, map: &Map) -> Option<(i32, f64, i32)> {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    leaves
        .iter()
        .filter_map(|leaf| match leaf {
            Symbol::Line(l) if l.color >= 0 && l.line_width > 0.0 => {
                Some((l.color, l.line_width, l.cap_style))
            }
            _ => None,
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// Which side of the line `symbol`, one of `map`'s, draws on: in mm, the middle
/// of how far its one-sided parts reach on either side of the line, positive
/// to the right of the line's direction (paper `y` grows down: a line going
/// along `x` has its right at positive `y`), 0 for a symbol drawn alike on both
/// sides.
///
/// Its parts are the symbols along it (start, end, mid and dash symbols, in
/// their own frame, which turns with the line) and its borders. The middle line
/// is the same on both sides and is left out.
pub fn line_side(symbol: &Symbol, map: &Map) -> f64 {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    let mut reach: Option<(f64, f64)> = None;
    let mut add = |lo: f64, hi: f64| {
        reach = Some(match reach {
            Some((a, b)) => (a.min(lo), b.max(hi)),
            None => (lo, hi),
        });
    };
    for leaf in leaves {
        let Symbol::Line(l) = leaf else { continue };
        let half = l.line_width / 2.0;
        // The first border is on the left, the second on the right.
        if l.border.is_visible() {
            let b = &l.border;
            add(
                -(half + b.shift + b.width / 2.0),
                -(half + b.shift - b.width / 2.0),
            );
        }
        if l.right_border.is_visible() {
            let b = &l.right_border;
            add(
                half + b.shift - b.width / 2.0,
                half + b.shift + b.width / 2.0,
            );
        }
        for point in [
            &l.start_symbol,
            &l.end_symbol,
            &l.mid_symbol,
            &l.dash_symbol,
        ]
        .into_iter()
        .flatten()
        {
            if let Some((lo, hi)) = point_reach(point) {
                add(lo, hi);
            }
        }
    }
    reach.map_or(0.0, |(lo, hi)| (lo + hi) / 2.0)
}

/// How far a point symbol reaches across `y`, in its own frame: `(lowest,
/// highest)`, `None` where it draws nothing.
fn point_reach(point: &PointSymbol) -> Option<(f64, f64)> {
    let mut reach: Option<(f64, f64)> = None;
    let mut add = |lo: f64, hi: f64| {
        reach = Some(match reach {
            Some((a, b)) => (a.min(lo), b.max(hi)),
            None => (lo, hi),
        });
    };
    let circle = point.inner_radius + point.outer_width;
    if (point.inner_color >= 0 || point.outer_color >= 0) && circle > 0.0 {
        add(-circle, circle);
    }
    for element in &point.elements {
        let ys = element.object.coords.iter().map(|c| c.y);
        let (Some(lo), Some(hi)) = (ys.clone().reduce(f64::min), ys.reduce(f64::max)) else {
            continue;
        };
        let grow = match &element.symbol {
            Symbol::Line(l) => l.line_width / 2.0,
            Symbol::Point(p) => p.inner_radius + p.outer_width,
            _ => 0.0,
        };
        add(lo - grow, hi + grow);
    }
    reach
}

/// `coords` the other way round: each part reversed, curves and all, so that the
/// line is the same but goes the other way.
pub fn reversed(coords: &CoordList) -> CoordList {
    let mut out = CoordList::with_capacity(coords.len());
    let mut begin = 0;
    for i in 0..coords.len() {
        if !(coords[i].is_hole_point() || i + 1 == coords.len()) {
            continue;
        }
        let part = &coords[begin..=i];
        // What stays with each point (a dash or a gap is where it is), and a
        // curve's start moving to the end it had: from there it now starts.
        let kept = coord_flag::DASH_POINT | coord_flag::GAP_POINT;
        let mut flags: Vec<i32> = part.iter().map(|c| c.flags & kept).collect();
        for (j, c) in part.iter().enumerate() {
            if c.is_curve_start() && j + 3 < part.len() {
                flags[j + 3] |= coord_flag::CURVE_START;
            }
        }
        // The part's end flags (closed, another part after it) stay at its end.
        let ends = part[part.len() - 1].flags & (coord_flag::CLOSE_POINT | coord_flag::HOLE_POINT);
        let n = out.len();
        for (j, c) in part.iter().enumerate().rev() {
            out.push(Coord::new(c.x, c.y, flags[j]));
        }
        let last = out.len() - 1;
        if last >= n {
            out[last].flags |= ends;
        }
        begin = i + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_reader::read_xml_map_str;

    /// A map of one colour and these symbols, as the XML of each `<symbol>`'s
    /// inside (with `{id}` for its id), and objects (symbol index, coordinates
    /// in mm).
    fn map(symbols: &[&str], objects: &[(usize, &[(f64, f64)])]) -> Map {
        let colors = r#"<colors count="2"><color priority="0" name="black" c="0" m="0" y="0" k="1" opacity="1"/><color priority="1" name="paved" c="0" m="0.168" y="0.3" k="0.054" opacity="1"/></colors>"#;
        let mut xml = format!("<map>{colors}<symbols count=\"{}\">", symbols.len());
        for (id, inside) in symbols.iter().enumerate() {
            xml += &inside.replace("{id}", &id.to_string());
        }
        xml += &format!(
            r#"</symbols><parts count="1" current="0"><part name="default"><objects count="{}">"#,
            objects.len()
        );
        for (symbol, points) in objects {
            let coords: Vec<String> = points
                .iter()
                .map(|(x, y)| format!("{} {}", (x * 1000.0) as i64, (y * 1000.0) as i64))
                .collect();
            xml += &format!(
                r#"<object type="1" symbol="{symbol}"><coords count="{}">{};</coords></object>"#,
                points.len(),
                coords.join(";")
            );
        }
        xml += "</objects></part></parts></map>";
        read_xml_map_str(&xml).unwrap().0
    }

    /// A line of no width with a filled triangle along it, its corners at
    /// `corners` (in 1/1000 mm, as a file has them).
    fn triangles(code: &str, corners: &str) -> String {
        format!(
            r#"<symbol type="2" id="{{id}}" code="{code}" name="s{{id}}"><line_symbol color="0" line_width="0" minimum_length="0" join_style="1" cap_style="0" start_offset="0" end_offset="0" segment_length="650" end_length="350" show_at_least_one_symbol="true" minimum_mid_symbol_count="0" minimum_mid_symbol_count_when_closed="0" dash_length="4000" break_length="1000" dashes_in_group="1" in_group_break_length="500" mid_symbols_per_spot="1" mid_symbol_distance="0"><mid_symbol><symbol type="1" code="" name="m"><point_symbol rotatable="true" inner_radius="1000" inner_color="-1" outer_width="0" outer_color="-1" elements="1"><element><symbol type="4" code=""><area_symbol inner_color="0" min_area="0" patterns="0"/></symbol><object type="1"><coords count="3">{corners}</coords></object></element></point_symbol></symbol></mid_symbol></line_symbol></symbol>"#
        )
    }

    /// A stairway: a line of `color` (-1 for none) with steps across it.
    fn stairway(code: &str, color: i32) -> String {
        format!(
            r#"<symbol type="2" id="{{id}}" code="{code}" name="s{{id}}"><line_symbol color="{color}" line_width="400" minimum_length="0" join_style="1" cap_style="0" start_offset="0" end_offset="0" segment_length="500" end_length="35" show_at_least_one_symbol="true" minimum_mid_symbol_count="0" minimum_mid_symbol_count_when_closed="0" dash_length="4000" break_length="1000" dashes_in_group="1" in_group_break_length="500" mid_symbols_per_spot="1" mid_symbol_distance="0"><mid_symbol><symbol type="1" code="" name="m"><point_symbol rotatable="true" inner_radius="1000" inner_color="-1" outer_width="0" outer_color="-1" elements="1"><element><symbol type="2" code=""><line_symbol color="0" line_width="100" minimum_length="0" join_style="1" cap_style="0" start_offset="0" end_offset="0" segment_length="4000" end_length="0" dash_length="4000" break_length="1000" dashes_in_group="1" in_group_break_length="500" mid_symbols_per_spot="1" mid_symbol_distance="0"/></symbol><object type="1"><coords count="2">0 220;0 -220;</coords></object></element></point_symbol></symbol></mid_symbol><borders><border color="0" width="100" shift="50"/></borders></line_symbol></symbol>"#
        )
    }

    #[test]
    fn a_symbol_drawn_on_one_side_has_that_side() {
        // Apex to the left of the line (negative y), base across it.
        let left = map(&[&triangles("512.1.2", "350 200;0 -400;-350 200;")], &[]);
        assert!(line_side(&left.symbols[0], &left) < -SIDED);
        // ISSprOM's: base on the line, apex to the right.
        let table = SymbolTable::issprom();
        let entry = table.get(table.id_of("512.1").unwrap()).unwrap();
        assert!(entry.side > SIDED, "{}", entry.side);
        // Steps across a stairway reach as far on both sides.
        let stairs = map(&[&stairway("532.6", 1)], &[]);
        assert!(line_side(&stairs.symbols[0], &stairs).abs() < 1e-9);
    }

    #[test]
    fn a_line_drawn_on_the_other_side_from_the_standard_is_reversed() {
        let m = map(
            &[&triangles("512.1", "350 200;0 -400;-350 200;")],
            &[(0, &[(0.0, 0.0), (5.0, 0.0), (10.0, 2.0)])],
        );
        let adapted = adapt(&m, &SymbolTable::issprom());
        let back = &adapted.coords[&0];
        // The other way round (and moved a little aside, see below).
        let xs: Vec<f64> = back.iter().map(|c| c.x).collect();
        assert!(
            (xs[0] - 10.0).abs() < 0.3 && (xs[1] - 5.0).abs() < 0.3 && (xs[2] - 0.0).abs() < 0.3,
            "{xs:?}"
        );
        assert!(
            adapted.warnings.iter().any(|w| w.contains("reversed")),
            "{:?}",
            adapted.warnings
        );
    }

    #[test]
    fn triangles_straddling_the_line_are_moved_to_stand_where_they_were() {
        // The map's triangles reach from 0.2 right of the line to 0.4 left of
        // it (their middle 0.1 left), ISSprOM's from the line to 0.606 right of
        // it (0.303 right). Reversed, the map's middle is 0.1 right of the new
        // direction: the line moves 0.1 - 0.303 = -0.203 to its right, which is
        // 0.203 to its left.
        let m = map(
            &[&triangles("512.1", "350 200;0 -400;-350 200;")],
            &[(0, &[(0.0, 0.0), (10.0, 0.0)])],
        );
        let adapted = adapt(&m, &SymbolTable::issprom());
        let moved = &adapted.coords[&0];
        assert_eq!(moved[0].x, 10.0);
        // Going towards -x, the left is +y.
        assert!((moved[0].y - 0.203).abs() < 1e-3, "{}", moved[0].y);
        assert!((moved[1].y - 0.203).abs() < 1e-3, "{}", moved[1].y);
        assert!(
            adapted.warnings.iter().any(|w| w.contains("moved 0.20 mm")),
            "{:?}",
            adapted.warnings
        );
    }

    #[test]
    fn a_line_moved_aside_keeps_its_corners_apart() {
        let c = |x: f64, y: f64| Coord::new(x, y, 0);
        // Right along x, then down along y: the right is +y, then -x.
        let moved = moved_aside(&vec![c(0.0, 0.0), c(10.0, 0.0), c(10.0, 10.0)], 1.0);
        let got: Vec<(f64, f64)> = moved.iter().map(|c| (c.x, c.y)).collect();
        let near =
            |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
        assert!(near(got[0], (0.0, 1.0)), "{got:?}");
        assert!(near(got[1], (9.0, 1.0)), "{got:?}");
        assert!(near(got[2], (9.0, 10.0)), "{got:?}");
    }

    #[test]
    fn a_line_drawn_on_the_standards_side_is_left_as_it_is() {
        let m = map(
            &[&triangles("512.1", "-350 0;350 0;0 606;")],
            &[(0, &[(0.0, 0.0), (10.0, 0.0)])],
        );
        assert!(adapt(&m, &SymbolTable::issprom()).coords.is_empty());
    }

    #[test]
    fn a_reversed_curve_is_the_same_curve() {
        let c = |x: f64, y: f64, flags: i32| Coord::new(x, y, flags);
        let coords = vec![
            c(0.0, 0.0, coord_flag::CURVE_START),
            c(1.0, 1.0, 0),
            c(2.0, 1.0, 0),
            c(3.0, 0.0, coord_flag::DASH_POINT),
            c(4.0, 0.0, coord_flag::HOLE_POINT),
            c(10.0, 0.0, 0),
            c(11.0, 0.0, 0),
        ];
        let back = reversed(&coords);
        let got: Vec<(f64, i32)> = back.iter().map(|c| (c.x, c.flags)).collect();
        assert_eq!(
            got,
            vec![
                (4.0, 0),
                (3.0, coord_flag::CURVE_START | coord_flag::DASH_POINT),
                (2.0, 0),
                (1.0, 0),
                (0.0, coord_flag::HOLE_POINT),
                (11.0, 0),
                (10.0, 0),
            ]
        );
        assert_eq!(reversed(&back), coords);
    }

    #[test]
    fn a_stairway_the_map_fills_gets_a_paved_area_under_it() {
        // The map's stairway is filled with a paved brown; ISSprOM's leaves the
        // middle empty.
        let m = map(&[&stairway("532.6", 1)], &[(0, &[(0.0, 0.0), (10.0, 0.0)])]);
        let table = SymbolTable::issprom();
        let adapted = adapt(&m, &table);
        assert_eq!(adapted.extras.len(), 1, "{:?}", adapted.warnings);
        let (area, id) = &adapted.extras[0];
        assert!(
            table.get(*id).unwrap().code.starts_with("501"),
            "{}",
            table.get(*id).unwrap().code
        );
        // 10 long and 0.4 wide, ends flat.
        let shape = crate::mot_cut::area_shape(&area.coords);
        use geo::Area;
        assert!(
            (shape.unsigned_area() - 4.0).abs() < 1e-6,
            "{}",
            shape.unsigned_area()
        );
    }

    /// A plain line of `color`, 0.35 wide, with a border of `border` (-1 for
    /// none).
    fn road(code: &str, color: i32, border: i32) -> String {
        let borders = if border >= 0 {
            format!(r#"<borders><border color="{border}" width="100" shift="50"/></borders>"#)
        } else {
            String::new()
        };
        format!(
            r#"<symbol type="2" id="{{id}}" code="{code}" name="s{{id}}"><line_symbol color="{color}" line_width="350" minimum_length="0" join_style="1" cap_style="0" start_offset="0" end_offset="0" segment_length="4000" end_length="0" dash_length="4000" break_length="1000" dashes_in_group="1" in_group_break_length="500" mid_symbols_per_spot="1" mid_symbol_distance="0">{borders}</line_symbol></symbol>"#
        )
    }

    #[test]
    fn a_road_the_map_draws_without_an_outline_is_stored_as_an_area() {
        // ISSprOM's 501.6 is a brown footprint with black borders; the map's
        // is the brown alone.
        let m = map(&[&road("501.6", 1, -1)], &[(0, &[(0.0, 0.0), (10.0, 0.0)])]);
        let table = SymbolTable::issprom();
        let adapted = adapt(&m, &table);
        assert!(adapted.coords[&0].is_empty(), "the line is not stored");
        assert_eq!(adapted.extras.len(), 1, "{:?}", adapted.warnings);
        let (area, id) = &adapted.extras[0];
        assert_eq!(table.get(*id).unwrap().code, "501.5");
        let shape = crate::mot_cut::area_shape(&area.coords);
        use geo::Area;
        assert!(
            (shape.unsigned_area() - 3.5).abs() < 1e-6,
            "{}",
            shape.unsigned_area()
        );
        assert!(adapted
            .warnings
            .iter()
            .any(|w| w.contains("stored as areas")));
    }

    #[test]
    fn a_road_the_map_outlines_too_stays_a_line() {
        let m = map(&[&road("501.6", 1, 0)], &[(0, &[(0.0, 0.0), (10.0, 0.0)])]);
        let adapted = adapt(&m, &SymbolTable::issprom());
        assert!(adapted.coords.is_empty() && adapted.extras.is_empty());
    }

    #[test]
    fn a_line_of_another_colour_than_the_standards_stays_a_line() {
        // Black, where 501.6's middle is brown: not the same thing outlined.
        let m = map(&[&road("501.6", 0, -1)], &[(0, &[(0.0, 0.0), (10.0, 0.0)])]);
        let adapted = adapt(&m, &SymbolTable::issprom());
        assert!(adapted.coords.is_empty() && adapted.extras.is_empty());
    }

    #[test]
    fn a_stairway_the_map_leaves_empty_gets_nothing_under_it() {
        let m = map(
            &[&stairway("532.6", -1)],
            &[(0, &[(0.0, 0.0), (10.0, 0.0)])],
        );
        assert!(adapt(&m, &SymbolTable::issprom()).extras.is_empty());
    }

    #[test]
    fn a_single_dot_the_standard_has_as_an_area_only_is_the_smallest_area() {
        let dot = r#"<symbol type="1" id="{id}" code="113.1" name="Broken ground, single dot"><point_symbol inner_radius="150" inner_color="0" outer_width="0" outer_color="-1" elements="0"/></symbol>"#;
        let mut m = map(&[dot], &[]);
        let mut point = Object::new(ObjectKind::Point);
        point.symbol_index = Some(0);
        point.coords = vec![Coord::new(5.0, 7.0, 0)];
        m.objects.push(point);
        let table = SymbolTable::issprom();
        assert!(table.closest(&m.symbols[0], &m).is_none());
        let adapted = adapt(&m, &table);
        assert!(adapted.coords[&0].is_empty(), "the point is not stored");
        assert_eq!(adapted.extras.len(), 1, "{:?}", adapted.warnings);
        let (area, id) = &adapted.extras[0];
        let entry = table.get(*id).unwrap();
        assert_eq!(entry.code, "113");
        let side = entry.least_side.unwrap();
        assert!(side > 0.3 && side < 1.5, "{side}");
        let xs: Vec<f64> = area.coords.iter().map(|c| c.x).collect();
        assert!((xs.iter().cloned().fold(f64::MIN, f64::max) - (5.0 + side / 2.0)).abs() < 1e-9);
        assert!(adapted
            .warnings
            .iter()
            .any(|w| w.contains("stored as areas of 113")));
    }

    #[test]
    fn single_dots_drawn_close_together_are_one_area() {
        let dot = r#"<symbol type="1" id="{id}" code="210.1" name="Stony ground, single dot"><point_symbol inner_radius="125" inner_color="0" outer_width="0" outer_color="-1" elements="0"/></symbol>"#;
        let mut m = map(&[dot], &[]);
        for (x, y) in [(0.0, 0.0), (0.3, 0.1), (5.0, 5.0)] {
            let mut point = Object::new(ObjectKind::Point);
            point.symbol_index = Some(0);
            point.coords = vec![Coord::new(x, y, 0)];
            m.objects.push(point);
        }
        let adapted = adapt(&m, &SymbolTable::issprom());
        assert_eq!(adapted.extras.len(), 2, "{:?}", adapted.warnings);
        assert!((0..3).all(|i| adapted.coords[&i].is_empty()));
    }

    #[test]
    fn the_dotted_areas_have_a_smallest_square() {
        let table = SymbolTable::issprom();
        for code in ["113", "210"] {
            let side = table.get(table.id_of(code).unwrap()).unwrap().least_side;
            eprintln!("{code}: {side:?}");
            assert!(side.is_some_and(|s| s < 1.5), "{code}: {side:?}");
        }
    }
}
