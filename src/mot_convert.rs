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
//! line into pieces (each says where it was cut), an area into a ring closed
//! along the edge. All the pieces have the object's [`TileObject::id`]. A line
//! whose symbol lays dashes or symbols out along the whole of it
//! ([`crate::mot_symbols::SymbolEntry::whole`]) is not cut but kept whole in
//! every tile it crosses. A point or a text is in every tile whose grown box
//! holds it.
//!
//! A map in a UTM zone is placed exactly as Mapper places it
//! ([`paper_to_geographic`]: paper to the projected CRS by the
//! georeferencing, then off it by the projection), however far its objects
//! are from its reference point. Any other map is placed from its
//! [`GeographicAnchor`] (the reference point in degrees) laid out on the
//! ellipsoid, turned by the map's grivation, as [`crate::orthophoto`] does it,
//! which is good to a few meters in ten kilometers.

use std::collections::{BTreeMap, HashMap};

use crate::map::{FillPattern, Georeferencing, Map, ObjectKind, PartRef, Point, Symbol};
use crate::mot::node_flag;
use crate::mot::{
    ground_angle, Grid, Node, Pattern, Shape, Text, Tile, TileId, TileObject, COVERAGE_SYMBOL,
    WORLD_HALF,
};
use crate::mot_adapt::{adapt, Adapted};
use crate::mot_clip::{clip_line, clip_ring, flatten, simplify, Rect, Vertex};
use crate::mot_lattice::{from_grid, grid_turn, keeps_its_map, nearest_origin};
use crate::mot_symbols::{Match, SymbolTable, ZoomRule};
use crate::ocd::{is_ocd_file, ocd_to_omap_xml};
use crate::orthophoto::{
    geographic_anchor, ground_to_geographic, paper_to_geographic, true_north_rotation,
    web_mercator, GeographicAnchor,
};
use crate::xml_reader::read_xml_map_str;

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
    /// drawing aid, which draws nothing on the printed map, or because they are
    /// texts: the tiles keep none (a map's texts are its title, its credits,
    /// its scale, which mean nothing on the one map of the world).
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
    /// The id of the area that says what the map covers, where it has one: the
    /// object of the [`COVERAGE_SYMBOL`] in the tiles.
    pub coverage_id: Option<u64>,
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

/// The box a map covers, as `[west, north, east, south]` in paper millimetres
/// (`y` down): the box round the nodes of the objects it draws, without the
/// outermost 0.05% on each side so that a stray object far from the rest does not
/// stretch it, and a margin of 1% of its size. `None` for a map with nothing on it.
///
/// A map is drawn on a page and a page is a rectangle, so a rectangle in the
/// map's own frame (which the projection turns, as it turns the map) is a fair
/// footprint of what it covers.
pub fn paper_extent(map: &Map) -> Option<[f64; 4]> {
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for o in &map.objects {
        let visible = o
            .symbol_index
            .and_then(|i| map.symbols.get(i))
            .is_some_and(|s| s.is_visible());
        if visible {
            for c in &o.coords {
                xs.push(c.x);
                ys.push(c.y);
            }
        }
    }
    if xs.is_empty() {
        return None;
    }
    // The value `cut` from each end, found by selection: no need to sort millions.
    let trim = |v: &mut Vec<f64>| -> (f64, f64) {
        let cut = v.len() / 2000;
        let top = v.len() - 1 - cut;
        let hi = *v.select_nth_unstable_by(top, f64::total_cmp).1;
        let lo = *v.select_nth_unstable_by(cut, f64::total_cmp).1;
        let margin = (hi - lo) * 0.01;
        (lo - margin, hi + margin)
    };
    let (x0, x1) = trim(&mut xs);
    let (y0, y1) = trim(&mut ys);
    Some([x0, y0, x1, y1])
}

/// Where a map's paper lies on the earth, exactly as [`convert`] places it
/// (see the module's doc): for drawing the map itself where its tiles are.
pub struct Placement<'m> {
    georef: &'m Georeferencing,
    anchor: GeographicAnchor,
    north: f64,
    meters_per_mm: f64,
}

