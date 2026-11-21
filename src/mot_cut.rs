//! Keeps visible what a map draws over one of its areas when the standard it is
//! stored in draws it under that area.
//!
//! Mapper draws by colour: a colour higher in the table covers every lower one.
//! A standard's colour order is not every map's. A club's map may draw a planter
//! or a hedge on top of a paved area, with the greens above the paved browns,
//! where ISSprOM puts the paved browns above the greens: stored as is, the
//! planter is drawn and then covered. A map drawn to the standard would cut the
//! paved area round it, so that is what is done here: an area is cut by the
//! footprint of every object that the map draws above it and the standard draws
//! below it. An area's footprint is the area; a line's, the line at its width.
//! Points cut nothing, and nor do objects of fills that are only patterns.
//!
//! Two kinds of area are left whole. A course symbol (700s), which the standard
//! means to cover the map. And, by a line, an area that draws its own outline (a
//! building, a paved area with its border): a hedge along a building's wall would
//! notch it and outline the notch, where the map drew the hedge over its edge.

use std::collections::{BTreeMap, BTreeSet};

use geo::algorithm::bool_ops::{unary_union, FillRule};
use geo::algorithm::buffer::{BufferStyle, LineCap};
use geo::{
    Area, BooleanOps, BoundingRect, Buffer, Coord as GeoCoord, LineString, MultiPolygon, Polygon,
};

use crate::geometry::flatten;
use crate::map::{cap_style, coord_flag, symbol_leaves, Coord, CoordList, Map, ObjectKind, Symbol};
use crate::mot_symbols::{widest_line, SymbolKind, SymbolTable};

/// The priority of the topmost colour an area of `symbol`, one of `map`'s, is
/// filled with (lower is drawn on top); `None` for a symbol with no plain fill.
pub(crate) fn fill_priority(symbol: &Symbol, map: &Map) -> Option<i32> {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    leaves
        .iter()
        .filter_map(|leaf| match leaf {
            Symbol::Area(a) if a.color >= 0 => Some(a.color),
            _ => None,
        })
        .min()
}

/// The priority of the topmost colour `symbol`, one of `map`'s, paints solid: an
/// area's plain fill or a line. `None` where it paints only patterns or points.
pub(crate) fn paint_priority(symbol: &Symbol, map: &Map) -> Option<i32> {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    leaves
        .iter()
        .filter_map(|leaf| match leaf {
            Symbol::Area(a) if a.color >= 0 => Some(a.color),
            Symbol::Line(l) if l.color >= 0 && l.line_width > 0.0 => Some(l.color),
            _ => None,
        })
        .min()
}

/// Whether `symbol`, one of `map`'s, draws any line.
pub(crate) fn draws_a_line(symbol: &Symbol, map: &Map) -> bool {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    leaves
        .iter()
        .any(|leaf| matches!(leaf, Symbol::Line(l) if l.color >= 0 && l.line_width > 0.0))
}

/// What [`cut_hidden`] found.
#[derive(Default)]
pub struct Cuts {
    /// The new coordinates of the areas cut, by index in [`Map::objects`]: empty
    /// for one covered whole.
    pub coords: BTreeMap<usize, CoordList>,
    /// One line per symbol whose areas were cut.
    pub warnings: Vec<String>,
}

/// Where a symbol stands in both colour orders.
struct Order {
    /// In the map.
    own: i32,
    /// In the standard.
    standard: i32,
}

