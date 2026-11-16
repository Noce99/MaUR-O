//! Makes MOT tiles from a map.
//!
//! The one direction of the codec that touches the [`Map`] model: every
//! object's paper coordinates are placed on the earth, turned into Web
//! Mercator, given the id of its symbol in a [`SymbolTable`], and handed to
//! every tile it touches. See `mds/MOT-Format.md`.
//!
//! A tile holds the part of an object inside its box grown by a buffer
//! ([`Options::buffer`]), so a stroke or a symbol at the edge is not cut short
//! and a renderer draws each tile clipped to the tile itself. An object
//! entirely inside the grown box is stored whole, curves and all. One that
//! crosses it has its curves flattened and is cut by [`crate::mot_clip`]: a
//! line into pieces (each says where it was cut, and how far along the line it
//! starts, for its dashes), an area into a ring closed along the edge. All the
//! pieces have the object's [`TileObject::id`]. A point or a text is in every
//! tile whose grown box holds it.
//!
//! The map is placed from its [`GeographicAnchor`] (the reference point in
//! degrees) laid out on the ellipsoid, turned by the map's grivation, as
//! [`crate::orthophoto`] does it. That is good to a few meters in ten
//! kilometers, which is enough to put a map in the right tile but not to
//! overlay it exactly on a survey of the same ground.

use std::collections::BTreeMap;

use crate::map::{Map, ObjectKind, Symbol};
use crate::mot::node_flag;
use crate::mot::{Grid, Node, Pattern, Shape, Text, Tile, TileId, TileObject, WORLD_HALF};
use crate::mot_clip::{clip_line, clip_ring, flatten, simplify, Rect, Vertex};
use crate::mot_symbols::{Match, SymbolTable, ZoomRule};
use crate::orthophoto::{
    ground_to_geographic, true_north_rotation, web_mercator, GeographicAnchor,
};

/// What to build.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The zoom level of the tiles.
    pub zoom: u8,
    /// The finest zoom level stored, where every object is kept as drawn. A
    /// level coarser than this one is generalised by the symbols' own rules
    /// ([`crate::mot_symbols::ZoomRule`]) and on a coarser grid; for the finest,
    /// `zoom` equals this.
    pub detail_zoom: u8,
    /// The resolution of the nodes, in centimetres of Mercator.
    pub grid_cm: u32,
    /// How far past its edge a tile keeps what it holds, as a fraction of the
    /// tile's side. 1/32 is about 50 m at zoom 14: more than a line's width or
    /// a symbol's size at 1:4000 or 1:10000, and about what one is at 1:15000.
    /// A tile holds roughly `(1 + 2 * buffer)^2` times what it would with none,
    /// so this is also the size of the archive.
    pub buffer: f64,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            zoom: 14,
            detail_zoom: 14,
            grid_cm: 5,
            buffer: 1.0 / 32.0,
        }
    }
}

/// The result of converting a map.
pub struct Converted {
    /// The tiles, by position.
    pub tiles: BTreeMap<TileId, Tile>,
    /// The id the next converted object should get: ids are assigned in
    /// order from the `first_id` given, one per object.
    pub next_id: u64,
    /// How many objects went into tiles.
    pub objects: usize,
    /// How many objects were not stored: no symbol of the table is close to
    /// theirs, or they have none.
    pub dropped: usize,
    /// How many objects were left out because their symbol is hidden or a
    /// drawing aid, which draws nothing on the printed map.
    pub hidden: usize,
    /// How many objects were left out of this level by the rules of their
    /// symbols: not shown yet at this zoom, or too small to see.
    pub generalized: usize,
    /// How many objects were stored under another symbol than their own.
    pub remapped: usize,
    /// One line for every symbol that was remapped or dropped, and for
    /// anything else worth knowing.
    pub warnings: Vec<String>,
    /// For a pyramid ([`convert_levels`]), what each level came to, finest
    /// first; empty for a single level.
    pub levels: Vec<LevelSummary>,
}

/// What one level of a pyramid came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelSummary {
    /// The zoom level.
    pub zoom: u8,
    /// How many objects went into its tiles.
    pub objects: usize,
    /// How many were left out by the rules of their symbols.
    pub generalized: usize,
    /// How many tiles it has.
    pub tiles: usize,
}

/// The grid, in centimetres, of the level at `zoom` when the finest, at
/// `detail_zoom`, is on a grid of `base_cm`. A coarser level has a coarser
/// grid, as it has coarser geometry: about 4096 cells across a tile, which is
/// finer than any pixel it is drawn in.
pub fn level_grid_cm(zoom: u8, detail_zoom: u8, base_cm: u32) -> u32 {
    if zoom >= detail_zoom {
        return base_cm.max(1);
    }
    let cells = (TileId::size(zoom) * 100.0 / 4096.0).ceil() as u32;
    cells.max(base_cm).max(1)
}