impl<'m> Placement<'m> {
    /// `map`, placed at `anchor`; an error where it has no georeferencing.
    pub fn new(map: &'m Map, anchor: &GeographicAnchor) -> Result<Placement<'m>, String> {
        let georef = map
            .georeferencing
            .as_ref()
            .ok_or("the map is not georeferenced")?;
        let scale = if georef.scale > 0 {
            georef.scale
        } else {
            map.scale_denominator
        };
        Ok(Placement {
            georef,
            anchor: *anchor,
            north: true_north_rotation(georef, anchor),
            meters_per_mm: f64::from(scale) / 1000.0,
        })
    }

    /// A point on the paper (mm, `y` down) as `(latitude, longitude)` in degrees.
    pub fn geographic(&self, x: f64, y: f64) -> (f64, f64) {
        paper_to_geographic(self.georef, (x, y)).unwrap_or_else(|| {
            ground_to_geographic(
                self.georef,
                &self.anchor,
                self.north,
                (x * self.meters_per_mm, y * self.meters_per_mm),
            )
        })
    }

    /// A point on the paper in Mercator meters, `y` growing north.
    pub fn mercator(&self, x: f64, y: f64) -> (f64, f64) {
        let (lat, lon) = self.geographic(x, y);
        web_mercator(lat, lon)
    }

    /// The point of the paper placed at `target` (Mercator meters): the inverse
    /// of [`mercator`](Self::mercator), by Newton's method from `guess`. The
    /// placement is all but affine, so it takes two or three steps.
    pub fn paper(&self, target: (f64, f64), guess: (f64, f64)) -> (f64, f64) {
        let mut p = guess;
        for _ in 0..8 {
            let m = self.mercator(p.0, p.1);
            let (dx, dy) = (target.0 - m.0, target.1 - m.1);
            // d(Mercator) / d(paper) at p.
            let h = 0.5;
            let (xp, xm) = (self.mercator(p.0 + h, p.1), self.mercator(p.0 - h, p.1));
            let (yp, ym) = (self.mercator(p.0, p.1 + h), self.mercator(p.0, p.1 - h));
            let j = [
                [(xp.0 - xm.0) / (2.0 * h), (yp.0 - ym.0) / (2.0 * h)],
                [(xp.1 - xm.1) / (2.0 * h), (yp.1 - ym.1) / (2.0 * h)],
            ];
            let det = j[0][0] * j[1][1] - j[0][1] * j[1][0];
            if det.abs() < 1e-12 {
                break;
            }
            let step = (
                (j[1][1] * dx - j[0][1] * dy) / det,
                (j[0][0] * dy - j[1][0] * dx) / det,
            );
            p = (p.0 + step.0, p.1 + step.1);
            if step.0.abs() + step.1.abs() < 1e-6 {
                break;
            }
        }
        p
    }
}

/// A map file as `mauro tiles build` reads it: the map, and where on the earth
/// it is, `None` where the file does not say. A Mapper file says so beside its
/// projected CRS (`<ref_point_deg>`); an OCAD file is converted first
/// ([`crate::ocd`]) and placed from its reference point where its CRS is a UTM
/// zone, the only ones OCAD's georeferencing gives without degrees that this
/// crate projects.
pub fn read_placed_map(bytes: &[u8]) -> Result<(Map, Option<GeographicAnchor>), String> {
    let text = if is_ocd_file(bytes) {
        ocd_to_omap_xml(bytes)?.xml
    } else {
        String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?
    };
    let (map, _) = read_xml_map_str(&text)?;
    let anchor = geographic_anchor(&text).or_else(|| {
        let g = map.georeferencing.as_ref()?;
        let (lat, lon) = paper_to_geographic(g, (g.map_ref_point_x, g.map_ref_point_y))?;
        Some(GeographicAnchor {
            lat,
            lon,
            declination: 0.0,
        })
    });
    Ok((map, anchor))
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
    let adapted = adapt(map, table);
    let mut result = convert_cut(
        map,
        anchor,
        table,
        first_id,
        options,
        paper_extent(map),
        &adapted,
    )?;
    result.warnings.extend(adapted.warnings);
    Ok(result)
}

/// [`convert`] with the box the map covers given (see [`paper_extent`]), which a
/// pyramid works out once for all its levels. The box is stored in the tiles
/// as an area of the [`COVERAGE_SYMBOL`], with an id of its own after the
/// objects'.
pub fn convert_with_extent(
    map: &Map,
    anchor: &GeographicAnchor,
    table: &SymbolTable,
    first_id: u64,
    options: &Options,
    extent: Option<[f64; 4]>,
) -> Result<Converted, String> {
    convert_cut(
        map,
        anchor,
        table,
        first_id,
        options,
        extent,
        &Adapted::default(),
    )
}

/// [`convert_with_extent`] with the objects adapted to the standard's symbols
/// (see [`crate::mot_adapt`]): the coordinates of those cut or turned round in
/// place of their own, and the objects made for them stored too.
fn convert_cut(
    map: &Map,
    anchor: &GeographicAnchor,
    table: &SymbolTable,
    first_id: u64,
    options: &Options,
    extent: Option<[f64; 4]>,
    adapted: &Adapted,
) -> Result<Converted, String> {
    let placement = Placement::new(map, anchor)?;
    let georef = placement.georef;
    let meters_per_mm = placement.meters_per_mm;
    let grid_cm = level_grid_cm(options.zoom, options.detail_zoom, options.grid_cm);
    let grid = Grid::new(grid_cm);
    let coarse = options.zoom < options.detail_zoom;
    // A pixel of a 256 pixel tile of this level, in Mercator meters.
    let pixel = TileId::size(options.zoom) / 256.0;

    // paper mm -> (lat, lon)
    let place = |x: f64, y: f64| placement.geographic(x, y);
    // paper mm -> Mercator meters, y growing north
    let project = |x: f64, y: f64| placement.mercator(x, y);

    // Where the map is, on the paper: the middle of what it covers (its
    // reference point may be far from it).
    let middle = extent.map_or(
        (georef.map_ref_point_x, georef.map_ref_point_y),
        |[west, north, east, south]| ((west + east) / 2.0, (north + south) / 2.0),
    );
    // A Mercator meter is longer than a ground meter by this much there.
    let stretch = 1.0 / place(middle.0, middle.1).0.to_radians().cos().max(0.05);
    // The angle from a tile's east to the map's own east there, which every
    // rotation in the map has to be turned by: counterclockwise, as Mapper
    // measures rotations (Mercator's `y` grows north, so it is the usual angle).
    let (a, b) = (
        project(middle.0, middle.1),
        project(middle.0 + 1.0, middle.1),
    );
    let frame = ground_angle(a, b, place(middle.0, middle.1).0);

    // What each symbol of the map becomes, worked out once, and for one stored
    // as another symbol, the note saying so (with its count, once known).
    let mut mapped: Vec<Option<u16>> = Vec::with_capacity(map.symbols.len());
    let mut notes: Vec<Option<String>> = Vec::with_capacity(map.symbols.len());
    for symbol in &map.symbols {
        let found = table.closest(symbol, map);
        mapped.push(found.map(|(id, _)| id));
        notes.push(match found {
            Some((id, how)) if how != Match::Code => {
                let target = table.get(id).expect("an id of the table");
                Some(format!(
                    "symbol {} stored as {} \"{}\" ({})",
                    describe(symbol),
                    target.code,
                    target.name,
                    match how {
                        Match::Parent => "the symbol it is a variant of".to_string(),
                        Match::Table => format!(
                            "line {} of the symbol map",
                            table.crossref_line(symbol, map).unwrap_or(0)
                        ),
                        _ => "the same name".to_string(),
                    }
                ))
            }
            _ => None,
        });
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
        coverage_id: None,
    };
    // A UTM easting a long way outside its zone's strip means the file's
    // georeference has slipped (an easting of 0 is the usual one). Mapper
    // still shows the objects in place, and so are they here; but its
    // grivation, scale factor and geographic reference point were worked out
    // for that far point.
    let utm_zone = matches!(georef.epsg, 32601..=32660 | 32701..=32760 | 25828..=25838);
    if utm_zone && !(100_000.0..=900_000.0).contains(&georef.ref_point_x) {
        let (lat, lon) = place(middle.0, middle.1);
        converted.warnings.push(format!(
            "the map's projected reference point has an easting of {}, far outside its UTM zone: \
             the map is placed as Mapper places it (around lat {lat:.4}, lon {lon:.4}), but its \
             georeference is worth setting again in Mapper, with a reference point on the map",
            georef.ref_point_x
        ));
    }
    // The point of the paper in the middle of each tile, for the patterns.
    let mut near_tile: HashMap<TileId, (f64, f64)> = HashMap::new();
    let mut remapped_symbols = vec![0usize; map.symbols.len()];
    let mut dropped_symbols = vec![0usize; map.symbols.len()];
    let mut texts = 0usize;

    // Ids are given in order along a Z-order curve through each object's first
    // node. Inside a tile objects are sorted by (symbol, id), so this keeps
    // both the ids and the first nodes of neighbours in the file close, and
    // so cheap to code as differences.
    // The map's objects, then those made for them (by position past the map's).
    let object_at = |position: usize| -> (&crate::map::Object, Option<u16>) {
        match position.checked_sub(map.objects.len()) {
            None => (&map.objects[position], None),
            Some(extra) => {
                let (object, id) = &adapted.extras[extra];
                (object, Some(*id))
            }
        }
    };
    let mut order: Vec<(u64, usize)> = map
        .objects
        .iter()
        .chain(adapted.extras.iter().map(|(o, _)| o))
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
        let (object, made) = object_at(position);
        if matches!(object.kind, ObjectKind::Text(_)) {
            converted.hidden += 1;
            texts += 1;
            continue;
        }
        let Some(index) = object.symbol_index else {
            converted.dropped += 1;
            continue;
        };
        let symbol = &map.symbols[index];
        let coords = adapted.coords.get(&position).unwrap_or(&object.coords);
        if !symbol.is_visible() || coords.is_empty() {
            converted.hidden += 1;
            continue;
        }
        let Some(id_in_table) = made.or(mapped[index]) else {
            converted.dropped += 1;
            dropped_symbols[index] += 1;
            continue;
        };
        if made.is_none()
            && table
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
        // Kept whole at the finest level only: a coarser one draws its dashes
        // too small to tell, and is thinned anyway.
        let keep_whole = !coarse && table.get(id_in_table).is_some_and(|e| e.whole);
        let closed = flags_closed(coords);
        if coarse && options.zoom < rule.min_zoom {
            converted.generalized += 1;
            continue;
        }
        let mut places: Vec<(f64, f64)> = coords.iter().map(|c| project(c.x, c.y)).collect();
        let mut flags: Vec<i32> = coords.iter().map(|c| c.flags).collect();
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
        // The patterns of an area are laid out on the world's lattice when the
        // tile is drawn (`mot_lattice`); the tile keeps only a turn the mapper
        // gave them. The north lines keep their map's lattice instead, from a
        // point of it near each tile (below), not one for the whole object: the
        // paper is not quite flat on the earth, and a lattice laid out from far
        // away is off by a fraction of its period by the time it reaches the tile.
        let own_lattice = table
            .get(id_in_table)
            .is_some_and(|e| keeps_its_map(&e.code));
        let pattern_place = match &object.kind {
            ObjectKind::Path(p)
                if wants_pattern(p.pattern_rotation, (p.pattern_origin.x, p.pattern_origin.y)) =>
            {
                let mut patterns = Vec::new();
                fill_patterns(map, symbol, &mut patterns);
                // The tiles turn every pattern with the map (see
                // `mot_render`); the object's own turn is for those that
                // follow it.
                let turned = patterns.is_empty() || patterns.iter().any(|p| p.rotatable);
                // The object's box on the paper, which the point the lattice is
                // laid out near stays in.
                let (mut lo, mut hi) = (
                    Point::new(f64::MAX, f64::MAX),
                    Point::new(f64::MIN, f64::MIN),
                );
                for c in coords {
                    lo = Point::new(lo.x.min(c.x), lo.y.min(c.y));
                    hi = Point::new(hi.x.max(c.x), hi.y.max(c.y));
                }
                Some((
                    (lo, hi),
                    patterns,
                    p.pattern_rotation,
                    p.pattern_origin,
                    if turned { p.pattern_rotation } else { 0.0 },
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

            // What goes in this tile: the nodes of each piece of the object.
            let mut stored: Vec<Vec<Node>> = Vec::new();
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
                ObjectKind::Path(_) => Shape::Path(pattern_place.as_ref().map(
                    |((lo, hi), patterns, rotation, origin, stored)| {
                        let near = *near_tile.entry(tile_id).or_insert_with(|| {
                            let (west, top) = tile_id.origin();
                            let half = TileId::size(options.zoom) / 2.0;
                            placement.paper((west + half, top - half), middle)
                        });
                        let near = Point::new(near.0.clamp(lo.x, hi.x), near.1.clamp(lo.y, hi.y));
                        if !own_lattice {
                            // On the world's lattice (`mot_lattice`), which the
                            // renderer lays it out on: only a turn the mapper gave
                            // it is kept, as one from grid north.
                            let (a, b) = (project(near.x, near.y), project(near.x + 1.0, near.y));
                            let turn = ground_angle(a, b, place(near.x, near.y).0);
                            return Pattern {
                                rotation: if *stored == 0.0 {
                                    0.0
                                } else {
                                    from_grid(stored + turn - grid_turn(a))
                                },
                                origin: (0, 0),
                            };
                        }
                        // The north lines keep their map's: laid out from a point of
                        // its lattice near the tile, turned as the map is there.
                        let at = nearest_origin(patterns, *rotation, *origin, near);
                        // The map's turn there, not at its middle: the
                        // meridians converge across a map, by some 0.04° over
                        // 5 km, and a pattern turned by the middle's slides
                        // off its paper's lattice a few hundred metres away.
                        let (a, b) = (project(at.x, at.y), project(at.x + 1.0, at.y));
                        let turn = ground_angle(a, b, place(at.x, at.y).0);
                        Pattern {
                            rotation: stored + turn,
                            origin: to_grid(a),
                        }
                    },
                )),
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
                        stored.push(whole(&locals));
                    }
                }
                ObjectKind::Path(_) if locals.iter().all(inside) => {
                    stored.push(whole(&locals));
                }
                // A line spaced out along its whole length is kept whole in
                // every tile it passes through; the control polygon of a curve
                // contains it, so a line it misses does not touch the tile.
                ObjectKind::Path(_) if keep_whole && !closed => {
                    let polygon: Vec<Vertex> = locals
                        .iter()
                        .map(|&(x, y)| Vertex { x, y, flags: 0 })
                        .collect();
                    if locals.iter().any(inside) || !clip_line(&polygon, &rect).is_empty() {
                        stored.push(whole(&locals));
                    }
                }
                ObjectKind::Path(_) if keep_whole => stored.push(whole(&locals)),
                ObjectKind::Path(_) => clip_path(&flags, &locals, &rect, &mut stored),
            }
            if stored.is_empty() {
                continue;
            }
            let tile = converted
                .tiles
                .entry(tile_id)
                .or_insert_with(|| Tile::new(grid_cm));
            for nodes in stored {
                tile.objects.push(TileObject {
                    id,
                    symbol: id_in_table,
                    rotation,
                    shape: shape.clone(),
                    nodes,
                });
            }
        }
    }

