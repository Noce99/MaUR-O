//! Drawing MOT tiles with the crate's own renderer.
//!
//! A tile holds objects and the ids of their symbols; what a symbol looks like is
//! in the layer's symbol set, which this module takes from the reference map of the
//! layer's standard that the crate ships. A tile is turned back into a [`Map`] — its objects, in
//! millimetres on a paper at the scale asked for — and the renderer that draws
//! `.omap` files draws it, so a tile looks as Mapper draws the map.
//!
//! The scale is the viewer's choice (`MOT-Format.md`): a tile has ground
//! coordinates, and the same tile at 1:4000 or 1:15000 is the same objects with
//! symbols a different size on the ground. A Mercator meter is longer than a
//! ground meter by `1 / cos(latitude)`, taken at the middle of the tile drawn.
//!
//! Any tile of the pyramid can be drawn from the tile of a coarser or finer level
//! that holds its ground: [`TileRenderer::render`] takes the tile with the data
//! and the tile to draw, which may be a part of it (a view finer than the finest
//! level is that level, overzoomed).
//!
//! A line whose symbol lays dashes or symbols out along the whole of it is in
//! the tile whole, not cut ([`crate::mot_symbols::SymbolEntry::whole`]), because
//! how Mapper spaces them depends on the length of the line: drawn from a piece
//! they would not meet the neighbouring tile's.

use tiny_skia::{Pixmap, Transform};

use crate::geometry::Rect;
use crate::map::{Coord, Map, Object, ObjectKind, PathObject, Point, TextObject};
use crate::mot::{lat_lon, node_flag, Shape, Tile, TileId, TileObject};
use crate::mot_symbols::SymbolTable;
use crate::renderer::Renderer;
use crate::xml_reader::read_xml_map_str;

/// How far past the window, in millimetres of paper, an object may reach and
/// still be drawn into it: the widest symbols are a few millimetres.
const REACH_MM: f64 = 10.0;

/// Draws tiles. It keeps the symbol set and one map to put each tile's objects
/// in, so it is made once and used for as many tiles as there are; it is not
/// shared between threads, each of which makes its own.
pub struct TileRenderer {
    template: Map,
    /// For each id of the symbol table, its symbol's index in the template.
    symbol_index: Vec<Option<usize>>,
}

impl TileRenderer {
    /// A renderer for the symbols of `table`, drawn as its reference map draws them.
    pub fn new(table: &SymbolTable) -> TileRenderer {
        let (mut template, _) =
            read_xml_map_str(table.reference()).expect("the shipped symbol sets are well-formed");
        template.objects.clear();
        let symbol_index = (0..table.len() as u16)
            .map(|id| {
                let code = &table.get(id)?.code;
                template
                    .symbols
                    .iter()
                    .position(|s| s.code() == code && !s.is_helper_symbol())
            })
            .collect();
        TileRenderer {
            template,
            symbol_index,
        }
    }

