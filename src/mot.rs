//! MOT, the tiled world-map format: tile addressing and the tile payload.
//!
//! See `mds/MOT-Format.md` for the design. This module is the pure part:
//! which tile a place is in, and how the objects of one tile are turned into
//! bytes and back. It knows nothing of maps ([`crate::mot_convert`] makes tiles
//! from a [`Map`](crate::map::Map)), symbol tables
//! ([`crate::mot_symbols`]) or containers.
//!
//! # Coordinates
//!
//! Tiles are the Web Mercator quadtree of slippy maps: `z/x/y`, with `y = 0` at
//! the top. Inside a tile a node is a pair of integers on a grid of
//! [`Tile::grid_cm`] centimetres of Mercator, counted from the tile's north-west
//! corner, with `y` growing *down* as on paper.
//!
//! # Payload
//!
//! ```text
//! version        u8
//! grid_cm        varint
//! object count   varint
//! run count      varint
//! runs           (symbol varint, count varint) * run count
//! streams        meta, counts, flags, dx, dy, extra: each a varint length,
//!                then that many bytes
//! ```
//!
//! Objects are sorted by `(symbol, id)`; the runs say how many objects each
//! symbol has, so the symbol is stored once per run and a renderer can walk the
//! tile in layer order without sorting. Splitting the node data into separate
//! streams (one byte of flags per node, `dx`, `dy`) is what makes it compress
//! well afterwards; the container compresses each tile as a whole.
//!
//! An object is stored whole in a tile it lies inside, or, where it crosses the
//! tile's box (grown by a buffer), as the pieces of it inside: see
//! [`crate::mot_clip`]. A node flagged [`node_flag::CLIPPED`] is a cut: for a
//! piece of a line, the end of the piece is not the end of the line; for a
//! ring, the edge leaving the node is not in the map.
//!
//! A node is stored the way Mapper stores a coordinate: a position and a set of
//! flags, a curve being a flag on the vertex it starts at, followed by its two
//! control points and its end. That is the same information as a command
//! stream with a cubic command, and nothing is lost converting a map.
//!
//! This is the draft, version 0, format: it will change before it is frozen.

use std::f64::consts::PI;

/// The version byte of the payload format written by this module.
pub const VERSION: u8 = 0;

/// Half the width of the Web Mercator world, in meters: `pi * R`.
pub const WORLD_HALF: f64 = 20_037_508.342_789_244;

/// The most zoomed a tile can be: the quadtree's coordinates must fit a `u32`
/// and the arithmetic below a `u64`.
pub const MAX_ZOOM: u8 = 30;

/// The symbol of an area that is not on a map but says where a map is: the
/// footprint of what a map covers. Not an id of any symbol table (those count up
/// from 0), so it sorts after every symbol of a tile, and a renderer that does
/// not know it draws nothing.
pub const COVERAGE_SYMBOL: u16 = u16::MAX;

/// Node flags: the same bits as [`crate::map::coord_flag`], which already fit a
/// byte, so a map's flags are stored as they are.
pub mod node_flag {
    pub use crate::map::coord_flag::{CLOSE_POINT, CURVE_START, DASH_POINT, GAP_POINT, HOLE_POINT};
    /// The node is at a cut made at a tile's border (see
    /// [`crate::mot_clip`]). On the first or last node of a piece of a line it
    /// means that end is a cut, not the end of the line. On a node of a ring it
    /// means the edge leaving the node is a cut, not an edge of the map: a
    /// renderer must not draw an area's outline along it.
    pub const CLIPPED: i32 = 1 << 6;
}

/// One tile of the Web Mercator quadtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TileId {
    /// The zoom level: 0 is the whole world in one tile.
    pub z: u8,
    /// The column, counted from the west.
    pub x: u32,
    /// The row, counted from the north.
    pub y: u32,
}

impl TileId {
    /// The side of a tile at zoom `z`, in Mercator meters.
    pub fn size(z: u8) -> f64 {
        2.0 * WORLD_HALF / (1u64 << z) as f64
    }

    /// The tile at zoom `z` holding the Mercator position `(x, y)` in meters
    /// (`y` growing north), clamped to the world.
    pub fn containing(z: u8, x: f64, y: f64) -> TileId {
        let n = 1u64 << z;
        let size = TileId::size(z);
        let column = ((x + WORLD_HALF) / size).floor();
        let row = ((WORLD_HALF - y) / size).floor();
        let clamp = |v: f64| v.clamp(0.0, (n - 1) as f64) as u32;
        TileId {
            z,
            x: clamp(column),
            y: clamp(row),
        }
    }