/// The areas of `map` to cut so that, stored in `table`'s standard, they do not
/// cover what the map draws above them. See the module's documentation.
pub fn cut_hidden(map: &Map, table: &SymbolTable) -> Cuts {
    // What each symbol fills (as an area to cut, and whether it is outlined) and
    // paints (as what cuts).
    let mut fills: Vec<Option<(Order, bool)>> = Vec::with_capacity(map.symbols.len());
    let mut paints: Vec<Option<(Order, SymbolKind)>> = Vec::with_capacity(map.symbols.len());
    for symbol in &map.symbols {
        let entry = symbol
            .is_visible()
            .then(|| table.closest(symbol, map))
            .flatten()
            .and_then(|(id, _)| table.get(id));
        let Some(entry) = entry else {
            fills.push(None);
            paints.push(None);
            continue;
        };
        let course = entry.code.starts_with('7');
        fills.push(match (entry.kind, fill_priority(symbol, map), entry.fill) {
            (SymbolKind::Area, Some(own), Some(standard)) if !course => {
                Some((Order { own, standard }, entry.outlined))
            }
            _ => None,
        });
        paints.push(
            match (entry.kind, paint_priority(symbol, map), entry.paint) {
                (SymbolKind::Area | SymbolKind::Line, Some(own), Some(standard)) => {
                    Some((Order { own, standard }, entry.kind))
                }
                _ => None,
            },
        );
    }
    // For each symbol filling areas, the symbols the map draws above it which the
    // standard draws below it.
    let mut under: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (a, fill) in fills.iter().enumerate() {
        let Some((fill, outlined)) = fill else {
            continue;
        };
        for (b, paint) in paints.iter().enumerate() {
            let Some((paint, kind)) = paint else { continue };
            if *outlined && *kind == SymbolKind::Line {
                continue;
            }
            if a != b && fill.standard < paint.standard && fill.own > paint.own {
                under.entry(a).or_default().insert(b);
            }
        }
    }
    let mut cuts = Cuts::default();
    if under.is_empty() {
        return cuts;
    }
    let cutting: BTreeSet<usize> = under.values().flatten().copied().collect();

    // The footprint of every object that may cut, with its box.
    let mut footprints: Vec<(usize, geo::Rect, MultiPolygon)> = Vec::new();
    for object in &map.objects {
        let (Some(s), ObjectKind::Path(_)) = (object.symbol_index, &object.kind) else {
            continue;
        };
        if !cutting.contains(&s) || object.coords.is_empty() {
            continue;
        }
        let Some((_, kind)) = &paints[s] else {
            continue;
        };
        let shape = match kind {
            SymbolKind::Area => area_shape(&object.coords),
            _ => {
                let half = widest_line(&map.symbols[s], map) / 2.0;
                if half <= 0.0 {
                    continue;
                }
                // The line's own ends: past them it covers nothing.
                let style = match line_cap(&map.symbols[s], map) {
                    cap_style::ROUND => BufferStyle::new(half),
                    cap_style::SQUARE => BufferStyle::new(half).line_cap(LineCap::Square),
                    _ => BufferStyle::new(half).line_cap(LineCap::Butt),
                };
                let lines: Vec<MultiPolygon> = flatten(&object.coords)
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
                unary_union(lines.iter())
            }
        };
        if let Some(rect) = shape.bounding_rect() {
            footprints.push((s, rect, shape));
        }
    }

    // Each area cut by what lies over it.
    let mut cut_by: BTreeMap<usize, (usize, BTreeSet<usize>)> = BTreeMap::new();
    for (index, object) in map.objects.iter().enumerate() {
        let (Some(a), ObjectKind::Path(_)) = (object.symbol_index, &object.kind) else {
            continue;
        };
        let Some(over) = under.get(&a) else { continue };
        if object.coords.is_empty() {
            continue;
        }
        let shape = area_shape(&object.coords);
        let Some(rect) = shape.bounding_rect() else {
            continue;
        };
        let hits: Vec<&(usize, geo::Rect, MultiPolygon)> = footprints
            .iter()
            .filter(|(s, r, _)| over.contains(s) && overlap(r, &rect))
            .collect();
        if hits.is_empty() {
            continue;
        }
        let clip = unary_union(hits.iter().map(|(_, _, f)| f));
        let rest = shape.difference(&clip);
        let before = shape.unsigned_area();
        if (before - rest.unsigned_area()).abs() <= before * 1e-9 + 1e-9 {
            continue;
        }
        cuts.coords.insert(index, area_coords(&rest));
        let entry = cut_by.entry(a).or_default();
        entry.0 += 1;
        entry.1.extend(hits.iter().map(|(s, _, _)| *s));
    }
    for (a, (count, by)) in cut_by {
        let by: Vec<String> = by
            .iter()
            .map(|&b| format!("{} \"{}\"", map.symbols[b].code(), map.symbols[b].name()))
            .collect();
        cuts.warnings.push(format!(
            "areas of symbol {} \"{}\" cut where {} lie on them (drawn above them in the map, below \
             them in {}): {count} objects",
            map.symbols[a].code(),
            map.symbols[a].name(),
            by.join(", "),
            table.standard,
        ));
    }
    cuts
}