    /// `view`, drawn at 1:`scale` into a square of `pixels` pixels, from the
    /// objects of `tile`, which is the tile `data`. `view` is `data` or a part of
    /// it, a tile of a finer zoom inside it. Transparent where nothing is drawn.
    ///
    /// An error where `view` is not inside `data` or the image cannot be made.
    pub fn render(
        &mut self,
        data: TileId,
        tile: &Tile,
        view: TileId,
        scale: f64,
        pixels: u32,
    ) -> Result<Pixmap, String> {
        if view.z < data.z
            || view.z > 30
            || (view.x >> (view.z - data.z)) != data.x
            || (view.y >> (view.z - data.z)) != data.y
        {
            return Err(format!("tile {view:?} is not in {data:?}"));
        }
        if scale.is_nan() || scale < 1.0 || pixels == 0 {
            return Err("the scale and the size must be positive".into());
        }
        let (view_west, view_north) = view.origin();
        let (data_west, data_north) = data.origin();
        let view_size = TileId::size(view.z);
        let (lat, _) = lat_lon(view_west + view_size / 2.0, view_north - view_size / 2.0);
        // Millimetres of paper per Mercator meter, and per unit of this tile.
        let per_meter = lat.to_radians().cos() * 1000.0 / scale;
        let per_unit = f64::from(tile.grid_cm) / 100.0 * per_meter;
        // The window to draw, in paper millimetres from the data tile's corner.
        let window = Rect::new(
            (view_west - data_west) * per_meter,
            (data_north - view_north) * per_meter,
            view_size * per_meter,
            view_size * per_meter,
        );
        let near = window.adjusted(-REACH_MM, -REACH_MM, REACH_MM, REACH_MM);

        self.template.scale_denominator = scale.round().clamp(1.0, f64::from(i32::MAX)) as i32;
        let mut objects = Vec::new();
        for o in &tile.objects {
            if let Some(object) = self.object(o, per_unit) {
                let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                for c in &object.coords {
                    (x0, x1) = (x0.min(c.x), x1.max(c.x));
                    (y0, y1) = (y0.min(c.y), y1.max(c.y));
                }
                if x1 >= near.left()
                    && x0 <= near.right()
                    && y1 >= near.top()
                    && y0 <= near.bottom()
                {
                    objects.push(object);
                }
            }
        }
        self.template.objects = objects;

        let mut pixmap = Pixmap::new(pixels, pixels).ok_or("cannot make an image that size")?;
        let ppm = f64::from(pixels) / window.width();
        let transform = Transform::from_translate(-window.left() as f32, -window.top() as f32)
            .post_concat(Transform::from_scale(ppm as f32, ppm as f32));
        Renderer::new(&self.template).paint(&mut pixmap.as_mut(), transform);
        self.template.objects.clear();
        Ok(pixmap)
    }