    /// The north-west corner of the tile, in Mercator meters.
    pub fn origin(self) -> (f64, f64) {
        let size = TileId::size(self.z);
        (
            f64::from(self.x) * size - WORLD_HALF,
            WORLD_HALF - f64::from(self.y) * size,
        )
    }

    /// Every tile at zoom `z` the box `[west, south, east, north]`, in Mercator
    /// meters, touches.
    pub fn covering(z: u8, bounds: [f64; 4]) -> Vec<TileId> {
        let first = TileId::containing(z, bounds[0], bounds[3]);
        let last = TileId::containing(z, bounds[2], bounds[1]);
        let mut tiles = Vec::new();
        for y in first.y..=last.y {
            for x in first.x..=last.x {
                tiles.push(TileId { z, x, y });
            }
        }
        tiles
    }
}

/// A Mercator position in meters as `(latitude, longitude)` in degrees: the
/// inverse of [`crate::orthophoto::web_mercator`].
pub fn lat_lon(x: f64, y: f64) -> (f64, f64) {
    let radius = WORLD_HALF / PI;
    (
        (y / radius).sinh().atan().to_degrees(),
        (x / radius).to_degrees(),
    )
}

/// Meters on the ground per Mercator meter at `latitude` (degrees): east-west,
/// then north-south. Web Mercator projects the ellipsoid's latitudes as if
/// they were a sphere's, so it is not conformal: the two differ, by some 0.3%,
/// and neither is quite `cos(latitude)`. A symbol drawn with one scale for
/// both is that much off, which no one sees; a fill pattern laid out from a
/// point a few hundred metres away is off by a fraction of its period.
pub fn ground_per_mercator(latitude: f64) -> (f64, f64) {
    // WGS 84's first eccentricity, squared; Web Mercator's sphere has the
    // ellipsoid's equatorial radius.
    const E2: f64 = 0.006_694_379_990_14;
    let (sin, cos) = latitude.to_radians().sin_cos();
    let w = 1.0 - E2 * sin * sin;
    (cos / w.sqrt(), cos * (1.0 - E2) / (w * w.sqrt()))
}

/// The direction from `a` to `b` (Mercator meters, `y` north) on the ground at
/// `latitude`: counterclockwise from east, in radians. Not quite the direction
/// in Mercator, which stretches north-south a little more than east-west
/// ([`ground_per_mercator`]).
pub fn ground_angle(a: (f64, f64), b: (f64, f64), latitude: f64) -> f64 {
    let (gx, gy) = ground_per_mercator(latitude);
    ((b.1 - a.1) * gy).atan2((b.0 - a.0) * gx)
}

/// The resolution of a tile's coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    /// The side of one grid cell, in centimetres of Mercator.
    pub cm: u32,
}

impl Grid {
    /// A grid of `cm` centimetres, at least one.
    pub fn new(cm: u32) -> Grid {
        Grid { cm: cm.max(1) }
    }

    /// A length in meters as grid units, rounded.
    pub fn units(self, meters: f64) -> i32 {
        (meters * 100.0 / f64::from(self.cm)).round() as i32
    }

    /// Grid units as a length in meters.
    pub fn meters(self, units: i32) -> f64 {
        f64::from(units) * f64::from(self.cm) / 100.0
    }
}

/// A position in a tile and what the path does there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    /// Grid units from the tile's west edge.
    pub x: i32,
    /// Grid units from the tile's north edge, growing south.
    pub y: i32,
    /// A set of [`node_flag`] bits.
    pub flags: u8,
}

/// What a path object carries beyond its nodes, where its symbol has a fill
/// pattern.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pattern {
    /// The rotation of the fill pattern, in radians.
    pub rotation: f64,
    /// The point the pattern is laid out from, in the tile's grid units.
    pub origin: (i32, i32),
}

/// What a text object carries beyond its nodes.
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    /// The text, possibly of several lines.
    pub text: String,
    /// Horizontal alignment, 0 to 3 as in [`crate::map::h_align`].
    pub h_align: u8,
    /// Vertical alignment, 0 to 3 as in [`crate::map::v_align`].
    pub v_align: u8,
    /// The size of the text box in grid units, where the text has one.
    pub box_size: Option<(u32, u32)>,
}