/// The position of a Mercator point along a Z-order curve over the world.
fn morton(x: f64, y: f64) -> u64 {
    let unit = |v: f64| {
        (((v + WORLD_HALF) / (2.0 * WORLD_HALF)).clamp(0.0, 1.0) * f64::from(u32::MAX)) as u64
    };
    let spread = |mut v: u64| {
        v = (v | (v << 16)) & 0x0000_ffff_0000_ffff;
        v = (v | (v << 8)) & 0x00ff_00ff_00ff_00ff;
        v = (v | (v << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
        v = (v | (v << 2)) & 0x3333_3333_3333_3333;
        (v | (v << 1)) & 0x5555_5555_5555_5555
    };
    spread(unit(x)) | (spread(unit(y)) << 1)
}

fn describe(symbol: &Symbol) -> String {
    format!("{} \"{}\"", symbol.code(), symbol.name())
}

/// Converts `map`, placed at `anchor`, into tiles of `table`'s symbols.
///
/// Ids are assigned from `first_id`. An error where the map has no
/// georeferencing, so cannot be placed.
pub fn convert(
    map: &Map,
    anchor: &GeographicAnchor,
    table: &SymbolTable,
    first_id: u64,
    options: &Options,
) -> Result<Converted, String> {
    let georef = map
        .georeferencing
        .as_ref()
        .ok_or("the map is not georeferenced")?;
    let scale = if georef.scale > 0 {
        georef.scale
    } else {
        map.scale_denominator
    };
    let meters_per_mm = f64::from(scale) / 1000.0;
    let north = true_north_rotation(georef, anchor);
    let grid_cm = level_grid_cm(options.zoom, options.detail_zoom, options.grid_cm);
    let grid = Grid::new(grid_cm);
    let coarse = options.zoom < options.detail_zoom;
    // A pixel of a 256 pixel tile of this level, in Mercator meters.
    let pixel = TileId::size(options.zoom) / 256.0;
    // A Mercator meter is longer than a ground meter by this much here.
    let stretch = 1.0 / anchor.lat.to_radians().cos().max(0.05);

    // paper mm -> Mercator meters, y growing north
    let project = |x: f64, y: f64| -> (f64, f64) {
        let (lat, lon) = ground_to_geographic(
            georef,
            anchor,
            north,
            (x * meters_per_mm, y * meters_per_mm),
        );
        web_mercator(lat, lon)
    };

    // The angle from a tile's east to the map's own east, which every
    // rotation in the map has to be turned by.
    let (a, b) = (
        project(georef.map_ref_point_x, georef.map_ref_point_y),
        project(georef.map_ref_point_x + 1.0, georef.map_ref_point_y),
    );
    let frame = (-(b.1 - a.1)).atan2(b.0 - a.0);

    // What each symbol of the map becomes, worked out once.
    let mut mapped: Vec<Option<u16>> = Vec::with_capacity(map.symbols.len());
    let mut notes: Vec<String> = Vec::new();
    for symbol in &map.symbols {
        let found = table.closest(symbol);
        match found {
            Some((id, Match::Code)) => mapped.push(Some(id)),
            Some((id, how)) => {
                let target = table.get(id).expect("an id of the table");
                notes.push(format!(
                    "symbol {} stored as {} \"{}\" ({})",
                    describe(symbol),
                    target.code,
                    target.name,
                    match how {
                        Match::Parent => "the symbol it is a variant of",
                        _ => "the same name",
                    }
                ));
                mapped.push(Some(id));
            }
            None => mapped.push(None),
        }
    }

    let mut converted = Converted {
        tiles: BTreeMap::new(),
        next_id: first_id,
        objects: 0,
        dropped: 0,
        hidden: 0,
        generalized: 0,
        remapped: 0,
        warnings: Vec::new(),
        levels: Vec::new(),
    };
    // A UTM easting a long way outside its zone's strip means the file's
    // georeference is wrong (an easting of 0 is the usual slip), and the map
    // would be stored where Mapper shows it, not where it is.
    let utm_zone = matches!(georef.epsg, 32601..=32660 | 32701..=32760 | 25828..=25838);
    if utm_zone && !(100_000.0..=900_000.0).contains(&georef.ref_point_x) {
        converted.warnings.push(format!(
            "the map's projected reference point has an easting of {}, far outside a UTM zone: \
             its georeference looks wrong, and it is placed at lat {:.4}, lon {:.4}",
            georef.ref_point_x, anchor.lat, anchor.lon
        ));
    }
    let mut remapped_symbols = vec![0usize; map.symbols.len()];
    let mut dropped_symbols = vec![0usize; map.symbols.len()];

    // Ids are given in order along a Z-order curve through each object's first
    // node. Inside a tile objects are sorted by (symbol, id), so this keeps
    // both the ids and the first nodes of neighbours in the file close, and
    // so cheap to code as differences.
    let mut order: Vec<(u64, usize)> = map
        .objects
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let key = o.coords.first().map_or(0, |c| {
                let (x, y) = project(c.x, c.y);
                morton(x, y)
            });
            (key, i)
        })
        .collect();
    order.sort_unstable();

    for &(_, position) in &order {
        let object = &map.objects[position];
        let Some(index) = object.symbol_index else {
            converted.dropped += 1;
            continue;
        };
        let symbol = &map.symbols[index];
        if !symbol.is_visible() || object.coords.is_empty() {
            converted.hidden += 1;
            continue;
        }
        let Some(id_in_table) = mapped[index] else {
            converted.dropped += 1;
            dropped_symbols[index] += 1;
            continue;
        };
        if table
            .get(id_in_table)
            .is_some_and(|e| e.code != symbol.code())
        {
            converted.remapped += 1;
            remapped_symbols[index] += 1;
        }

        // Every object has its id whatever a level keeps of it, so that the
        // levels agree on which object is which.
        let id = converted.next_id;
        converted.next_id += 1;

        let rule = table.rule(id_in_table);
        if coarse && options.zoom < rule.min_zoom {
            converted.generalized += 1;
            continue;
        }
        let mut places: Vec<(f64, f64)> = object.coords.iter().map(|c| project(c.x, c.y)).collect();
        let mut flags: Vec<i32> = object.coords.iter().map(|c| c.flags).collect();
        if coarse && matches!(object.kind, ObjectKind::Path(_)) {
            match generalize(&places, &flags, pixel, &rule) {
                Some((p, f)) => (places, flags) = (p, f),
                None => {
                    converted.generalized += 1;
                    continue;
                }
            }
        }
        let mut bounds = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for &(x, y) in &places {
            bounds = [
                bounds[0].min(x),
                bounds[1].min(y),
                bounds[2].max(x),
                bounds[3].max(y),
            ];
        }

        let wants_pattern = |rotation: f64, origin: (f64, f64)| {
            rotation != 0.0
                || origin != (0.0, 0.0)
                || match symbol {
                    Symbol::Area(a) => !a.patterns.is_empty(),
                    Symbol::Combined(_) => true,
                    _ => false,
                }
        };
        let pattern_place = match &object.kind {
            ObjectKind::Path(p)
                if wants_pattern(p.pattern_rotation, (p.pattern_origin.x, p.pattern_origin.y)) =>
            {
                Some((
                    p.pattern_rotation,
                    project(p.pattern_origin.x, p.pattern_origin.y),
                ))
            }
            _ => None,
        };

        converted.objects += 1;

        let per_unit = 100.0 / f64::from(grid_cm);
        let side = TileId::size(options.zoom) * per_unit;
        let margin = options.buffer.max(0.0) * side;
        let margin_m = options.buffer.max(0.0) * TileId::size(options.zoom);
        let grown = [
            bounds[0] - margin_m,
            bounds[1] - margin_m,
            bounds[2] + margin_m,
            bounds[3] + margin_m,
        ];
        let rotation = object.rotation + frame;

        for tile_id in TileId::covering(options.zoom, grown) {
            let (west, top) = tile_id.origin();
            let local = |(x, y): (f64, f64)| ((x - west) * per_unit, (top - y) * per_unit);
            let to_grid = |p: (f64, f64)| {
                let (x, y) = local(p);
                (x.round() as i32, y.round() as i32)
            };
            let rect = Rect {
                x0: -margin,
                y0: -margin,
                x1: side + margin,
                y1: side + margin,
            };
            let locals: Vec<(f64, f64)> = places.iter().map(|&p| local(p)).collect();
            let inside = |&(x, y): &(f64, f64)| rect.contains(x, y);

            // What goes in this tile: nodes, and for a line piece its dash offset.
            let mut stored: Vec<(Vec<Node>, u32)> = Vec::new();
            let whole = |locals: &[(f64, f64)]| -> Vec<Node> {
                flags
                    .iter()
                    .zip(locals)
                    .map(|(&f, &p)| Node {
                        x: p.0.round() as i32,
                        y: p.1.round() as i32,
                        flags: f as u8,
                    })
                    .collect()
            };
            let shape = match &object.kind {
                ObjectKind::Point => Shape::Point,
                ObjectKind::Path(_) => {
                    Shape::Path(pattern_place.map(|(pattern_rotation, origin)| Pattern {
                        rotation: pattern_rotation + frame,
                        origin: to_grid(origin),
                    }))
                }
                ObjectKind::Text(t) => Shape::Text(Text {
                    text: t.text.clone(),
                    h_align: t.h_align.clamp(0, 3) as u8,
                    v_align: t.v_align.clamp(0, 3) as u8,
                    box_size: t.box_size.map(|(w, h)| {
                        let units =
                            |mm: f64| grid.units(mm * meters_per_mm * stretch).max(0) as u32;
                        (units(w), units(h))
                    }),
                }),
            };
            match &object.kind {
                ObjectKind::Point | ObjectKind::Text(_) => {
                    if locals.iter().any(inside) {
                        stored.push((whole(&locals), 0));
                    }
                }
                ObjectKind::Path(_) if locals.iter().all(inside) => {
                    stored.push((whole(&locals), 0));
                }
                ObjectKind::Path(_) => {
                    clip_path(&flags, &locals, &rect, &mut stored);
                }
            }
            if stored.is_empty() {
                continue;
            }
            let tile = converted
                .tiles
                .entry(tile_id)
                .or_insert_with(|| Tile::new(grid_cm));
            for (nodes, dash_offset) in stored {
                tile.objects.push(TileObject {
                    id,
                    symbol: id_in_table,
                    rotation,
                    shape: shape.clone(),
                    nodes,
                    dash_offset,
                });
            }
        }
    }

    converted.warnings.extend(
        notes
            .into_iter()
            .zip(&remapped_symbols)
            .filter(|(_, &n)| n > 0)
            .map(|(note, n)| format!("{note}: {n} objects")),
    );
    for (index, &n) in dropped_symbols.iter().enumerate() {
        if n > 0 {
            converted.warnings.push(format!(
                "symbol {} has no close symbol in {}, so its {} objects were dropped",
                describe(&map.symbols[index]),
                table.standard,
                n
            ));
        }
    }
    Ok(converted)
}