    if let Some([x0, y0, x1, y1]) = extent {
        let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].map(|(x, y)| project(x, y));
        let id = converted.next_id;
        converted.next_id += 1;
        converted.coverage_id = Some(id);
        store_coverage(&mut converted.tiles, &corners, id, options, grid_cm);
    }

    if texts > 0 {
        converted.warnings.push(format!(
            "texts are left out, the tiles keep none: {texts} objects"
        ));
    }
    // `notes` and `remapped_symbols` both have one entry per symbol of the map.
    converted.warnings.extend(
        notes
            .into_iter()
            .zip(&remapped_symbols)
            .filter(|(_, &n)| n > 0)
            .filter_map(|(note, n)| note.map(|note| format!("{note}: {n} objects"))),
    );
    for (index, &n) in dropped_symbols.iter().enumerate() {
        if n == 0 {
            continue;
        }
        let symbol = &map.symbols[index];
        converted
            .warnings
            .push(match table.crossref_line(symbol, map) {
                Some(line) if table.left_out(symbol, map) => format!(
                    "symbol {} is left out by line {line} of the symbol map: {n} objects",
                    describe(symbol)
                ),
                _ => format!(
                    "symbol {} has no close symbol in {}, so its {} objects were dropped",
                    describe(symbol),
                    table.standard,
                    n
                ),
            });
    }
    Ok(converted)
}