/// How the widest line of `symbol`, one of `map`'s, ends: a [`cap_style`].
fn line_cap(symbol: &Symbol, map: &Map) -> i32 {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    leaves
        .iter()
        .filter_map(|leaf| match leaf {
            Symbol::Line(l) => Some(l),
            _ => None,
        })
        .max_by(|a, b| a.line_width.total_cmp(&b.line_width))
        .map_or(cap_style::FLAT, |l| l.cap_style)
}

fn overlap(a: &geo::Rect, b: &geo::Rect) -> bool {
    a.min().x <= b.max().x
        && b.min().x <= a.max().x
        && a.min().y <= b.max().y
        && b.min().y <= a.max().y
}

/// An area's coordinates as a valid shape: its parts, curves flattened, filled
/// even-odd as Mapper fills them.
fn area_shape(coords: &CoordList) -> MultiPolygon {
    let rings: Vec<Polygon> = flatten(coords)
        .iter()
        .filter(|part| part.points.len() >= 3)
        .map(|part| {
            Polygon::new(
                LineString::from(
                    part.points
                        .iter()
                        .map(|p| GeoCoord { x: p.x, y: p.y })
                        .collect::<Vec<_>>(),
                ),
                vec![],
            )
        })
        .collect();
    MultiPolygon::new(rings).union_with_fill_rule(&MultiPolygon::new(vec![]), FillRule::EvenOdd)
}

