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
//!
//! The other way round, a map may draw an area over a line to hide a piece of
//! it, which the standard draws under the line: a club's maps cover the junctions
//! of their roads with a paved area in the roads' own infill colour, so that the
//! roads' outlines stop where they meet. Stored as is, the outlines come back
//! across the junction. So a line which the map draws wholly under an area and
//! the standard (at least partly) above it is cut where the area lies over it.
//! Only a line with nothing laid out along it: a dashed one, or one with dots or
//! tags, would have them spaced anew on each piece. It is cut where its middle
//! goes under the area. What the map drew of the pieces taken away does not all
//! lie under the area: a road running along the area's edge, or along a hole of
//! it, has half its width out of it, and where it meets the edge aslant one side
//! of its cut end sticks out. So the footprint of the pieces taken away, as wide
//! as the map draws the line (its borders too), is added to the area (which is
//! in a colour hiding the line's, so the line goes on as the area): taken away
//! alone, it would leave a strip or a notch that nothing covers. Less what the
//! area was cut by itself, which the map draws above both.

use std::collections::{BTreeMap, BTreeSet};

use geo::algorithm::bool_ops::{unary_union, FillRule};
use geo::algorithm::buffer::{BufferStyle, LineCap};
use geo::{
    Area, BooleanOps, BoundingRect, Buffer, Coord as GeoCoord, LineString, MultiLineString,
    MultiPolygon, Polygon,
};

use crate::geometry::flatten;
use crate::map::{cap_style, coord_flag, symbol_leaves, Coord, CoordList, Map, ObjectKind, Symbol};
use crate::mot_symbols::{SymbolKind, SymbolTable};

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