/// What kind of object this is.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A single position (one node).
    Point,
    /// A line or an area, with its pattern where there is one.
    Path(Option<Pattern>),
    /// A piece of text, at one or two nodes.
    Text(Text),
}

/// An object in a tile.
#[derive(Clone, Debug, PartialEq)]
pub struct TileObject {
    /// The object's stable id, the same in every tile it appears in.
    pub id: u64,
    /// The symbol, as an id in the layer's symbol table.
    pub symbol: u16,
    /// The rotation in radians, in the tile's frame (a quantised 1/65536 of a
    /// turn once stored).
    pub rotation: f64,
    /// What kind of object it is, and what that kind carries.
    pub shape: Shape,
    /// Its nodes: a point has one.
    pub nodes: Vec<Node>,
}

/// The objects of one tile.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    /// The resolution of the nodes.
    pub grid_cm: u32,
    /// The objects. Stored sorted by `(symbol, id)`.
    pub objects: Vec<TileObject>,
}

// ---- byte coding ----

pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(crate) fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// A rotation in radians as a 16-bit fraction of a turn.
fn quantise_angle(radians: f64) -> u16 {
    let turns = radians / (2.0 * PI);
    ((turns - turns.floor()) * 65536.0).round() as u32 as u16
}

fn dequantise_angle(q: u16) -> f64 {
    f64::from(q) * 2.0 * PI / 65536.0
}

/// A cursor over bytes which never panics: every read says what ran out.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, at: 0 }
    }

    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.bytes.get(self.at).ok_or("the bytes end too soon")?;
        self.at += 1;
        Ok(b)
    }

    pub(crate) fn varint(&mut self) -> Result<u64, String> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let b = self.byte()?;
            if shift == 63 && b > 1 {
                return Err("varint too long".into());
            }
            value |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("varint too long".into())
    }

    pub(crate) fn signed(&mut self) -> Result<i64, String> {
        Ok(unzigzag(self.varint()?))
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or("length overflows")?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or("the bytes end too soon")?;
        self.at = end;
        Ok(slice)
    }

    fn stream(&mut self) -> Result<Reader<'a>, String> {
        let n = usize::try_from(self.varint()?).map_err(|_| "stream too long")?;
        Ok(Reader::new(self.take(n)?))
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn to_i32(v: i64) -> Result<i32, String> {
        i32::try_from(v).map_err(|_| "coordinate out of range".to_string())
    }
}

const KIND_POINT: u8 = 0;
const KIND_PATH: u8 = 1;
const KIND_TEXT: u8 = 2;
const HAS_ROTATION: u8 = 1 << 2;
const HAS_PATTERN: u8 = 1 << 3;

impl Tile {
    /// An empty tile on a grid of `grid_cm` centimetres.
    pub fn new(grid_cm: u32) -> Tile {
        Tile {
            grid_cm: grid_cm.max(1),
            objects: Vec::new(),
        }
    }

