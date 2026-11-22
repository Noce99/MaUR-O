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
    let reverse: Vec<bool> = map
        .symbols
        .iter()
        .map(|symbol| {
            line_target(symbol, map, table)
                .and_then(|id| table.get(id))
                .is_some_and(|entry| {
                    let own = line_side(symbol, map);
                    own.abs() >= SIDED && entry.side.abs() >= SIDED && own * entry.side < 0.0
                })
        })
        .collect();
    for (index, object) in map.objects.iter().enumerate() {
        let (Some(s), ObjectKind::Path(_)) = (object.symbol_index, &object.kind) else {
            continue;
        };
        if !reverse[s] {
            continue;
        }
        let coords = adapted.coords.get(&index).unwrap_or(&object.coords);
        if coords.len() < 2 {
            continue;
        }
        let back = reversed(coords);
        adapted.coords.insert(index, back);
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
        adapted.warnings.push(format!(
            "lines of symbol {} \"{}\" reversed: {} \"{}\" draws on the other side of the line: \
             {n} objects",
            symbol.code(),
            symbol.name(),
            target.code,
            target.name,
        ));
    }
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
            continue;
        }
        let mut extra = Object::new(ObjectKind::Path(PathObject::default()));
        extra.symbol_id = object.symbol_id;
        extra.symbol_index = Some(s);
        extra.coords = footprint;
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
        let xs: Vec<f64> = back.iter().map(|c| c.x).collect();
        assert_eq!(xs, vec![10.0, 5.0, 0.0]);
        assert!(
            adapted.warnings.iter().any(|w| w.contains("reversed")),
            "{:?}",
            adapted.warnings
        );
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

    #[test]
    fn a_stairway_the_map_leaves_empty_gets_nothing_under_it() {
        let m = map(
            &[&stairway("532.6", -1)],
            &[(0, &[(0.0, 0.0), (10.0, 0.0)])],
        );
        assert!(adapt(&m, &SymbolTable::issprom()).extras.is_empty());
    }
}