/// Half the width `symbol`, one of `map`'s, draws a line: its widest line with
/// the borders out to their outer edges.
pub(crate) fn footprint_half(symbol: &Symbol, map: &Map) -> f64 {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    leaves
        .iter()
        .filter_map(|leaf| match leaf {
            Symbol::Line(l) => Some(
                [&l.border, &l.right_border]
                    .into_iter()
                    .filter(|b| b.is_visible())
                    .map(|b| l.line_width / 2.0 + b.shift + b.width / 2.0)
                    .fold(l.line_width / 2.0, f64::max),
            ),
            _ => None,
        })
        .fold(0.0, f64::max)
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
    // The lines first: what an area grows by where a line it hides is taken
    // away is part of it from then on, and cuts what the area cuts.
    let added = cut_lines_under_areas(map, table, &fills, &mut cuts);
    // What each area was cut by, by index in `map.objects`.
    let mut clips: BTreeMap<usize, MultiPolygon> = BTreeMap::new();
    if under.is_empty() {
        grow(map, added, &clips, &mut cuts);
        return cuts;
    }
    let cutting: BTreeSet<usize> = under.values().flatten().copied().collect();

    // The footprint of every object that may cut, with its box.
    let mut footprints: Vec<(usize, geo::Rect, MultiPolygon)> = Vec::new();
    for (index, object) in map.objects.iter().enumerate() {
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
            SymbolKind::Area => added
                .get(&index)
                .into_iter()
                .flatten()
                .fold(area_shape(&object.coords), |shape, piece| {
                    shape.union(piece)
                }),
            _ => {
                let half = footprint_half(&map.symbols[s], map);
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
        clips.insert(index, clip);
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
    grow(map, added, &clips, &mut cuts);
    cuts
}

/// Adds to each area the pieces of lines taken away under it (by index in
/// `map.objects`), on what it is after its own cuts and less what it was cut by
/// (`clips`).
fn grow(
    map: &Map,
    added: BTreeMap<usize, Vec<MultiPolygon>>,
    clips: &BTreeMap<usize, MultiPolygon>,
    cuts: &mut Cuts,
) {
    for (area, pieces) in added {
        let base = match cuts.coords.get(&area) {
            Some(coords) => area_shape(coords),
            None => area_shape(&map.objects[area].coords),
        };
        // One by one: `unary_union` fills by the winding of the first ring it is
        // given, and a buffer's rings may wind the other way from an area's.
        let mut shape = pieces
            .iter()
            .fold(base.clone(), |shape, piece| shape.union(piece));
        if let Some(clip) = clips.get(&area) {
            shape = shape.difference(clip);
        }
        // A footprint wholly under the area adds nothing but rounding, and the
        // area keeps its own coordinates (and curves).
        if shape.unsigned_area() > base.unsigned_area() * (1.0 + 1e-6) + 1e-4 {
            cuts.coords.insert(area, area_coords(&shape));
        }
    }
}

/// Cuts the lines which the map hides under an area and the standard would draw
/// over it, where the area lies over them (see the module's documentation).
/// `fills` is what [`cut_hidden`] found each symbol to fill. Returns what each
/// area (by index in `map.objects`) grows by: the footprints of the pieces taken
/// away under it.
fn cut_lines_under_areas(
    map: &Map,
    table: &SymbolTable,
    fills: &[Option<(Order, bool)>],
    cuts: &mut Cuts,
) -> BTreeMap<usize, Vec<MultiPolygon>> {
    // For each line symbol, the area symbols that hide it in the map and not in
    // the standard.
    let mut hidden_by: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (l, symbol) in map.symbols.iter().enumerate() {
        let entry = symbol
            .is_visible()
            .then(|| table.closest(symbol, map))
            .flatten()
            .and_then(|(id, _)| table.get(id));
        let Some(entry) = entry else { continue };
        if entry.kind != SymbolKind::Line || entry.whole {
            continue;
        }
        let (Some(own), Some(standard)) = (paint_priority(symbol, map), entry.paint) else {
            continue;
        };
        for (a, fill) in fills.iter().enumerate() {
            let Some((fill, _)) = fill else { continue };
            // Wholly at or under the area in the map (the same colour is the
            // area's own), partly over it in the standard.
            if a != l && own >= fill.own && standard < fill.standard {
                hidden_by.entry(l).or_default().insert(a);
            }
        }
    }
    if hidden_by.is_empty() {
        return BTreeMap::new();
    }
    let hiding: BTreeSet<usize> = hidden_by.values().flatten().copied().collect();
    // Each area that hides a line: its symbol, its index and its shape.
    let mut areas: Vec<(usize, usize, geo::Rect, MultiPolygon)> = Vec::new();
    for (index, object) in map.objects.iter().enumerate() {
        let (Some(a), ObjectKind::Path(_)) = (object.symbol_index, &object.kind) else {
            continue;
        };
        if !hiding.contains(&a) || object.coords.is_empty() {
            continue;
        }
        let shape = area_shape(&object.coords);
        if let Some(rect) = shape.bounding_rect() {
            areas.push((a, index, rect, shape));
        }
    }
    // What is added to an area: the pieces of lines taken away under it.
    let mut added: BTreeMap<usize, Vec<MultiPolygon>> = BTreeMap::new();

    let mut cut_by: BTreeMap<usize, (usize, BTreeSet<usize>)> = BTreeMap::new();
    for (index, object) in map.objects.iter().enumerate() {
        let (Some(l), ObjectKind::Path(_)) = (object.symbol_index, &object.kind) else {
            continue;
        };
        let Some(by) = hidden_by.get(&l) else {
            continue;
        };
        // As wide as the map draws it, or the standard where that is wider: the
        // pieces left are drawn as the standard draws them.
        let half = table
            .closest(&map.symbols[l], map)
            .and_then(|(id, _)| table.get(id))
            .map_or(0.0, |e| e.width)
            .max(2.0 * footprint_half(&map.symbols[l], map))
            / 2.0;
        let lines = MultiLineString::new(
            flatten(&object.coords)
                .iter()
                .filter(|part| part.points.len() >= 2)
                .map(|part| {
                    LineString::from(
                        part.points
                            .iter()
                            .map(|p| GeoCoord { x: p.x, y: p.y })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect(),
        );
        let Some(rect) = lines.bounding_rect() else {
            continue;
        };
        let hits: Vec<&(usize, usize, geo::Rect, MultiPolygon)> = areas
            .iter()
            .filter(|(a, _, r, _)| by.contains(a) && overlap(r, &rect))
            .collect();
        if hits.is_empty() {
            continue;
        }
        let cover = unary_union(hits.iter().map(|(_, _, _, s)| s));
        let length = |m: &MultiLineString| -> f64 {
            m.0.iter()
                .flat_map(|l| l.lines())
                .map(|s| (s.end.x - s.start.x).hypot(s.end.y - s.start.y))
                .sum()
        };
        let before = length(&lines);
        let rest = joined(cover.clip(&lines, true));
        if (before - length(&rest)).abs() <= before * 1e-9 + 1e-9 {
            continue;
        }
        cuts.coords.insert(index, line_coords(&rest));
        if half > 0.0 {
            let taken = joined(cover.clip(&lines, false));
            let style = BufferStyle::new(half).line_cap(LineCap::Butt);
            let pieces: Vec<MultiPolygon> = taken
                .0
                .iter()
                .map(|l| l.buffer_with_style(style.clone()))
                .collect();
            let footprint = unary_union(pieces.iter());
            // Into the area it is most under; one with its own outline would
            // outline it, and is left as it is.
            let into = hits
                .iter()
                .filter(|(a, _, _, _)| fills[*a].as_ref().is_some_and(|(_, outlined)| !outlined))
                .max_by(|x, y| {
                    let under = |s: &MultiPolygon| footprint.intersection(s).unsigned_area();
                    under(&x.3).total_cmp(&under(&y.3))
                });
            if let Some((_, area, _, _)) = into {
                added.entry(*area).or_default().push(footprint);
            }
        }
        let entry = cut_by.entry(l).or_default();
        entry.0 += 1;
        entry.1.extend(hits.iter().map(|(a, _, _, _)| *a));
    }
    for (l, (count, by)) in cut_by {
        let by: Vec<String> = by
            .iter()
            .map(|&a| format!("{} \"{}\"", map.symbols[a].code(), map.symbols[a].name()))
            .collect();
        cuts.warnings.push(format!(
            "lines of symbol {} \"{}\" cut where {} lie over them (drawn above them in the map, \
             below them in {}): {count} objects",
            map.symbols[l].code(),
            map.symbols[l].name(),
            by.join(", "),
            table.standard,
        ));
    }
    added
}

/// The pieces of a clipped line joined again where one ends where the next one
/// starts: clipping may hand a piece back segment by segment.
fn joined(pieces: MultiLineString) -> MultiLineString {
    const NEAR: f64 = 1e-9;
    let mut out: Vec<Vec<GeoCoord>> = Vec::new();
    for piece in pieces.0 {
        let points = piece.0;
        if points.len() < 2 {
            continue;
        }
        if let Some(last) = out.last_mut() {
            let end = last[last.len() - 1];
            if (end.x - points[0].x).abs() <= NEAR && (end.y - points[0].y).abs() <= NEAR {
                last.extend_from_slice(&points[1..]);
                continue;
            }
        }
        out.push(points);
    }
    MultiLineString::new(out.into_iter().map(LineString::from).collect())
}

/// Lines as a line's coordinates: one part each, ended by a hole point but the last.
fn line_coords(lines: &MultiLineString) -> CoordList {
    let parts: Vec<&LineString> = lines.0.iter().filter(|l| l.0.len() >= 2).collect();
    let mut coords = CoordList::new();
    for (i, part) in parts.iter().enumerate() {
        let last = part.0.len() - 1;
        for (j, c) in part.0.iter().enumerate() {
            let flags = if j == last && i + 1 < parts.len() {
                coord_flag::HOLE_POINT
            } else {
                0
            };
            coords.push(Coord::new(c.x, c.y, flags));
        }
    }
    coords
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
pub(crate) fn area_shape(coords: &CoordList) -> MultiPolygon {
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
pub(crate) fn area_coords(shape: &MultiPolygon) -> CoordList {
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

    /// A map with three colours, `top` drawn above `mid` above `bottom`, and
    /// these symbols (code, kind and colour) and objects (symbol index and
    /// coordinates in mm, a closed ring for an area).
    fn map(symbols: &[(&str, &str, &str)], objects: &[(usize, &[(f64, f64)])]) -> Map {
        let colors = r#"<colors count="3"><color priority="0" name="top" c="0.8" m="0" y="1" k="0" opacity="1"/><color priority="1" name="mid" c="0" m="0.5" y="0.5" k="0" opacity="1"/><color priority="2" name="bottom" c="0" m="0.17" y="0.3" k="0.05" opacity="1"/></colors>"#;
        let mut xml = format!("<map>{colors}<symbols count=\"{}\">", symbols.len());
        for (id, (code, kind, color)) in symbols.iter().enumerate() {
            let c = match *color {
                "top" => 0,
                "mid" => 1,
                _ => 2,
            };
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

    fn line_length(coords: &CoordList) -> f64 {
        flatten(coords)
            .iter()
            .map(|part| {
                part.points
                    .windows(2)
                    .map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y))
                    .sum::<f64>()
            })
            .sum()
    }

    #[test]
    fn a_road_the_map_hides_under_a_paved_area_is_cut_where_it_lies() {
        // The map covers a junction with a paved area above the road; ISSprOM
        // draws the road (501.7: its infill and outline) above a paved area.
        let road = [(-10.0, 5.0), (0.0, 5.0), (20.0, 5.0)];
        let m = map(
            &[("501.5", "area", "top"), ("501.7", "line", "bottom")],
            &[(0, &square(0.0, 0.0, 10.0)), (1, &road)],
        );
        let cuts = cut_hidden(&m, &SymbolTable::issprom());
        let cut = &cuts.coords[&1];
        // 30 long, 10 of it under the area: two pieces, 10 each side.
        assert!(
            (line_length(cut) - 20.0).abs() < 1e-6,
            "{}",
            line_length(cut)
        );
        assert_eq!(flatten(cut).len(), 2);
        // Across the area square to its edges: the piece taken away was all under
        // it, which is left as it is.
        assert_eq!(cuts.coords.len(), 1);
        assert!(cuts
            .warnings
            .iter()
            .any(|w| w.contains("lines of symbol 501.7")));
    }

    #[test]
    fn the_piece_of_a_road_taken_away_aslant_is_added_to_the_area() {
        // The road goes under the area's left edge at 45 degrees: cut where its
        // middle goes in, one side of its end would stick out of the area, which
        // grows by that much.
        let road = [(-10.0, -5.0), (5.0, 10.0)];
        let paved = square(0.0, 0.0, 10.0);
        let m = map(
            &[("501.5", "area", "top"), ("501.7", "line", "bottom")],
            &[(0, &paved), (1, &road)],
        );
        let cuts = cut_hidden(&m, &SymbolTable::issprom());
        assert!(cuts.coords.contains_key(&1));
        let grown = area_of(&cuts.coords[&0]);
        assert!(grown > 100.0 && grown < 100.2, "{grown}");
    }

    #[test]
    fn a_road_along_the_edge_of_the_area_hiding_it_goes_on_as_the_area() {
        // The road's middle runs just inside the area's top edge: it is taken
        // away, and the half of it out of the area, which the map drew, is added
        // to the area rather than left uncovered.
        let road = [(-10.0, 0.1), (20.0, 0.1)];
        let m = map(
            &[("501.5", "area", "top"), ("501.7", "line", "bottom")],
            &[(0, &square(0.0, 0.0, 10.0)), (1, &road)],
        );
        let cuts = cut_hidden(&m, &SymbolTable::issprom());
        assert!(cuts.coords.contains_key(&1));
        // The map's line is 0.4 wide: 0.1 of its 0.2 above the middle is out of
        // the area, along the 10 under it (the standard's 501.7 is wider still).
        let grown = area_of(&cuts.coords[&0]);
        assert!(grown > 101.0 - 1e-6, "{grown}");
    }

    #[test]
    fn what_a_road_goes_on_as_cuts_what_the_area_cuts() {
        // A white forest under all; the paved area over its left half, and a road
        // between them, hidden by the paved area along its edge. The forest is cut
        // where the paved area lies (ISSprOM draws it above), and where the road
        // lay, which now goes on as the paved area: else the forest would cover
        // the half of the road out of the paved area.
        let forest = square(0.0, 0.0, 10.0);
        let paved = [(0.0, 0.0), (5.0, 0.0), (5.0, 10.0), (0.0, 10.0), (0.0, 0.0)];
        let road = [(4.9, -5.0), (4.9, 15.0)];
        let m = map(
            &[
                ("405", "area", "bottom"),
                ("501.5", "area", "top"),
                ("501.7", "line", "mid"),
            ],
            &[(0, &forest), (1, &paved), (2, &road)],
        );
        let cuts = cut_hidden(&m, &SymbolTable::issprom());
        assert!(cuts.coords.contains_key(&2), "the road is cut");
        let left = area_of(&cuts.coords[&0]);
        assert!(left < 49.0, "{left}");
        // And what the forest lost, the paved area has.
        let paved = area_of(&cuts.coords[&1]);
        assert!((paved + left - 100.0).abs() < 1e-3, "{paved} + {left}");
    }

    #[test]
    fn a_road_the_map_draws_over_a_paved_area_is_left_whole() {
        let road = [(-10.0, 5.0), (20.0, 5.0)];
        let m = map(
            &[("501.5", "area", "bottom"), ("501.7", "line", "top")],
            &[(0, &square(0.0, 0.0, 10.0)), (1, &road)],
        );
        assert!(!cut_hidden(&m, &SymbolTable::issprom())
            .coords
            .contains_key(&1));
    }

    #[test]
    fn an_area_grown_by_a_piece_of_road_keeps_its_holes() {
        // A paved area with a planter in it (a hole: two rings, filled even-odd),
        // and a road going under its edge aslant, far from the hole.
        let paved = [
            (0.0, 0.0),
            (20.0, 0.0),
            (20.0, 20.0),
            (0.0, 20.0),
            (0.0, 0.0),
        ];
        let road = [(-10.0, 5.0), (5.0, 20.0)];
        let mut m = map(
            &[("501.5", "area", "top"), ("501.7", "line", "bottom")],
            &[(0, &paved), (1, &road)],
        );
        // The hole, as a second closed part of the same area.
        let mut coords = m.objects[0].coords.clone();
        let last = coords.len() - 1;
        coords[last] = Coord::new(
            coords[last].x,
            coords[last].y,
            coord_flag::CLOSE_POINT | coord_flag::HOLE_POINT,
        );
        for (x, y) in square(14.0, 14.0, 4.0) {
            coords.push(Coord::new(x, y, 0));
        }
        let last = coords.len() - 1;
        coords[last] = Coord::new(coords[last].x, coords[last].y, coord_flag::CLOSE_POINT);
        m.objects[0].coords = coords;
        let before = area_of(&m.objects[0].coords);
        assert!((before - 384.0).abs() < 1e-6, "{before}");
        let cuts = cut_hidden(&m, &SymbolTable::issprom());
        let grown = area_of(&cuts.coords[&0]);
        assert!(grown > 384.0 && grown < 384.5, "{grown}");
    }
}