    /// The payload bytes, uncompressed. Objects are written sorted by
    /// `(symbol, id)`.
    pub fn encode(&self) -> Vec<u8> {
        let mut order: Vec<&TileObject> = self.objects.iter().collect();
        order.sort_by_key(|o| (o.symbol, o.id));

        let mut runs: Vec<(u16, u64)> = Vec::new();
        for o in &order {
            match runs.last_mut() {
                Some((symbol, count)) if *symbol == o.symbol => *count += 1,
                _ => runs.push((o.symbol, 1)),
            }
        }

        let (mut meta, mut counts, mut flags) = (Vec::new(), Vec::new(), Vec::new());
        let (mut dx, mut dy, mut extra) = (Vec::new(), Vec::new(), Vec::new());
        let mut previous_id = 0u64;
        let mut previous_first = (0i32, 0i32);
        for o in &order {
            let pattern = match &o.shape {
                Shape::Path(Some(p)) => Some(p),
                _ => None,
            };
            let kind = match o.shape {
                Shape::Point => KIND_POINT,
                Shape::Path(_) => KIND_PATH,
                Shape::Text(_) => KIND_TEXT,
            };
            let rotation = quantise_angle(o.rotation);
            let mut byte = kind;
            if rotation != 0 {
                byte |= HAS_ROTATION;
            }
            if pattern.is_some() {
                byte |= HAS_PATTERN;
            }
            meta.push(byte);
            put_varint(&mut meta, zigzag(o.id as i64 - previous_id as i64));
            previous_id = o.id;

            if kind != KIND_POINT {
                put_varint(&mut counts, o.nodes.len() as u64);
            }
            let (mut px, mut py) = previous_first;
            for (i, n) in o.nodes.iter().enumerate() {
                flags.push(n.flags);
                put_varint(&mut dx, zigzag(i64::from(n.x) - i64::from(px)));
                put_varint(&mut dy, zigzag(i64::from(n.y) - i64::from(py)));
                px = n.x;
                py = n.y;
                if i == 0 {
                    previous_first = (n.x, n.y);
                }
            }

            if rotation != 0 {
                extra.extend_from_slice(&rotation.to_le_bytes());
            }
            let first = o.nodes.first().map_or((0, 0), |n| (n.x, n.y));
            if let Some(p) = pattern {
                extra.extend_from_slice(&quantise_angle(p.rotation).to_le_bytes());
                put_varint(
                    &mut extra,
                    zigzag(i64::from(p.origin.0) - i64::from(first.0)),
                );
                put_varint(
                    &mut extra,
                    zigzag(i64::from(p.origin.1) - i64::from(first.1)),
                );
            }
            if let Shape::Text(t) = &o.shape {
                put_varint(&mut extra, t.text.len() as u64);
                extra.extend_from_slice(t.text.as_bytes());
                let mut align = (t.h_align & 3) | ((t.v_align & 3) << 2);
                if t.box_size.is_some() {
                    align |= 1 << 4;
                }
                extra.push(align);
                if let Some((w, h)) = t.box_size {
                    put_varint(&mut extra, u64::from(w));
                    put_varint(&mut extra, u64::from(h));
                }
            }
        }

        let mut out = vec![VERSION];
        put_varint(&mut out, u64::from(self.grid_cm));
        put_varint(&mut out, order.len() as u64);
        put_varint(&mut out, runs.len() as u64);
        for (symbol, count) in runs {
            put_varint(&mut out, u64::from(symbol));
            put_varint(&mut out, count);
        }
        for stream in [&meta, &counts, &flags, &dx, &dy, &extra] {
            put_varint(&mut out, stream.len() as u64);
            out.extend_from_slice(stream);
        }
        out
    }

