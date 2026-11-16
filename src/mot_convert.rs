//! Makes MOT tiles from a map.
//!
//! The one direction of the codec that touches the [`Map`] model: every
//! object's paper coordinates are placed on the earth, turned into Web
//! Mercator, given the id of its symbol in a [`SymbolTable`], and handed to
//! every tile it touches. See `mds/MOT-Format.md`.
//!
//! Version 0 does not clip: an object is stored whole in each tile its box
//! touches, so one that crosses a border is in both, with the same
//! [`TileObject::id`], and a reader drawing a tile draws it clipped to the
//! tile's own edge. The clipped edges and the cut dashes the format reserves
//! room for come with clipping.
//!
//! The map is placed from its [`GeographicAnchor`] (the reference point in
//! degrees) laid out on the ellipsoid, turned by the map's grivation, as
//! [`crate::orthophoto`] does it. That is good to a few meters in ten
//! kilometers, which is enough to put a map in the right tile but not to
//! overlay it exactly on a survey of the same ground.

use std::collections::BTreeMap;

use crate::map::{Map, ObjectKind, Symbol};
use crate::mot::{Grid, Node, Pattern, Shape, Text, Tile, TileId, TileObject, WORLD_HALF};
use crate::mot_symbols::{Match, SymbolTable};
use crate::orthophoto::{
    ground_to_geographic, true_north_rotation, web_mercator, GeographicAnchor,
};

/// What to build.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The zoom level of the tiles.
    pub zoom: u8,
    /// The resolution of the nodes, in centimetres of Mercator.
    pub grid_cm: u32,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            zoom: 14,
            grid_cm: 5,
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
    /// How many objects were stored under another symbol than their own.
    pub remapped: usize,
    /// One line for every symbol that was remapped or dropped, and for
    /// anything else worth knowing.
    pub warnings: Vec<String>,
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
    let grid = Grid::new(options.grid_cm);
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
        remapped: 0,
        warnings: Vec::new(),
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

        let places: Vec<(f64, f64)> = object.coords.iter().map(|c| project(c.x, c.y)).collect();
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

        let id = converted.next_id;
        converted.next_id += 1;
        converted.objects += 1;
        for tile_id in TileId::covering(options.zoom, bounds) {
            let (west, top) = tile_id.origin();
            let to_grid = |(x, y): (f64, f64)| (grid.units(x - west), grid.units(top - y));
            let nodes: Vec<Node> = object
                .coords
                .iter()
                .zip(&places)
                .map(|(c, &p)| {
                    let (x, y) = to_grid(p);
                    Node {
                        x,
                        y,
                        flags: c.flags as u8,
                    }
                })
                .collect();
            let shape = match &object.kind {
                ObjectKind::Point => Shape::Point,
                ObjectKind::Path(_) => {
                    Shape::Path(pattern_place.map(|(rotation, origin)| Pattern {
                        rotation: rotation + frame,
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
            converted
                .tiles
                .entry(tile_id)
                .or_insert_with(|| Tile::new(options.grid_cm))
                .objects
                .push(TileObject {
                    id,
                    symbol: id_in_table,
                    rotation: object.rotation + frame,
                    shape,
                    nodes,
                });
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

    #[test]
    fn an_object_across_a_border_is_in_both_tiles_with_one_id() {
        let mut map = empty_map();
        // At 1:10000 a millimetre of paper is 10 m, so 500 mm is 5 km: wider
        // than a zoom 14 tile (about 1.7 km at this latitude).
        let line = object_of(
            &map,
            |s| matches!(s, Symbol::Line(_)),
            ObjectKind::Path(PathObject::default()),
            vec![Coord::new(0.0, 0.0, 0), Coord::new(500.0, 0.0, 0)],
        );
        map.objects.push(line);
        let converted = convert_map(&map);
        assert!(
            converted.tiles.len() >= 3,
            "{} tiles",
            converted.tiles.len()
        );
        let mut ends = Vec::new();
        for (id, tile) in &converted.tiles {
            assert_eq!(tile.objects.len(), 1);
            assert_eq!(tile.objects[0].id, 0);
            let n = &tile.objects[0].nodes;
            ends.push((
                tile.node_mercator(*id, &n[0]),
                tile.node_mercator(*id, &n[1]),
            ));
        }
        // Every tile agrees on where the line is.
        for e in &ends[1..] {
            assert!((e.0 .0 - ends[0].0 .0).abs() < 0.1 && (e.1 .0 - ends[0].1 .0).abs() < 0.1);
        }
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