/// Puts the ring `corners` (Mercator meters) in every tile of the level it
/// touches, cut to the tile's box and its buffer like an area, as an object of
/// the [`COVERAGE_SYMBOL`].
fn store_coverage(
    tiles: &mut BTreeMap<TileId, Tile>,
    corners: &[(f64, f64); 4],
    id: u64,
    options: &Options,
    grid_cm: u32,
) {
    let per_unit = 100.0 / f64::from(grid_cm);
    let side = TileId::size(options.zoom) * per_unit;
    let margin = options.buffer.max(0.0) * side;
    let rect = Rect {
        x0: -margin,
        y0: -margin,
        x1: side + margin,
        y1: side + margin,
    };
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(x, y) in corners {
        (x0, x1) = (x0.min(x), x1.max(x));
        (y0, y1) = (y0.min(y), y1.max(y));
    }
    let grow = options.buffer.max(0.0) * TileId::size(options.zoom);
    for tile_id in TileId::covering(options.zoom, [x0 - grow, y0 - grow, x1 + grow, y1 + grow]) {
        let (west, top) = tile_id.origin();
        let ring: Vec<Vertex> = corners
            .iter()
            .map(|&(x, y)| Vertex {
                x: (x - west) * per_unit,
                y: (top - y) * per_unit,
                flags: 0,
            })
            .collect();
        let cut = clip_ring(&ring, &rect);
        let Some(first) = cut.first() else { continue };
        let mut nodes: Vec<Node> = cut
            .iter()
            .map(|v| Node {
                x: v.x.round() as i32,
                y: v.y.round() as i32,
                flags: v.flags,
            })
            .collect();
        nodes.push(Node {
            x: first.x.round() as i32,
            y: first.y.round() as i32,
            flags: node_flag::CLOSE_POINT as u8,
        });
        tiles
            .entry(tile_id)
            .or_insert_with(|| Tile::new(grid_cm))
            .objects
            .push(TileObject {
                id,
                symbol: COVERAGE_SYMBOL,
                rotation: 0.0,
                shape: Shape::Path(None),
                nodes,
            });
    }
}

/// Whether a path has a closed part: an area, or a ring drawn as a line.
/// `map` with its fill patterns laid out as its tiles lay them out (on the world's
/// lattice, [`crate::mot_lattice`]), not on its own paper: what the map is compared
/// with its tiles as, the patterns of each in the same places.
///
/// Each patterned area is laid out from the point of the lattice nearest its
/// middle (in the patterns of the symbol its tiles have, at the symbol set's
/// scale), square to the grid's north or turned as far from it as the mapper
/// turned it; its symbols' patterns are all made rotatable so that this is what
/// Mapper's renderer does. The north lines are left as they are, on the map's
/// paper. An error where the map cannot be placed.
pub fn lay_out_on_the_world_lattice(
    map: &mut Map,
    anchor: &GeographicAnchor,
    table: &SymbolTable,
) -> Result<(), String> {
    let (template, _) = crate::xml_reader::read_xml_map_str(table.reference())?;
    let scale = f64::from(template.scale_denominator);
    let mut placed: Vec<(usize, f64, Point)> = Vec::new();
    {
        let placement = Placement::new(map, anchor)?;
        // The direction of the paper's east at the paper point `p`, on the ground.
        let paper_turn = |p: Point| {
            let a = placement.mercator(p.x, p.y);
            let b = placement.mercator(p.x + 1.0, p.y);
            ground_angle(a, b, crate::mot::lat_lon(a.0, a.1).0)
        };
        for (i, object) in map.objects.iter().enumerate() {
            let ObjectKind::Path(path) = &object.kind else {
                continue;
            };
            let Some(symbol) = object.symbol_index.and_then(|k| map.symbols.get(k)) else {
                continue;
            };
            let mut own = Vec::new();
            fill_patterns(map, symbol, &mut own);
            if own.is_empty() || keeps_its_map(symbol.code()) || object.coords.is_empty() {
                continue;
            }
            let Some((id, _)) = table.closest(symbol, map) else {
                continue;
            };
            let Some(code) = table.get(id).map(|e| &e.code) else {
                continue;
            };
            let Some(theirs) = template
                .symbols
                .iter()
                .find(|s| s.code() == code && !s.is_helper_symbol())
            else {
                continue;
            };
            let mut patterns = Vec::new();
            fill_patterns(&template, theirs, &mut patterns);
            if patterns.is_empty() {
                continue;
            }
            let (mut lo, mut hi) = (
                Point::new(f64::MAX, f64::MAX),
                Point::new(f64::MIN, f64::MIN),
            );
            for c in &object.coords {
                lo = Point::new(lo.x.min(c.x), lo.y.min(c.y));
                hi = Point::new(hi.x.max(c.x), hi.y.max(c.y));
            }
            let middle = Point::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
            let m = placement.mercator(middle.x, middle.y);
            // As the tiles keep it (`convert`): only a turn the mapper gave it.
            let turned = own.iter().any(|p| p.rotatable);
            let rotation = if !turned || path.pattern_rotation == 0.0 {
                0.0
            } else {
                from_grid(path.pattern_rotation + paper_turn(middle) - grid_turn(m))
            };
            let at = crate::mot_lattice::anchor(&patterns, rotation, scale, m);
            let (x, y) = placement.paper(at, (middle.x, middle.y));
            let origin = Point::new(x, y);
            placed.push((i, rotation + grid_turn(at) - paper_turn(origin), origin));
        }
    }
    fn rotatable(symbols: &mut [Symbol]) {
        for symbol in symbols {
            if keeps_its_map(symbol.code()) {
                continue;
            }
            match symbol {
                Symbol::Area(a) => a.patterns.iter_mut().for_each(|p| p.rotatable = true),
                Symbol::Combined(c) => rotatable(&mut c.owned_parts),
                _ => {}
            }
        }
    }
    rotatable(&mut map.symbols);
    for (i, rotation, origin) in placed {
        if let ObjectKind::Path(p) = &mut map.objects[i].kind {
            p.pattern_rotation = rotation;
            p.pattern_origin = origin;
        }
    }
    Ok(())
}