    /// Reads a payload written by [`Tile::encode`]. An error, never a panic,
    /// for bytes that are not one.
    pub fn decode(bytes: &[u8]) -> Result<Tile, String> {
        let mut r = Reader::new(bytes);
        let version = r.byte()?;
        if version != VERSION {
            return Err(format!("unsupported tile version {version}"));
        }
        let grid_cm = u32::try_from(r.varint()?).map_err(|_| "grid out of range")?;
        let object_count = usize::try_from(r.varint()?).map_err(|_| "too many objects")?;
        let run_count = usize::try_from(r.varint()?).map_err(|_| "too many runs")?;
        // An object takes at least a byte of the meta stream, so a count
        // beyond what is left is a lie that must not size an allocation.
        if object_count > bytes.len() || run_count > bytes.len() {
            return Err("tile counts exceed its size".into());
        }
        let mut symbols: Vec<u16> = Vec::with_capacity(object_count);
        for _ in 0..run_count {
            let symbol = u16::try_from(r.varint()?).map_err(|_| "symbol out of range")?;
            let count = usize::try_from(r.varint()?).map_err(|_| "run too long")?;
            if symbols.len() + count > object_count {
                return Err("runs hold more objects than the tile".into());
            }
            symbols.extend(std::iter::repeat_n(symbol, count));
        }
        if symbols.len() != object_count {
            return Err("runs hold fewer objects than the tile".into());
        }
        let mut meta = r.stream()?;
        let mut counts = r.stream()?;
        let mut flags = r.stream()?;
        let mut dx = r.stream()?;
        let mut dy = r.stream()?;
        let mut extra = r.stream()?;

        let mut objects = Vec::with_capacity(object_count);
        let mut previous_id = 0i64;
        let mut previous_first = (0i32, 0i32);
        for &symbol in &symbols {
            let byte = meta.byte()?;
            let kind = byte & 3;
            previous_id = previous_id
                .checked_add(meta.signed()?)
                .ok_or("object id out of range")?;
            let id = u64::try_from(previous_id).map_err(|_| "object id out of range")?;

            let n = match kind {
                KIND_POINT => 1,
                KIND_PATH | KIND_TEXT => {
                    usize::try_from(counts.varint()?).map_err(|_| "too many nodes")?
                }
                _ => return Err("unknown object kind".into()),
            };
            if n > flags.bytes.len() - flags.at {
                return Err("node count exceeds the node data".into());
            }
            let mut nodes = Vec::with_capacity(n);
            let (mut px, mut py) = previous_first;
            for i in 0..n {
                let flag = flags.byte()?;
                px = Reader::to_i32(i64::from(px) + dx.signed()?)?;
                py = Reader::to_i32(i64::from(py) + dy.signed()?)?;
                nodes.push(Node {
                    x: px,
                    y: py,
                    flags: flag,
                });
                if i == 0 {
                    previous_first = (px, py);
                }
            }

            let rotation = if byte & HAS_ROTATION != 0 {
                dequantise_angle(extra.u16()?)
            } else {
                0.0
            };
            let first = nodes.first().map_or((0, 0), |n| (n.x, n.y));
            let shape = match kind {
                KIND_POINT => Shape::Point,
                KIND_PATH => {
                    if byte & HAS_PATTERN != 0 {
                        let rotation = dequantise_angle(extra.u16()?);
                        let ox = Reader::to_i32(i64::from(first.0) + extra.signed()?)?;
                        let oy = Reader::to_i32(i64::from(first.1) + extra.signed()?)?;
                        Shape::Path(Some(Pattern {
                            rotation,
                            origin: (ox, oy),
                        }))
                    } else {
                        Shape::Path(None)
                    }
                }
                _ => {
                    let len = usize::try_from(extra.varint()?).map_err(|_| "text too long")?;
                    let text = String::from_utf8(extra.take(len)?.to_vec())
                        .map_err(|_| "text is not UTF-8")?;
                    let align = extra.byte()?;
                    let box_size = if align & (1 << 4) != 0 {
                        let w = u32::try_from(extra.varint()?).map_err(|_| "box out of range")?;
                        let h = u32::try_from(extra.varint()?).map_err(|_| "box out of range")?;
                        Some((w, h))
                    } else {
                        None
                    };
                    Shape::Text(Text {
                        text,
                        h_align: align & 3,
                        v_align: (align >> 2) & 3,
                        box_size,
                    })
                }
            };
            objects.push(TileObject {
                id,
                symbol,
                rotation,
                shape,
                nodes,
            });
        }
        Ok(Tile { grid_cm, objects })
    }