    /// The map's object for a tile's, in paper millimetres; `None` where its
    /// symbol is not in the template.
    fn object(&self, o: &TileObject, per_unit: f64) -> Option<Object> {
        let index = (*self.symbol_index.get(usize::from(o.symbol))?)?;
        let point = |x: i32, y: i32| (f64::from(x) * per_unit, f64::from(y) * per_unit);
        let kind = match &o.shape {
            Shape::Point => ObjectKind::Point,
            Shape::Path(pattern) => ObjectKind::Path(match pattern {
                Some(p) => {
                    let (x, y) = point(p.origin.0, p.origin.1);
                    PathObject {
                        pattern_rotation: p.rotation,
                        pattern_origin: Point::new(x, y),
                    }
                }
                None => PathObject::default(),
            }),
            Shape::Text(t) => ObjectKind::Text(TextObject {
                text: t.text.clone(),
                h_align: i32::from(t.h_align),
                v_align: i32::from(t.v_align),
                box_size: t
                    .box_size
                    .map(|(w, h)| (f64::from(w) * per_unit, f64::from(h) * per_unit)),
            }),
        };
        let mut object = Object::new(kind);
        object.symbol_id = self.template.symbol_ids[index];
        object.symbol_index = Some(index);
        object.rotation = o.rotation;
        let keep = !(node_flag::CLIPPED as u8);
        object.coords = o
            .nodes
            .iter()
            .map(|n| {
                let (x, y) = point(n.x, n.y);
                Coord::new(x, y, i32::from(n.flags & keep))
            })
            .collect();
        Some(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mot::{Node, Tile};

    const SCALE: f64 = 10_000.0;

    fn node(x: i32, y: i32, flags: u8) -> Node {
        Node { x, y, flags }
    }

    /// A tile with a 40 m square building at 0.3 of the way across it each way.
    fn tile_with_a_building(table: &SymbolTable, id: TileId) -> Tile {
        let side = (TileId::size(id.z) * 100.0 / 5.0) as i32;
        let (x, y, size) = (side * 3 / 10, side * 3 / 10, 800);
        let mut tile = Tile::new(5);
        tile.objects.push(TileObject {
            id: 0,
            symbol: table.id_of("521").unwrap(),
            rotation: 0.0,
            shape: Shape::Path(None),
            nodes: vec![
                node(x, y, 0),
                node(x + size, y, 0),
                node(x + size, y + size, 0),
                node(x, y + size, 0),
                node(x, y, node_flag::CLOSE_POINT as u8),
            ],
        });
        tile
    }

    fn pixel(pixmap: &Pixmap, fx: f64, fy: f64) -> [u8; 4] {
        let x = (fx * f64::from(pixmap.width())) as u32;
        let y = (fy * f64::from(pixmap.height())) as u32;
        let p = pixmap.pixel(x, y).unwrap();
        [p.red(), p.green(), p.blue(), p.alpha()]
    }

    #[test]
    fn a_building_is_drawn_black_where_it_is_and_nothing_elsewhere() {
        let table = SymbolTable::isom();
        let mut renderer = TileRenderer::new(&table);
        let id = TileId {
            z: 14,
            x: 8700,
            y: 5900,
        };
        let tile = tile_with_a_building(&table, id);
        let pixmap = renderer.render(id, &tile, id, SCALE, 512).unwrap();
        // The building's middle is at 0.3 + half its width, in a tile whose side
        // is about 2.4 km: 20 m is under 0.01 of it.
        let half = 20.0 / TileId::size(14);
        let [r, g, b, a] = pixel(&pixmap, 0.3 + half, 0.3 + half);
        assert!(a == 255 && r < 60 && g < 60 && b < 60, "{:?}", [r, g, b, a]);
        assert_eq!(
            pixel(&pixmap, 0.8, 0.8)[3],
            0,
            "empty ground is transparent"
        );
    }

    #[test]
    fn a_view_finer_than_the_data_is_the_data_overzoomed() {
        let table = SymbolTable::isom();
        let mut renderer = TileRenderer::new(&table);
        let data = TileId {
            z: 14,
            x: 8700,
            y: 5900,
        };
        let tile = tile_with_a_building(&table, data);
        // The top left quarter of the tile, one level finer: the building is in it,
        // at 0.6 of the way across.
        let view = TileId {
            z: 15,
            x: 17400,
            y: 11800,
        };
        let pixmap = renderer.render(data, &tile, view, SCALE, 256).unwrap();
        let half = 20.0 / TileId::size(15);
        let [r, _, _, a] = pixel(&pixmap, 0.6 + half, 0.6 + half);
        assert!(a == 255 && r < 60, "{a} {r}");
        // The bottom right quarter has none of it.
        let view = TileId {
            z: 15,
            x: 17401,
            y: 11801,
        };
        let pixmap = renderer.render(data, &tile, view, SCALE, 256).unwrap();
        assert_eq!(pixel(&pixmap, 0.5, 0.5)[3], 0);
    }

    #[test]
    fn a_view_outside_the_data_is_an_error_and_an_unknown_symbol_is_skipped() {
        let table = SymbolTable::isom();
        let mut renderer = TileRenderer::new(&table);
        let data = TileId {
            z: 14,
            x: 8700,
            y: 5900,
        };
        let mut tile = tile_with_a_building(&table, data);
        let elsewhere = TileId {
            z: 14,
            x: 8701,
            y: 5900,
        };
        assert!(renderer.render(data, &tile, elsewhere, SCALE, 64).is_err());
        let coarser = TileId {
            z: 13,
            x: 4350,
            y: 2950,
        };
        assert!(renderer.render(data, &tile, coarser, SCALE, 64).is_err());
        tile.objects[0].symbol = 60_000;
        let pixmap = renderer.render(data, &tile, data, SCALE, 64).unwrap();
        assert!(pixmap.pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn the_scale_is_the_viewers_choice() {
        let table = SymbolTable::isom();
        let mut renderer = TileRenderer::new(&table);
        let id = TileId {
            z: 14,
            x: 8700,
            y: 5900,
        };
        let tile = tile_with_a_building(&table, id);
        let mut drawn = |scale: f64| -> usize {
            renderer
                .render(id, &tile, id, scale, 512)
                .unwrap()
                .pixels()
                .iter()
                .filter(|p| p.alpha() > 0)
                .count()
        };
        // A building is a fill of the same ground at any scale, so the pixels
        // it covers do not depend on it; a symbol drawn in paper millimetres
        // would. The area is 40 m by 40 m at 4.8 m a pixel, about 70 pixels.
        let a = drawn(4_000.0);
        let b = drawn(15_000.0);
        assert!((40..120).contains(&a) && (40..120).contains(&b), "{a} {b}");
    }
}