/// The fill patterns `symbol` lays out, those of a combined symbol's parts
/// included.
pub fn fill_patterns<'a>(map: &'a Map, symbol: &'a Symbol, out: &mut Vec<&'a FillPattern>) {
    match symbol {
        Symbol::Area(a) => out.extend(a.patterns.iter()),
        Symbol::Combined(c) => {
            for part in &c.parts {
                let part = match *part {
                    PartRef::Shared(i) => map.symbols.get(i),
                    PartRef::Private(i) => c.owned_parts.get(i),
                    PartRef::None => None,
                };
                // A combined symbol may not hold another combined one, but a
                // file can say anything: one level deep only.
                if let Some(Symbol::Area(a)) = part {
                    out.extend(a.patterns.iter());
                }
            }
        }
        _ => {}
    }
}

fn flags_closed(coords: &[crate::map::Coord]) -> bool {
    coords.iter().any(|c| c.flags & node_flag::CLOSE_POINT != 0)
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

/// The parts of a path inside `rect`, as the nodes of each, pushed to `stored`.
///
/// `locals` are the path's coordinates in the tile's grid units. The path is
/// flattened first. A closed part is cut as a ring, and all of them make one
/// object, as the rings of an area do; an open one is cut into line pieces,
/// each its own.
fn clip_path(flags: &[i32], locals: &[(f64, f64)], rect: &Rect, stored: &mut Vec<Vec<Node>>) {
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
                stored.push(nodes);
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
        stored.insert(0, area);
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
    // Once for every level: the objects adapted to the standard's symbols.
    let adapted = adapt(map, table);
    let extent = paper_extent(map);
    let mut result = convert_cut(
        map,
        anchor,
        table,
        first_id,
        &Options {
            detail_zoom: detail,
            ..*options
        },
        extent,
        &adapted,
    )?;
    result.warnings.extend(adapted.warnings.iter().cloned());
    result.levels.push(LevelSummary {
        zoom: detail,
        objects: result.objects,
        generalized: result.generalized,
        tiles: result.tiles.len(),
    });
    for zoom in (min_zoom..detail).rev() {
        let level = convert_cut(
            map,
            anchor,
            table,
            first_id,
            &Options {
                zoom,
                detail_zoom: detail,
                ..*options
            },
            extent,
            &adapted,
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
    use crate::map::{coord_flag, fill_pattern_type, Coord, Georeferencing, Object, PathObject};
    use crate::mot::node_flag;
    use crate::osm_convert::{utm, utm_inverse};
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

    /// The ISOM reference map with nothing on it, placed at the anchor: its
    /// projected reference point is the anchor's, in UTM zone 32.
    fn empty_map() -> Map {
        let (mut map, _) = read_xml_map_str(ISOM).unwrap();
        map.objects.clear();
        let (ref_point_x, ref_point_y) = utm(32, (anchor().lon, anchor().lat));
        map.georeferencing = Some(Georeferencing {
            scale: 10000,
            epsg: 32632,
            ref_point_x,
            ref_point_y,
            ..Default::default()
        });
        map
    }

    /// A map whose projected reference point slipped to an easting of 0, as
    /// Corno alle Scale's did: its objects are drawn some 640 km east of it,
    /// with the grivation and grid scale factor of that far point, and Mapper
    /// shows them where they are. So must the tiles: a point the georeference
    /// puts at UTM 32N (645 773, 4 889 977) is stored there, within a metre,
    /// not where the geographic reference point (lon 2.77) laid out on the
    /// ellipsoid would put it, some 30 km away.
    #[test]
    fn a_map_far_from_its_reference_point_is_placed_as_mapper_places_it() {
        let mut map = empty_map();
        map.georeferencing = Some(Georeferencing {
            scale: 15000,
            epsg: 32632,
            ref_point_x: 0.0,
            ref_point_y: 4_884_408.909961,
            grivation: 5.74,
            grivation_specified: true,
            grid_scale_factor: 1.002676,
            ..Default::default()
        });
        let paper = (42684.328, -4662.612);
        let o = object_of(
            &map,
            |s| matches!(s, Symbol::Point(_)),
            ObjectKind::Point,
            vec![Coord::new(paper.0, paper.1, 0)],
        );
        map.objects.push(o);
        // What the file says it is: lat 43.9427, lon 2.7700.
        let far = GeographicAnchor {
            lat: 43.94268593,
            lon: 2.77002108,
            declination: 1.41,
        };
        let converted = convert(&map, &far, &SymbolTable::isom(), 0, &Options::default()).unwrap();
        assert!(converted
            .warnings
            .iter()
            .any(|w| w.contains("easting of 0")));
        let (id, tile) = converted.tiles.iter().next().unwrap();
        let (x, y) = tile.node_mercator(*id, &tile.objects[0].nodes[0]);
        // Mapper's transform, by hand: rotate by -grivation, scale, translate.
        let s = 15.0 * 1.002676;
        let (sin_g, cos_g) = 5.74f64.to_radians().sin_cos();
        let (lx, ly) = (paper.0 * s, paper.1 * s);
        let e = lx * cos_g - ly * sin_g;
        let n = 4_884_408.909961 - (lx * sin_g + ly * cos_g);
        assert!(
            (e - 645_773.0).abs() < 2.0 && (n - 4_889_977.0).abs() < 2.0,
            "{e}, {n}"
        );
        let (lon, lat) = utm_inverse(32, (e, n));
        let (ex, ey) = web_mercator(lat, lon);
        // A Mercator meter is about 1.4 ground meters at 44 degrees.
        assert!(
            (x - ex).abs() < 1.4 && (y - ey).abs() < 1.4,
            "{x},{y} vs {ex},{ey}"
        );
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
    fn texts_are_left_out() {
        let mut map = empty_map();
        let point = object_of(
            &map,
            |s| matches!(s, Symbol::Point(_)),
            ObjectKind::Point,
            vec![Coord::new(0.0, 0.0, 0)],
        );
        let text = object_of(
            &map,
            |s| matches!(s, Symbol::Text(_)),
            ObjectKind::Text(crate::map::TextObject {
                text: "Scala 1:10000".into(),
                ..Default::default()
            }),
            vec![Coord::new(5.0, 5.0, 0)],
        );
        map.objects.extend([point, text]);
        let converted = convert_map(&map);
        assert_eq!(converted.objects, 1, "the point only");
        assert_eq!(converted.hidden, 1);
        assert!(converted
            .tiles
            .values()
            .flat_map(|t| &t.objects)
            .all(|o| !matches!(o.shape, Shape::Text(_))));
        assert!(
            converted
                .warnings
                .iter()
                .any(|w| w == "texts are left out, the tiles keep none: 1 objects"),
            "{:?}",
            converted.warnings
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
    fn a_line_across_tiles_is_cut_into_pieces_with_their_ends_marked() {
        let mut map = empty_map();
        // At 1:10000 a millimetre of paper is 10 m, so 500 mm is 5 km: several
        // zoom 14 tiles (about 1.7 km each at this latitude).
        let line = object_by_code(
            &map,
            "101",
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

        /// A piece as stored: where it is, and whether its first and last nodes
        /// are cuts.
        type Piece = (Vec<(f64, f64)>, bool, bool);
        let mut pieces: Vec<Piece> = Vec::new();
        for (id, tile) in &converted.tiles {
            for o in tile.objects.iter().filter(|o| o.symbol != COVERAGE_SYMBOL) {
                assert_eq!(o.id, 0, "every piece has the object's id");
                let first = o.nodes[0].flags & node_flag::CLIPPED as u8 != 0;
                let last = o.nodes[o.nodes.len() - 1].flags & node_flag::CLIPPED as u8 != 0;
                pieces.push((absolute(tile, *id, &o.nodes), first, last));
            }
        }
        // The true ends are the ones that are not cuts.
        let start = pieces.iter().find(|p| !p.1).expect("a piece has the start");
        let start = start.0[0];
        let end = pieces.iter().find(|p| !p.2).expect("a piece has the end");
        let end = *end.0.last().unwrap();
        let total = (end.0 - start.0).hypot(end.1 - start.1);
        let sum: f64 = pieces.iter().map(|p| path_length(&p.0)).sum();
        assert!((sum - total).abs() < 0.5, "{sum} vs {total}");
        // Only the first piece starts at the start, and only the last ends at
        // the end: the others are cut at both.
        assert_eq!(pieces.iter().filter(|p| !p.1).count(), 1);
        assert_eq!(pieces.iter().filter(|p| !p.2).count(), 1);
    }

    #[test]
    fn a_line_with_dashes_or_symbols_along_it_is_kept_whole_in_every_tile_it_crosses() {
        let table = SymbolTable::isom();
        let of_kind = |whole: bool| {
            (0..table.len() as u16)
                .map(|i| table.get(i).unwrap())
                .find(|e| e.kind == crate::mot_symbols::SymbolKind::Line && e.whole == whole)
                .unwrap()
                .code
                .clone()
        };
        let (dashed, plain) = (of_kind(true), of_kind(false));
        let mut map = empty_map();
        // 5 km at 1:10000, crossing several tiles.
        for code in [&dashed, &plain] {
            let line = object_by_code(
                &map,
                code,
                ObjectKind::Path(PathObject::default()),
                vec![Coord::new(0.0, 0.0, 0), Coord::new(500.0, 40.0, 0)],
            );
            map.objects.push(line);
        }
        let options = Options {
            buffer: 0.0,
            ..Options::default()
        };
        let converted = convert_with(&map, &options);
        let (dashed_id, plain_id) = (table.id_of(&dashed).unwrap(), table.id_of(&plain).unwrap());
        let nodes_of = |symbol: u16| -> Vec<usize> {
            converted
                .tiles
                .values()
                .flat_map(|t| &t.objects)
                .filter(|o| o.symbol == symbol)
                .map(|o| o.nodes.len())
                .collect()
        };
        // The dashed one is whole in each tile it crosses: its two nodes, in
        // all of them. The plain one is cut, so a piece has just its two ends too,
        // but each piece is a different length and none has the line's far end.
        let dashed_pieces = nodes_of(dashed_id);
        assert!(dashed_pieces.len() >= 3 && dashed_pieces.iter().all(|&n| n == 2));
        let whole_ends = |symbol: u16| -> usize {
            converted
                .tiles
                .values()
                .flat_map(|t| &t.objects)
                .filter(|o| o.symbol == symbol)
                .filter(|o| {
                    o.nodes
                        .iter()
                        .all(|n| n.flags & node_flag::CLIPPED as u8 == 0)
                })
                .count()
        };
        assert_eq!(whole_ends(dashed_id), dashed_pieces.len());
        assert!(whole_ends(plain_id) < nodes_of(plain_id).len());
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
            .flat_map(|(_, t)| t.objects.iter())
            .filter(|o| o.symbol != COVERAGE_SYMBOL)
            .map(|o| o.id)
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
                .filter(|&s| s != COVERAGE_SYMBOL)
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
        // Three objects, and then the id of the map's coverage.
        assert_eq!(levels.next_id, 104);
        assert_eq!(levels.coverage_id, Some(103));
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

    fn coverage_at(converted: &Converted, zoom: u8) -> Vec<(TileId, &TileObject)> {
        converted
            .tiles
            .iter()
            .filter(|(id, _)| id.z == zoom)
            .flat_map(|(id, t)| t.objects.iter().map(move |o| (*id, o)))
            .filter(|(_, o)| o.symbol == COVERAGE_SYMBOL)
            .collect()
    }

    #[test]
    fn what_a_map_covers_is_stored_at_every_level_even_where_it_has_no_objects() {
        let map = map_for_levels();
        let levels = convert_levels(
            &map,
            &anchor(),
            &SymbolTable::isom(),
            0,
            8,
            &Options {
                buffer: 0.0,
                ..Options::default()
            },
        )
        .unwrap();
        let id = levels.coverage_id.expect("the map has a coverage");
        for zoom in 8..=14 {
            let found = coverage_at(&levels, zoom);
            assert!(!found.is_empty(), "zoom {zoom}");
            assert!(found.iter().all(|(_, o)| o.id == id));
        }
        // Below the zoom the first symbol appears at, a tile has the coverage and
        // nothing else.
        for zoom in 8..11 {
            let others = levels
                .tiles
                .iter()
                .filter(|(i, _)| i.z == zoom)
                .flat_map(|(_, t)| &t.objects)
                .filter(|o| o.symbol != COVERAGE_SYMBOL)
                .count();
            assert_eq!(others, 0, "zoom {zoom}");
        }
        // The pieces over a level's tiles are the box the objects are in: a
        // rectangle of the map's frame, so its area is its sides' product.
        let [x0, y0, x1, y1] = paper_extent(&map).unwrap();
        let stretch = 1.0 / anchor().lat.to_radians().cos();
        let expected = (x1 - x0) * (y1 - y0) * (10.0 * stretch) * (10.0 * stretch);
        let mut area = 0.0;
        for (tile_id, o) in coverage_at(&levels, 14) {
            let tile = &levels.tiles[&tile_id];
            let p = absolute(tile, tile_id, &o.nodes);
            area += (0..p.len() - 1)
                .map(|i| p[i].0 * p[i + 1].1 - p[i + 1].0 * p[i].1)
                .sum::<f64>()
                / 2.0;
        }
        assert!(
            (area.abs() - expected).abs() / expected < 0.02,
            "{area} vs {expected}"
        );
    }

    #[test]
    fn the_box_a_map_covers_leaves_out_a_stray_object() {
        let mut map = empty_map();
        let mut coords: Vec<Coord> = (0..3000)
            .map(|i| Coord::new(f64::from(i % 50), f64::from(i / 50), 0))
            .collect();
        coords.push(Coord::new(1_000_000.0, 1_000_000.0, 0));
        let cluster = object_by_code(&map, "101", ObjectKind::Path(PathObject::default()), coords);
        map.objects.push(cluster);
        let [x0, y0, x1, y1] = paper_extent(&map).unwrap();
        assert!(x1 - x0 < 100.0 && y1 - y0 < 100.0, "{x0} {y0} {x1} {y1}");
        assert!(paper_extent(&empty_map()).is_none());
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
            let line = object_by_code(
                &map,
                "101",
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

    /// A map whose north is east of true north (declination east) is turned
    /// clockwise on the earth; Mapper's rotations are counterclockwise, so its
    /// symbols and its north lines' pattern are stored turned by minus that
    /// much: they then point to magnetic north, not as far west of true north.
    /// Another pattern not turned in the map is laid on the world's grid
    /// (`mot_lattice`), as one not turned from grid north.
    #[test]
    fn a_map_turned_clockwise_turns_its_symbols_and_patterns_clockwise() {
        let mut map = empty_map();
        // Placed from its anchor: true north is the declination away.
        map.georeferencing.as_mut().unwrap().epsg = 0;
        let declination = 5.0;
        let anchor = GeographicAnchor {
            declination,
            ..anchor()
        };
        let point = object_of(
            &map,
            |s| matches!(s, Symbol::Point(_)),
            ObjectKind::Point,
            vec![Coord::new(0.0, 0.0, 0)],
        );
        let square = vec![
            Coord::new(-5.0, -5.0, 0),
            Coord::new(5.0, -5.0, 0),
            Coord::new(5.0, 5.0, 0),
            Coord::new(-5.0, 5.0, coord_flag::CLOSE_POINT),
        ];
        let area = object_of(
            &map,
            |s| s.code() == "211",
            ObjectKind::Path(PathObject::default()),
            square.clone(),
        );
        let north_lines = object_of(
            &map,
            |s| s.code() == "601.2",
            ObjectKind::Path(PathObject::default()),
            square,
        );
        map.objects.extend([point, area, north_lines]);
        let converted =
            convert(&map, &anchor, &SymbolTable::isom(), 0, &Options::default()).unwrap();
        let tile = converted.tiles.values().next().unwrap();
        let expected = -declination.to_radians();
        let point = tile
            .objects
            .iter()
            .find(|o| o.shape == Shape::Point)
            .unwrap();
        assert!(
            (point.rotation - expected).abs() < 1e-3,
            "{}",
            point.rotation
        );
        let table = SymbolTable::isom();
        let pattern = |code: &str| {
            let id = table.id_of(code).unwrap();
            tile.objects
                .iter()
                .find_map(|o| match &o.shape {
                    Shape::Path(Some(p)) if o.symbol == id => Some(p.rotation),
                    _ => None,
                })
                .unwrap()
        };
        let north = pattern("601.2");
        assert!((north - expected).abs() < 1e-3, "{north}");
        assert_eq!(pattern("211"), 0.0);
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
                if a.symbol != COVERAGE_SYMBOL {
                    seen.insert(a.id);
                }
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

    /// A symbol stored as another is said so, with the number of its own
    /// objects: the notes are kept per symbol of the map, and so is the count,
    /// however many of the map's symbols are stored as themselves before it.
    #[test]
    fn a_symbol_stored_as_another_is_reported_with_its_own_count() {
        let mut map = empty_map();
        let mut push_variant = |code: &str, objects: usize| {
            map.symbols.push(Symbol::Point(crate::map::PointSymbol {
                code: code.into(),
                name: "no such name".into(),
                ..Default::default()
            }));
            let id = 90000 + map.symbols.len() as i32;
            map.symbol_ids.push(id);
            for _ in 0..objects {
                let mut o = Object::new(ObjectKind::Point);
                o.symbol_id = id;
                o.symbol_index = Some(map.symbols.len() - 1);
                o.coords = vec![Coord::new(0.0, 0.0, 0)];
                map.objects.push(o);
            }
        };
        push_variant("417.77", 3);
        push_variant("418.77", 2);
        let converted = convert_map(&map);
        assert_eq!(converted.remapped, 5);
        for expected in [
            "symbol 417.77 \"no such name\" stored as 417 ",
            "symbol 418.77 \"no such name\" stored as 418 ",
        ] {
            let warning = converted
                .warnings
                .iter()
                .find(|w| w.starts_with(expected))
                .unwrap_or_else(|| panic!("{expected} in {:?}", converted.warnings));
            let count = if expected.contains("417") { 3 } else { 2 };
            assert!(
                warning.ends_with(&format!(": {count} objects")),
                "{warning}"
            );
        }
    }

    /// The patterns of the symbol `code` of `map`.
    fn patterns_of<'a>(map: &'a Map, code: &str) -> Vec<&'a FillPattern> {
        let symbol = map.symbols.iter().find(|s| s.code() == code).unwrap();
        let mut patterns = Vec::new();
        fill_patterns(map, symbol, &mut patterns);
        patterns
    }

    #[test]
    fn a_pattern_origin_moves_by_whole_periods_to_beside_the_object() {
        let (map, _) = read_xml_map_str(ISOM).unwrap();
        let near = Point::new(41_234.567, -25_678.901);
        // Dots on a square grid (turned with the object), the two hatchings
        // of an out-of-bounds area, lines all one way (marsh), and two dot
        // patterns offset from each other (vineyard).
        for code in ["211", "709", "308", "414"] {
            let patterns = patterns_of(&map, code);
            let origin = Point::new(3.3, -7.1);
            let moved = nearest_origin(&patterns, 0.3, origin, near);
            let shift = moved - origin;
            let mut largest: f64 = 0.0;
            for p in patterns {
                let r = p.angle + if p.rotatable { 0.3 } else { 0.0 };
                let whole = |along: f64, period: f64| {
                    let n = along / period;
                    assert!((n - n.round()).abs() < 1e-6, "{code}: {n} periods");
                };
                whole(shift.dot(Point::new(r.sin(), r.cos())), p.line_spacing);
                largest = largest.max(p.line_spacing);
                if p.pattern_type == fill_pattern_type::POINT {
                    whole(shift.dot(Point::new(r.cos(), -r.sin())), p.point_distance);
                    largest = largest.max(p.point_distance);
                }
            }
            let left = (moved - near).length();
            assert!(left <= 2.0 * largest, "{code}: {left} mm from the object");
        }
    }

    /// North lines laid out from 400 km away are stored laid out from beside
    /// them, on their map's lattice. The other patterns keep only the turn the
    /// mapper gave them, from grid north: stony ground turns with the object,
    /// cultivated land does not, and goes on the grid unturned.
    #[test]
    fn a_far_pattern_origin_is_stored_beside_its_object() {
        let mut map = empty_map();
        // 40 m of paper from the paper's origin: 400 km at 1:10000, as a map
        // whose projected reference point is at an easting of 0 has.
        let (x, y) = (40_000.0, -3_000.0);
        let square = |x: f64, y: f64| {
            vec![
                Coord::new(x, y, 0),
                Coord::new(x + 20.0, y, 0),
                Coord::new(x + 20.0, y + 20.0, 0),
                Coord::new(x, y + 20.0, 0),
                Coord::new(x, y, coord_flag::CLOSE_POINT),
            ]
        };
        let turned = PathObject {
            pattern_rotation: 1.0,
            pattern_origin: Point::new(0.0, 0.0),
        };
        for code in ["211", "412", "601.2"] {
            let o = object_of(
                &map,
                |s| s.code() == code,
                ObjectKind::Path(PathObject { ..turned }),
                square(x, y),
            );
            map.objects.push(o);
        }
        let converted = convert(
            &map,
            &anchor(),
            &SymbolTable::isom(),
            0,
            &Options::default(),
        )
        .unwrap();
        let table = SymbolTable::isom();
        let north_lines = table.id_of("601.2").unwrap();
        let mut rotations = Vec::new();
        for tile in converted.tiles.values() {
            for o in &tile.objects {
                let Shape::Path(Some(pattern)) = &o.shape else {
                    continue;
                };
                rotations.push((o.symbol, pattern.rotation));
                if o.symbol != north_lines {
                    continue;
                }
                let (cx, cy) = (
                    o.nodes.iter().map(|n| f64::from(n.x)).sum::<f64>() / o.nodes.len() as f64,
                    o.nodes.iter().map(|n| f64::from(n.y)).sum::<f64>() / o.nodes.len() as f64,
                );
                let (dx, dy) = (
                    f64::from(pattern.origin.0) - cx,
                    f64::from(pattern.origin.1) - cy,
                );
                // 5 cm units: within one spacing of the lines (300 m) and the
                // 200 m square of its nodes' middle.
                assert!(
                    dx.hypot(dy) < 8_000.0,
                    "the origin is {dx}, {dy} units away"
                );
            }
        }
        let rotation = |code: &str| {
            let id = table.id_of(code).unwrap();
            rotations.iter().find(|r| r.0 == id).unwrap().1
        };
        // The map's paper is zone 32's grid (no grivation), but 400 km east the
        // square is in zone 33, whose grid north is some 4° from it: the
        // mapper's turn on the ground, from that zone's grid north.
        let off = rotation("211") - 1.0;
        assert!((off.to_degrees() + 4.2).abs() < 0.5, "{}", off.to_degrees());
        assert_eq!(rotation("412"), 0.0);
    }

    #[test]
    fn a_placement_puts_the_paper_where_the_tiles_have_it() {
        // In a UTM zone, by the projection; elsewhere, from the anchor.
        let utm_map = empty_map();
        let mut local = empty_map();
        local.georeferencing.as_mut().unwrap().epsg = 0;
        for map in [utm_map, local] {
            let placement = Placement::new(&map, &anchor()).unwrap();
            let g = map.georeferencing.as_ref().unwrap();
            let (lat, lon) = placement.geographic(g.map_ref_point_x, g.map_ref_point_y);
            assert!((lat - anchor().lat).abs() < 1e-7, "{lat}");
            assert!((lon - anchor().lon).abs() < 1e-7, "{lon}");
            // A millimetre east on the paper is the scale's meters east.
            let a = placement.mercator(g.map_ref_point_x, g.map_ref_point_y);
            let b = placement.mercator(g.map_ref_point_x + 1.0, g.map_ref_point_y);
            let ground = (b.0 - a.0).hypot(b.1 - a.1) * anchor().lat.to_radians().cos();
            assert!((ground - 10.0).abs() < 0.05, "{ground} m");
            // Within the grid convergence (1.6° here, in zone 32).
            assert!((b.1 - a.1).abs() < (b.0 - a.0) * 0.05, "east is east");
        }
        assert!(Placement::new(&Map::default(), &anchor()).is_err());
    }

    #[test]
    fn a_mapper_file_says_where_it_is_and_one_without_a_place_is_none() {
        let (map, anchor) = read_placed_map(ISOM.as_bytes()).unwrap();
        assert!(!map.symbols.is_empty());
        assert_eq!(anchor, geographic_anchor(ISOM));
        assert!(read_placed_map(b"\xff\xfe not a map").is_err());
    }
}