/// A path's positions, in Mercator meters, and its node flags.
type Generalized = (Vec<(f64, f64)>, Vec<i32>);

/// A path as a coarser level shows it: `None` where it is too small to see,
/// otherwise with its curves flattened and its nodes thinned out, never moving
/// the path further than the rule allows.
///
/// `places` are in Mercator meters, `flags` the path's node flags, and `pixel`
/// is how many meters a pixel of the level is. A part which comes out with too
/// little left to be a line or a ring goes; the nodes which mean something (a
/// dash, a gap, the end of a hole, a close point) stay.
fn generalize(
    places: &[(f64, f64)],
    flags: &[i32],
    pixel: f64,
    rule: &ZoomRule,
) -> Option<Generalized> {
    let closed = flags.iter().any(|f| f & node_flag::CLOSE_POINT != 0);
    let size = if closed {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(x, y) in places {
            (x0, x1) = (x0.min(x), x1.max(x));
            (y0, y1) = (y0.min(y), y1.max(y));
        }
        (x1 - x0).max(y1 - y0)
    } else {
        places
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
            .sum()
    };
    if size < f64::from(rule.min_size_px) * pixel {
        return None;
    }
    if rule.simplify_px <= 0.0 {
        return Some((places.to_vec(), flags.to_vec()));
    }
    let (mut out_places, mut out_flags): Generalized = (Vec::new(), Vec::new());
    let mut begin = 0;
    for end in 0..flags.len() {
        if flags[end] & node_flag::HOLE_POINT == 0 && end + 1 != flags.len() {
            continue;
        }
        let part: Vec<(f64, f64, i32)> = (begin..=end)
            .map(|i| (places[i].0, places[i].1, flags[i]))
            .collect();
        begin = end + 1;
        let thinned = simplify(
            &flatten(&part, 0.25 * pixel),
            f64::from(rule.simplify_px) * pixel,
        );
        let closed_part = flags[end] & node_flag::CLOSE_POINT != 0;
        if thinned.len() < if closed_part { 4 } else { 2 } {
            continue;
        }
        for v in thinned {
            out_places.push((v.x, v.y));
            out_flags.push(i32::from(v.flags));
        }
    }
    // A part that went may have been the one after a hole's end.
    if let Some(last) = out_flags.last_mut() {
        *last &= !node_flag::HOLE_POINT;
    }
    (!out_places.is_empty()).then_some((out_places, out_flags))
}