/// A shape as an area's coordinates: each ring a closed part, the parts after the
/// first holes (filled even-odd, which a shape's rings are).
fn area_coords(shape: &MultiPolygon) -> CoordList {
    let rings: Vec<&LineString> = shape
        .0
        .iter()
        .flat_map(|p| std::iter::once(p.exterior()).chain(p.interiors()))
        .filter(|r| r.0.len() >= 4)
        .collect();
    let mut coords = CoordList::new();
    for (i, ring) in rings.iter().enumerate() {
        let last = ring.0.len() - 1;
        for (j, c) in ring.0.iter().enumerate() {
            let mut flags = 0;
            if j == last {
                flags |= coord_flag::CLOSE_POINT;
                if i + 1 < rings.len() {
                    flags |= coord_flag::HOLE_POINT;
                }
            }
            coords.push(Coord::new(c.x, c.y, flags));
        }
    }
    coords
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_reader::read_xml_map_str;

    /// A map with two colours, `top` drawn above `bottom`, and these symbols
    /// (code, kind and colour: `top` or `bottom`) and objects (symbol index and
    /// coordinates in mm, a closed ring for an area).
    fn map(symbols: &[(&str, &str, &str)], objects: &[(usize, &[(f64, f64)])]) -> Map {
        let colors = r#"<colors count="2"><color priority="0" name="top" c="0.8" m="0" y="1" k="0" opacity="1"/><color priority="1" name="bottom" c="0" m="0.17" y="0.3" k="0.05" opacity="1"/></colors>"#;
        let mut xml = format!("<map>{colors}<symbols count=\"{}\">", symbols.len());
        for (id, (code, kind, color)) in symbols.iter().enumerate() {
            let c = if *color == "top" { 0 } else { 1 };
            xml += &match *kind {
                "area" => format!(
                    r#"<symbol type="4" id="{id}" code="{code}" name="s{id}"><area_symbol inner_color="{c}" min_area="0" patterns="0"/></symbol>"#
                ),
                _ => format!(
                    r#"<symbol type="2" id="{id}" code="{code}" name="s{id}"><line_symbol color="{c}" line_width="400" minimum_length="0" join_style="1" cap_style="0" start_offset="0" end_offset="0" segment_length="4000" end_length="0" dash_length="4000" break_length="1000" dashes_in_group="1" in_group_break_length="500" mid_symbols_per_spot="1" mid_symbol_distance="0"/></symbol>"#
                ),
            };
        }
        xml += &format!(
            r#"</symbols><parts count="1" current="0"><part name="default"><objects count="{}">"#,
            objects.len()
        );
        for (symbol, points) in objects {
            let n = points.len();
            let coords: Vec<String> = points
                .iter()
                .enumerate()
                .map(|(i, (x, y))| {
                    let (x, y) = ((x * 1000.0) as i64, (y * 1000.0) as i64);
                    let closed = symbols[*symbol].1 == "area" && i + 1 == n;
                    if closed {
                        format!("{x} {y} 18")
                    } else {
                        format!("{x} {y}")
                    }
                })
                .collect();
            xml += &format!(
                r#"<object type="1" symbol="{symbol}"><coords count="{n}">{};</coords></object>"#,
                coords.join(";")
            );
        }
        xml += "</objects></part></parts></map>";
        read_xml_map_str(&xml).unwrap().0
    }

    fn square(x: f64, y: f64, side: f64) -> Vec<(f64, f64)> {
        vec![
            (x, y),
            (x + side, y),
            (x + side, y + side),
            (x, y + side),
            (x, y),
        ]
    }

    fn area_of(coords: &CoordList) -> f64 {
        area_shape(coords).unsigned_area()
    }

    #[test]
    fn a_paved_area_is_cut_where_the_map_draws_green_over_it() {
        // ISSprOM draws the paved browns above the greens; this map, the greens
        // above.
        let paved = square(0.0, 0.0, 10.0);
        let planter = square(4.0, 4.0, 2.0);
        let hedge = [(0.0, 8.0), (10.0, 8.0)];
        let m = map(
            &[
                ("501.5", "area", "bottom"),
                ("410", "area", "top"),
                ("408.3", "line", "top"),
            ],
            &[(0, &paved), (1, &planter), (2, &hedge)],
        );
        let cuts = cut_hidden(&m, &SymbolTable::issprom());
        let cut = &cuts.coords[&0];
        // 100 less the planter (4) and the hedge, 10 long and 0.4 wide with flat ends.
        assert!(
            (area_of(cut) - (100.0 - 4.0 - 4.0)).abs() < 1e-6,
            "{}",
            area_of(cut)
        );
        assert_eq!(cuts.coords.len(), 1, "only the paved area is cut");
        assert_eq!(cuts.warnings.len(), 1);
        assert!(cuts.warnings[0].contains("501.5"), "{}", cuts.warnings[0]);
        // The planter is a hole of the paved area: two closed parts.
        let closes = cut.iter().filter(|c| c.is_close_point()).count();
        assert!(closes >= 2, "{closes}");
    }

    #[test]
    fn nothing_is_cut_where_the_map_has_the_standards_order() {
        let m = map(
            &[("501.5", "area", "top"), ("410", "area", "bottom")],
            &[(0, &square(0.0, 0.0, 10.0)), (1, &square(4.0, 4.0, 2.0))],
        );
        assert!(cut_hidden(&m, &SymbolTable::issprom()).coords.is_empty());
    }

    #[test]
    fn an_area_away_from_what_it_would_hide_is_not_cut() {
        let m = map(
            &[("501.5", "area", "bottom"), ("410", "area", "top")],
            &[(0, &square(0.0, 0.0, 10.0)), (1, &square(20.0, 20.0, 2.0))],
        );
        assert!(cut_hidden(&m, &SymbolTable::issprom()).coords.is_empty());
    }

    #[test]
    fn an_area_with_its_own_outline_is_cut_by_areas_only() {
        // 501 is a paved area with its border.
        let hedge = [(0.0, 8.0), (10.0, 8.0)];
        let m = map(
            &[
                ("501", "area", "bottom"),
                ("410", "area", "top"),
                ("408.3", "line", "top"),
            ],
            &[
                (0, &square(0.0, 0.0, 10.0)),
                (1, &square(4.0, 4.0, 2.0)),
                (2, &hedge),
            ],
        );
        let cuts = cut_hidden(&m, &SymbolTable::issprom());
        assert!((area_of(&cuts.coords[&0]) - 96.0).abs() < 1e-6);
    }

    #[test]
    fn a_course_symbol_is_never_cut() {
        // 714, a closed area, is meant to cover the map.
        let m = map(
            &[("714", "area", "bottom"), ("410", "area", "top")],
            &[(0, &square(0.0, 0.0, 10.0)), (1, &square(4.0, 4.0, 2.0))],
        );
        assert!(cut_hidden(&m, &SymbolTable::issprom()).coords.is_empty());
    }

    #[test]
    fn an_area_covered_whole_is_left_empty() {
        let m = map(
            &[("501.5", "area", "bottom"), ("410", "area", "top")],
            &[(0, &square(4.0, 4.0, 2.0)), (1, &square(0.0, 0.0, 10.0))],
        );
        let cuts = cut_hidden(&m, &SymbolTable::issprom());
        assert!(cuts.coords[&0].is_empty());
    }
}