    /// A node as a Mercator position in meters (`y` growing north), given the
    /// tile it is in.
    pub fn node_mercator(&self, tile: TileId, node: &Node) -> (f64, f64) {
        let grid = Grid::new(self.grid_cm);
        let (west, north) = tile.origin();
        (west + grid.meters(node.x), north - grid.meters(node.y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tile {
        let node = |x, y, flags| Node { x, y, flags };
        let mut tile = Tile::new(5);
        tile.objects.push(TileObject {
            id: 40,
            symbol: 7,
            rotation: 0.0,
            shape: Shape::Point,
            nodes: vec![node(100, -50, 0)],
        });
        tile.objects.push(TileObject {
            id: 12,
            symbol: 3,
            rotation: 0.0,
            shape: Shape::Path(None),
            nodes: vec![
                node(0, 0, node_flag::CURVE_START as u8),
                node(10, 20, 0),
                node(30, 20, 0),
                node(40, 0, node_flag::DASH_POINT as u8),
                node(500, 500, node_flag::HOLE_POINT as u8),
                node(600, 500, 0),
            ],
        });
        tile.objects.push(TileObject {
            id: 13,
            symbol: 3,
            rotation: 1.0,
            shape: Shape::Path(Some(Pattern {
                rotation: 0.5,
                origin: (-7, 9),
            })),
            nodes: vec![node(-3, 4, 0), node(5, 5, node_flag::CLOSE_POINT as u8)],
        });
        tile.objects.push(TileObject {
            id: 99,
            symbol: 300,
            rotation: 0.0,
            shape: Shape::Text(Text {
                text: "Forêt\nnord".into(),
                h_align: 1,
                v_align: 2,
                box_size: Some((400, 90)),
            }),
            nodes: vec![node(1_000_000, 2_000_000, 0), node(1_000_400, 2_000_090, 0)],
        });
        tile
    }

    #[test]
    fn a_tile_survives_the_round_trip() {
        let tile = sample();
        let back = Tile::decode(&tile.encode()).unwrap();
        let mut expected = tile.clone();
        expected.objects.sort_by_key(|o| (o.symbol, o.id));
        assert_eq!(back.grid_cm, 5);
        assert_eq!(back.objects.len(), expected.objects.len());
        for (a, b) in back.objects.iter().zip(&expected.objects) {
            assert_eq!((a.id, a.symbol, &a.nodes), (b.id, b.symbol, &b.nodes));
            assert!(
                (a.rotation - b.rotation).abs() < 1e-3,
                "{} vs {}",
                a.rotation,
                b.rotation
            );
            match (&a.shape, &b.shape) {
                (Shape::Path(Some(p)), Shape::Path(Some(q))) => {
                    // A quantised rotation is close, not equal.
                    assert!((p.rotation - q.rotation).abs() < 1e-3);
                    assert_eq!(p.origin, q.origin);
                }
                (x, y) => assert_eq!(x, y),
            }
        }
    }

    #[test]
    fn rotations_are_kept_within_a_hundredth_of_a_degree() {
        for r in [0.0, 0.1, 1.0, 3.0, 6.0, -1.0] {
            let back = dequantise_angle(quantise_angle(r));
            let diff = (back - r.rem_euclid(2.0 * PI)).abs();
            assert!(diff < 1e-3 || (2.0 * PI - diff) < 1e-3, "{r}: {back}");
        }
    }

    #[test]
    fn varints_round_trip_at_their_edges() {
        for v in [
            0u64,
            1,
            127,
            128,
            16383,
            16384,
            u64::from(u32::MAX),
            u64::MAX,
        ] {
            let mut out = Vec::new();
            put_varint(&mut out, v);
            assert_eq!(Reader::new(&out).varint().unwrap(), v);
        }
        for v in [
            0i64,
            -1,
            1,
            i64::from(i32::MIN),
            i64::from(i32::MAX),
            i64::MIN,
            i64::MAX,
        ] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let good = sample().encode();
        assert!(Tile::decode(&[]).is_err());
        assert!(Tile::decode(&[9]).is_err());
        for cut in 0..good.len() {
            let _ = Tile::decode(&good[..cut]);
        }
        let mut bad = good.clone();
        for i in 0..bad.len() {
            bad[i] ^= 0xa5;
            let _ = Tile::decode(&bad);
            bad[i] ^= 0xa5;
        }
    }

    #[test]
    fn world_tile_is_the_whole_world() {
        assert_eq!(TileId::containing(0, 1.0, 1.0), TileId { z: 0, x: 0, y: 0 });
        let t = TileId { z: 0, x: 0, y: 0 };
        assert_eq!(t.origin(), (-WORLD_HALF, WORLD_HALF));
        assert!((TileId::size(1) - WORLD_HALF).abs() < 1e-6);
    }

    #[test]
    fn a_position_is_inside_its_tile() {
        let (x, y) = (1_285_000.0, 5_465_000.0); // near Bologna
        let id = TileId::containing(14, x, y);
        let (west, north) = id.origin();
        let size = TileId::size(14);
        assert!(west <= x && x < west + size);
        assert!(north >= y && y > north - size);
    }

    #[test]
    fn covering_lists_every_tile_a_box_touches() {
        let size = TileId::size(10);
        let (west, north) = TileId {
            z: 10,
            x: 500,
            y: 300,
        }
        .origin();
        let tiles = TileId::covering(
            10,
            [
                west + 1.0,
                north - 2.0 * size - 1.0,
                west + size + 1.0,
                north - 1.0,
            ],
        );
        assert_eq!(tiles.len(), 2 * 3);
        assert!(tiles.contains(&TileId {
            z: 10,
            x: 501,
            y: 302
        }));
    }

    #[test]
    fn lat_lon_undoes_web_mercator() {
        let (x, y) = crate::orthophoto::web_mercator(44.4938, 11.343);
        let (lat, lon) = lat_lon(x, y);
        assert!((lat - 44.4938).abs() < 1e-9 && (lon - 11.343).abs() < 1e-9);
    }

    #[test]
    fn grid_units_round_trip_to_the_grid() {
        let g = Grid::new(5);
        assert_eq!(g.units(1.0), 20);
        assert!((g.meters(20) - 1.0).abs() < 1e-12);
    }
}