/// The parts of a path inside `rect`, as nodes (and the dash offset of each
/// line piece), pushed to `stored`.
///
/// `locals` are the path's coordinates in the tile's grid units. The path is
/// flattened first. A closed part is cut as a ring, and all of them make one
/// object, as the rings of an area do; an open one is cut into line pieces,
/// each its own.
fn clip_path(
    flags: &[i32],
    locals: &[(f64, f64)],
    rect: &Rect,
    stored: &mut Vec<(Vec<Node>, u32)>,
) {
    let node = |v: &Vertex| Node {
        x: v.x.round() as i32,
        y: v.y.round() as i32,
        flags: v.flags,
    };
    let structure = (node_flag::CLOSE_POINT | node_flag::HOLE_POINT) as u8;
    let mut rings: Vec<Vec<Node>> = Vec::new();
    let mut begin = 0;
    for end in 0..flags.len() {
        if flags[end] & node_flag::HOLE_POINT == 0 && end + 1 != flags.len() {
            continue;
        }
        let part: Vec<(f64, f64, i32)> = (begin..=end)
            .map(|i| (locals[i].0, locals[i].1, flags[i]))
            .collect();
        begin = end + 1;
        let vertices = flatten(&part, 0.5);
        if flags[end] & node_flag::CLOSE_POINT != 0 {
            let ring = clip_ring(&vertices, rect);
            if let Some(first) = ring.first() {
                let mut nodes: Vec<Node> = ring.iter().map(&node).collect();
                for n in &mut nodes {
                    n.flags &= !structure;
                }
                nodes.push(Node {
                    flags: node_flag::CLOSE_POINT as u8,
                    ..node(first)
                });
                rings.push(nodes);
            }
            continue;
        }
        // An open part: a line, which stops at a gap and goes on after it.
        let mut run_start = 0;
        for (i, v) in vertices.iter().enumerate() {
            if v.flags & node_flag::GAP_POINT as u8 == 0 && i + 1 != vertices.len() {
                continue;
            }
            for piece in clip_line(&vertices[run_start..=i], rect) {
                let mut nodes: Vec<Node> = piece.vertices.iter().map(&node).collect();
                for n in &mut nodes {
                    n.flags &= !structure;
                }
                if piece.start_cut {
                    nodes[0].flags |= node_flag::CLIPPED as u8;
                }
                if piece.end_cut {
                    let last = nodes.len() - 1;
                    nodes[last].flags |= node_flag::CLIPPED as u8;
                }
                let offset = if piece.start_cut {
                    piece.offset.round() as u32
                } else {
                    0
                };
                stored.push((nodes, offset));
            }
            run_start = i + 1;
        }
    }
    let last = rings.len().saturating_sub(1);
    let mut area: Vec<Node> = Vec::new();
    for (i, mut ring) in rings.into_iter().enumerate() {
        if i != last {
            let end = ring.len() - 1;
            ring[end].flags |= node_flag::HOLE_POINT as u8;
        }
        area.extend(ring);
    }
    if !area.is_empty() {
        stored.insert(0, (area, 0));
    }
}

/// Converts `map` into every level from `options.zoom`, the finest, up to
/// `min_zoom`: the finest as drawn, each coarser one generalised by the rules of
/// the symbols and on a coarser grid. The tiles of all the levels are in the one
/// result, and the objects have the same ids in all of them.
///
/// The counts and the warnings are those of the finest level; `levels` says what
/// each came to.
pub fn convert_levels(
    map: &Map,
    anchor: &GeographicAnchor,
    table: &SymbolTable,
    first_id: u64,
    min_zoom: u8,
    options: &Options,
) -> Result<Converted, String> {
    let detail = options.zoom;
    let mut result = convert(
        map,
        anchor,
        table,
        first_id,
        &Options {
            detail_zoom: detail,
            ..*options
        },
    )?;
    result.levels.push(LevelSummary {
        zoom: detail,
        objects: result.objects,
        generalized: result.generalized,
        tiles: result.tiles.len(),
    });
    for zoom in (min_zoom..detail).rev() {
        let level = convert(
            map,
            anchor,
            table,
            first_id,
            &Options {
                zoom,
                detail_zoom: detail,
                ..*options
            },
        )?;
        result.levels.push(LevelSummary {
            zoom,
            objects: level.objects,
            generalized: level.generalized,
            tiles: level.tiles.len(),
        });
        merge_tiles(&mut result.tiles, level.tiles);
    }
    Ok(result)
}

/// Adds the tiles of `from` to those of `into`: the objects of a tile in both
/// go into one. Both must be on the same grid.
pub fn merge_tiles(into: &mut BTreeMap<TileId, Tile>, from: BTreeMap<TileId, Tile>) {
    for (id, tile) in from {
        match into.entry(id) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(tile);
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                slot.get_mut().objects.extend(tile.objects);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{coord_flag, Coord, Georeferencing, Object, PathObject};
    use crate::mot::node_flag;
    use crate::osm_orienteering::ISOM;
    use crate::xml_reader::read_xml_map_str;

    /// Bologna, near enough.
    fn anchor() -> GeographicAnchor {
        GeographicAnchor {
            lat: 44.4938,
            lon: 11.343,
            declination: 0.0,
        }
    }

    /// The ISOM reference map with nothing on it, placed at the anchor.
    fn empty_map() -> Map {
        let (mut map, _) = read_xml_map_str(ISOM).unwrap();
        map.objects.clear();
        map.georeferencing = Some(Georeferencing {
            scale: 10000,
            epsg: 32632,
            ref_point_x: 700_000.0,
            ref_point_y: 4_900_000.0,
            ..Default::default()
        });
        map
    }

    /// An object of the first visible symbol of the given kind.
    fn object_of(
        map: &Map,
        pick: impl Fn(&Symbol) -> bool,
        kind: ObjectKind,
        coords: Vec<Coord>,
    ) -> Object {
        let index = map
            .symbols
            .iter()
            .position(|s| pick(s) && s.is_visible() && !s.code().is_empty())
            .unwrap();
        let mut o = Object::new(kind);
        o.symbol_id = map.symbol_ids[index];
        o.symbol_index = Some(index);
        o.coords = coords;
        o
    }

    fn convert_map(map: &Map) -> Converted {
        convert(map, &anchor(), &SymbolTable::isom(), 0, &Options::default()).unwrap()
    }

    #[test]
    fn the_reference_point_lands_where_the_anchor_is() {
        let mut map = empty_map();
        let o = object_of(
            &map,
            |s| matches!(s, Symbol::Point(_)),
            ObjectKind::Point,
            vec![Coord::new(0.0, 0.0, 0)],
        );
        map.objects.push(o);
        let converted = convert_map(&map);
        assert_eq!(converted.objects, 1);
        let (id, tile) = converted.tiles.iter().next().unwrap();
        let (x, y) = tile.node_mercator(*id, &tile.objects[0].nodes[0]);
        let (ax, ay) = web_mercator(anchor().lat, anchor().lon);
        assert!(
            (x - ax).abs() < 0.05 && (y - ay).abs() < 0.05,
            "{x},{y} vs {ax},{ay}"
        );
    }

    fn convert_with(map: &Map, options: &Options) -> Converted {
        convert(map, &anchor(), &SymbolTable::isom(), 0, options).unwrap()
    }

    /// Where a tile's nodes are, in Mercator meters.
    fn absolute(tile: &Tile, id: TileId, nodes: &[Node]) -> Vec<(f64, f64)> {
        nodes.iter().map(|n| tile.node_mercator(id, n)).collect()
    }

    fn path_length(points: &[(f64, f64)]) -> f64 {
        points
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
            .sum()
    }

    #[test]
    fn a_line_across_tiles_is_cut_into_pieces_which_know_their_dash_phase() {
        let mut map = empty_map();
        // At 1:10000 a millimetre of paper is 10 m, so 500 mm is 5 km: several
        // zoom 14 tiles (about 1.7 km each at this latitude).
        let line = object_of(
            &map,
            |s| matches!(s, Symbol::Line(_)),
            ObjectKind::Path(PathObject::default()),
            vec![Coord::new(0.0, 0.0, 0), Coord::new(500.0, 0.0, 0)],
        );
        map.objects.push(line);
        let options = Options {
            buffer: 0.0,
            ..Options::default()
        };
        let converted = convert_with(&map, &options);
        assert!(
            converted.tiles.len() >= 3,
            "{} tiles",
            converted.tiles.len()
        );

        /// A piece as stored: where it is, its dash offset, and whether its
        /// first and last nodes are cuts.
        type Piece = (Vec<(f64, f64)>, u32, bool, bool);
        let mut pieces: Vec<Piece> = Vec::new();
        for (id, tile) in &converted.tiles {
            for o in &tile.objects {
                assert_eq!(o.id, 0, "every piece has the object's id");
                let first = o.nodes[0].flags & node_flag::CLIPPED as u8 != 0;
                let last = o.nodes[o.nodes.len() - 1].flags & node_flag::CLIPPED as u8 != 0;
                pieces.push((absolute(tile, *id, &o.nodes), o.dash_offset, first, last));
            }
        }
        // The true ends are the ones that are not cuts.
        let start = pieces.iter().find(|p| !p.2).expect("a piece has the start");
        assert_eq!(start.1, 0);
        let start = start.0[0];
        let end = pieces.iter().find(|p| !p.3).expect("a piece has the end");
        let end = *end.0.last().unwrap();
        let total = (end.0 - start.0).hypot(end.1 - start.1);
        let sum: f64 = pieces.iter().map(|p| path_length(&p.0)).sum();
        assert!((sum - total).abs() < 0.5, "{sum} vs {total}");
        // A piece cut at its start starts as far along as its offset says.
        for (points, offset, start_cut, _) in &pieces {
            let along = (points[0].0 - start.0).hypot(points[0].1 - start.1);
            let offset_m = f64::from(*offset) * 0.05;
            assert!(*start_cut == (*offset > 0));
            assert!((along - offset_m).abs() < 0.5, "{along} vs {offset_m}");
        }
    }

    #[test]
    fn an_area_across_tiles_is_cut_to_each_and_the_pieces_add_up() {
        let mut map = empty_map();
        let square = vec![
            Coord::new(-100.0, -100.0, 0),
            Coord::new(100.0, -100.0, 0),
            Coord::new(100.0, 100.0, 0),
            Coord::new(-100.0, 100.0, 0),
            Coord::new(-100.0, -100.0, coord_flag::CLOSE_POINT),
        ];
        let area = object_of(
            &map,
            |s| matches!(s, Symbol::Area(_)),
            ObjectKind::Path(PathObject::default()),
            square,
        );
        map.objects.push(area);
        let area_of = |converted: &Converted| -> f64 {
            let mut total = 0.0;
            for (id, tile) in &converted.tiles {
                for o in &tile.objects {
                    let p = absolute(tile, *id, &o.nodes);
                    total += (0..p.len() - 1)
                        .map(|i| p[i].0 * p[i + 1].1 - p[i + 1].0 * p[i].1)
                        .sum::<f64>()
                        / 2.0;
                }
            }
            total.abs()
        };
        // 2 km square: one tile at zoom 10, several at 14.
        let whole = convert_with(
            &map,
            &Options {
                zoom: 10,
                detail_zoom: 10,
                buffer: 0.0,
                ..Options::default()
            },
        );
        assert_eq!(whole.tiles.len(), 1);
        let cut = convert_with(
            &map,
            &Options {
                buffer: 0.0,
                ..Options::default()
            },
        );
        assert!(cut.tiles.len() >= 2, "{} tiles", cut.tiles.len());
        let (a, b) = (area_of(&whole), area_of(&cut));
        assert!((a - b).abs() / a < 0.005, "{a} vs {b}");
        // Each cut ring is closed, and has an edge which is not the map's.
        for tile in cut.tiles.values() {
            for o in &tile.objects {
                let last = o.nodes.last().unwrap();
                assert!(last.flags & node_flag::CLOSE_POINT as u8 != 0);
                assert!(o
                    .nodes
                    .iter()
                    .any(|n| n.flags & node_flag::CLIPPED as u8 != 0));
            }
        }
    }

    /// An object of the symbol with this code.
    fn object_by_code(map: &Map, code: &str, kind: ObjectKind, coords: Vec<Coord>) -> Object {
        let index = map.symbols.iter().position(|s| s.code() == code).unwrap();
        let mut o = Object::new(kind);
        o.symbol_id = map.symbol_ids[index];
        o.symbol_index = Some(index);
        o.coords = coords;
        o
    }

    fn square(x: f64, y: f64, side: f64) -> Vec<Coord> {
        vec![
            Coord::new(x, y, 0),
            Coord::new(x + side, y, 0),
            Coord::new(x + side, y + side, 0),
            Coord::new(x, y + side, 0),
            Coord::new(x, y, coord_flag::CLOSE_POINT),
        ]
    }

    /// Water (a large area from zoom 11), a tiny pond of it, and a tree.
    fn map_for_levels() -> Map {
        let mut map = empty_map();
        let lake = object_by_code(
            &map,
            "301",
            ObjectKind::Path(PathObject::default()),
            square(0.0, 0.0, 100.0),
        );
        map.objects.push(lake);
        // 0.1 mm of paper at 1:10000 is a metre.
        let pond = object_by_code(
            &map,
            "301",
            ObjectKind::Path(PathObject::default()),
            square(300.0, 0.0, 0.1),
        );
        map.objects.push(pond);
        let tree = object_by_code(
            &map,
            "418",
            ObjectKind::Point,
            vec![Coord::new(50.0, 50.0, 0)],
        );
        map.objects.push(tree);
        map
    }

    fn ids_at(converted: &Converted, zoom: u8) -> Vec<u64> {
        let mut ids: Vec<u64> = converted
            .tiles
            .iter()
            .filter(|(id, _)| id.z == zoom)
            .flat_map(|(_, t)| t.objects.iter().map(|o| o.id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    #[test]
    fn a_coarser_level_keeps_what_the_rules_of_the_symbols_say() {
        let map = map_for_levels();
        let levels = convert_levels(
            &map,
            &anchor(),
            &SymbolTable::isom(),
            0,
            11,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(levels.levels.len(), 4);
        // Objects get their ids in order along a curve, so find them by symbol.
        let table = SymbolTable::isom();
        let (water, tree) = (table.id_of("301").unwrap(), table.id_of("418").unwrap());
        let symbols_at = |zoom: u8| -> Vec<u16> {
            let mut v: Vec<u16> = levels
                .tiles
                .iter()
                .filter(|(id, _)| id.z == zoom)
                .flat_map(|(_, t)| t.objects.iter().map(|o| o.symbol))
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        // At the finest level everything is kept: the lake, the pond, the tree.
        assert_eq!(ids_at(&levels, 14).len(), 3);
        assert_eq!(symbols_at(14), {
            let mut v = vec![water, tree];
            v.sort_unstable();
            v
        });
        // Lower down the tree is not shown, and the pond is too small to see.
        for zoom in [11u8, 12, 13] {
            assert_eq!(symbols_at(zoom), vec![water], "zoom {zoom}");
            assert_eq!(ids_at(&levels, zoom).len(), 1, "zoom {zoom}");
        }
        let summary = |zoom| levels.levels.iter().find(|l| l.zoom == zoom).unwrap();
        assert_eq!((summary(14).objects, summary(14).generalized), (3, 0));
        assert_eq!((summary(12).objects, summary(12).generalized), (1, 2));
    }

    #[test]
    fn the_levels_agree_on_the_ids() {
        let map = map_for_levels();
        let levels = convert_levels(
            &map,
            &anchor(),
            &SymbolTable::isom(),
            100,
            12,
            &Options::default(),
        )
        .unwrap();
        let fine = ids_at(&levels, 14);
        assert_eq!(fine.len(), 3);
        assert!(fine.iter().all(|&i| (100..103).contains(&i)));
        let coarse = ids_at(&levels, 12);
        assert!(coarse.iter().all(|i| fine.contains(i)));
        assert_eq!(levels.next_id, 103);
    }

    #[test]
    fn a_coarser_level_is_on_a_coarser_grid_and_means_the_same_place() {
        assert_eq!(level_grid_cm(14, 14, 5), 5);
        assert!(level_grid_cm(12, 14, 5) > 100);
        assert!(level_grid_cm(11, 14, 5) > level_grid_cm(12, 14, 5));
        let map = map_for_levels();
        let levels = convert_levels(
            &map,
            &anchor(),
            &SymbolTable::isom(),
            0,
            12,
            &Options::default(),
        )
        .unwrap();
        // The lake's first corner is in the same place at both levels, to the
        // coarser grid.
        let corner = |zoom: u8| -> (f64, f64, f64) {
            let (id, tile) = levels.tiles.iter().find(|(id, _)| id.z == zoom).unwrap();
            let o = tile
                .objects
                .iter()
                .find(|o| matches!(o.shape, Shape::Path(_)))
                .unwrap();
            let (x, y) = tile.node_mercator(*id, &o.nodes[0]);
            (x, y, f64::from(tile.grid_cm) / 100.0)
        };
        let (fx, fy, _) = corner(14);
        let (cx, cy, cell) = corner(12);
        assert!(cell > 1.0);
        assert!((fx - cx).abs() <= cell && (fy - cy).abs() <= cell);
    }

    #[test]
    fn a_curve_in_a_coarse_level_is_flattened_and_thinned() {
        let mut map = empty_map();
        // A wavy road 1 km long, in many nodes and curves, in a large area's way.
        let coords: Vec<Coord> = (0..200)
            .map(|i| Coord::new(f64::from(i) * 0.5, (f64::from(i) * 0.7).sin() * 0.02, 0))
            .collect();
        let road = object_by_code(&map, "503", ObjectKind::Path(PathObject::default()), coords);
        map.objects.push(road);
        let levels = convert_levels(
            &map,
            &anchor(),
            &SymbolTable::isom(),
            0,
            12,
            &Options::default(),
        )
        .unwrap();
        let nodes = |zoom: u8| -> usize {
            levels
                .tiles
                .iter()
                .filter(|(id, _)| id.z == zoom)
                .flat_map(|(_, t)| t.objects.iter().map(|o| o.nodes.len()))
                .sum()
        };
        assert!(nodes(14) >= 200);
        assert!(nodes(12) > 0 && nodes(12) < 20, "{} nodes", nodes(12));
    }

    #[test]
    fn a_curve_which_is_cut_is_flattened_and_one_which_is_not_stays_a_curve() {
        let mut map = empty_map();
        let curve = |x: f64| {
            vec![
                Coord::new(x, 0.0, coord_flag::CURVE_START),
                Coord::new(x + 50.0, 80.0, 0),
                Coord::new(x + 150.0, -80.0, 0),
                Coord::new(x + 200.0, 0.0, 0),
            ]
        };
        for x in [0.0, 300.0] {
            let line = object_of(
                &map,
                |s| matches!(s, Symbol::Line(_)),
                ObjectKind::Path(PathObject::default()),
                curve(x),
            );
            map.objects.push(line);
        }
        let curves = |converted: &Converted| -> usize {
            converted
                .tiles
                .values()
                .flat_map(|t| &t.objects)
                .filter(|o| {
                    o.nodes
                        .iter()
                        .any(|n| n.flags & node_flag::CURVE_START as u8 != 0)
                })
                .count()
        };
        // In a tile 150 km wide neither is cut.
        let big = convert_with(
            &map,
            &Options {
                zoom: 8,
                detail_zoom: 8,
                ..Options::default()
            },
        );
        assert_eq!(curves(&big), 2);
        // In small ones they cross borders, and the pieces carry no curves.
        let small = convert_with(
            &map,
            &Options {
                zoom: 16,
                buffer: 0.0,
                ..Options::default()
            },
        );
        assert!(small.tiles.len() > 2);
        assert_eq!(curves(&small), 0);
    }

    #[test]
    fn a_point_near_an_edge_is_in_the_tiles_whose_buffer_holds_it() {
        let mut map = empty_map();
        let o = object_of(
            &map,
            |s| matches!(s, Symbol::Point(_)),
            ObjectKind::Point,
            vec![Coord::new(0.0, 0.0, 0)],
        );
        map.objects.push(o);
        let none = convert_with(
            &map,
            &Options {
                buffer: 0.0,
                ..Options::default()
            },
        );
        assert_eq!(none.tiles.len(), 1);
        // With half a tile of buffer a point is within reach of a neighbour.
        let wide = convert_with(
            &map,
            &Options {
                buffer: 0.5,
                ..Options::default()
            },
        );
        assert!(wide.tiles.len() >= 2, "{} tiles", wide.tiles.len());
        assert_eq!(wide.objects, 1);
    }

    #[test]
    fn flags_curves_and_holes_are_kept() {
        let mut map = empty_map();
        let coords = vec![
            Coord::new(0.0, 0.0, coord_flag::CURVE_START),
            Coord::new(1.0, 1.0, 0),
            Coord::new(2.0, 1.0, 0),
            Coord::new(3.0, 0.0, coord_flag::HOLE_POINT),
            Coord::new(10.0, 10.0, 0),
            Coord::new(11.0, 10.0, coord_flag::CLOSE_POINT),
        ];
        let o = object_of(
            &map,
            |s| matches!(s, Symbol::Area(_)),
            ObjectKind::Path(PathObject::default()),
            coords,
        );
        map.objects.push(o);
        let converted = convert_map(&map);
        let tile = converted.tiles.values().next().unwrap();
        let flags: Vec<u8> = tile.objects[0].nodes.iter().map(|n| n.flags).collect();
        assert_eq!(
            flags,
            vec![
                node_flag::CURVE_START as u8,
                0,
                0,
                node_flag::HOLE_POINT as u8,
                0,
                node_flag::CLOSE_POINT as u8
            ]
        );
        assert!(
            matches!(tile.objects[0].shape, Shape::Path(Some(_))),
            "an area with a fill pattern keeps its pattern origin"
        );
    }

    #[test]
    fn a_rotation_is_turned_with_the_map() {
        let mut map = empty_map();
        for rotation in [0.0, std::f64::consts::FRAC_PI_2] {
            let mut o = object_of(
                &map,
                |s| matches!(s, Symbol::Point(_)),
                ObjectKind::Point,
                vec![Coord::new(0.0, 0.0, 0)],
            );
            o.rotation = rotation;
            map.objects.push(o);
        }
        let converted = convert_map(&map);
        let tile = converted.tiles.values().next().unwrap();
        let turned = tile.objects[1].rotation - tile.objects[0].rotation;
        assert!(
            (turned - std::f64::consts::FRAC_PI_2).abs() < 1e-3,
            "{turned}"
        );
    }

    #[test]
    fn the_map_survives_encoding_and_decoding() {
        let mut map = empty_map();
        for i in 0..50 {
            let mut o = object_of(
                &map,
                |s| matches!(s, Symbol::Line(_)),
                ObjectKind::Path(PathObject::default()),
                (0..8)
                    .map(|k| Coord::new(f64::from(i) * 3.0 + f64::from(k), f64::from(k * k), 0))
                    .collect(),
            );
            o.rotation = 0.0;
            map.objects.push(o);
        }
        let converted = convert_map(&map);
        let mut seen = std::collections::HashSet::new();
        for tile in converted.tiles.values() {
            let back = Tile::decode(&tile.encode()).unwrap();
            let mut expected = tile.clone();
            expected.objects.sort_by_key(|o| (o.symbol, o.id));
            assert_eq!(back.objects.len(), expected.objects.len());
            for (a, b) in back.objects.iter().zip(&expected.objects) {
                assert_eq!((a.id, a.symbol, &a.nodes), (b.id, b.symbol, &b.nodes));
                seen.insert(a.id);
            }
        }
        assert_eq!(seen.len(), 50, "every object is in some tile");
    }

    #[test]
    fn an_easting_outside_the_zone_is_flagged() {
        let mut map = empty_map();
        let o = object_of(
            &map,
            |s| matches!(s, Symbol::Point(_)),
            ObjectKind::Point,
            vec![Coord::new(0.0, 0.0, 0)],
        );
        map.objects.push(o);
        let fine = convert_map(&map);
        assert!(fine.warnings.iter().all(|w| !w.contains("easting")));
        map.georeferencing.as_mut().unwrap().ref_point_x = 0.0;
        let broken = convert_map(&map);
        assert!(
            broken.warnings.iter().any(|w| w.contains("easting")),
            "{:?}",
            broken.warnings
        );
    }

    #[test]
    fn an_unplaced_map_is_an_error() {
        let mut map = empty_map();
        map.georeferencing = None;
        assert!(convert(
            &map,
            &anchor(),
            &SymbolTable::isom(),
            0,
            &Options::default()
        )
        .is_err());
    }

    #[test]
    fn a_symbol_with_nothing_close_is_dropped_with_a_warning() {
        let mut map = empty_map();
        map.symbols.push(Symbol::Point(crate::map::PointSymbol {
            code: "9999".into(),
            name: "no such name".into(),
            ..Default::default()
        }));
        map.symbol_ids.push(99999);
        let index = map.symbols.len() - 1;
        let mut o = Object::new(ObjectKind::Point);
        o.symbol_id = 99999;
        o.symbol_index = Some(index);
        o.coords = vec![Coord::new(0.0, 0.0, 0)];
        map.objects.push(o);
        let converted = convert_map(&map);
        assert_eq!(converted.dropped, 1);
        assert_eq!(converted.objects, 0);
        assert!(
            converted.warnings.iter().any(|w| w.contains("9999")),
            "{:?}",
            converted.warnings
        );
    }
}
