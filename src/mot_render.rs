//! Drawing MOT tiles with the crate's own renderer.
//!
//! A tile holds objects and the ids of their symbols; what a symbol looks like is
//! in the layer's symbol set, which this module takes from the reference map of the
//! layer's standard that the crate ships. A tile is turned back into a [`Map`] — its objects, in
//! millimetres on a paper at the scale asked for — and the renderer that draws
//! `.omap` files draws it, so a tile looks as Mapper draws the map.
//!
//! A tile has ground coordinates and a symbol set has paper millimetres, so
//! the scale joins them: the same tile at 1:4000 or 1:15000 is the same objects
//! with symbols a different size on the ground. A symbol set is drawn true to
//! the ground only at its own scale, the one its reference map is drawn at
//! ([`TileRenderer::symbol_scale`]: 1:10000 for ISOM, whose 1:15000 maps have
//! symbols two thirds that size on paper and so the same on the ground, and
//! 1:4000 for ISSprOM); [`TileRenderer::render`] takes the scale, so another
//! is possible but draws the symbols bigger or smaller than the maps did. A
//! Mercator meter is longer than a ground meter by about `1 / cos(latitude)`,
//! a little more north-south than east-west
//! ([`crate::mot::ground_per_mercator`]), taken at the
//! middle of the tile holding the data: every view of one tile is then on one
//! paper, and a fill pattern's elements (laid out on the paper) fall in the
//! same places in each, not a little apart across the edges between views.
//!
//! Every fill pattern is turned with the map and laid out from the pattern
//! origin the tile keeps for the object, near the object (see
//! [`crate::mot_convert`]), as Mapper lays out a rotatable one. Mapper lays a
//! pattern which is not rotatable out from the paper's origin, square to the
//! paper; in a tile the paper is the tile's, so it would start again at each
//! tile's corner and lose the map's turn.
//!
//! Any tile of the pyramid can be drawn from the tile of a coarser or finer level
//! that holds its ground: [`TileRenderer::render`] takes the tile with the data
//! and the tile to draw, which may be a part of it (a view finer than the finest
//! level is that level, overzoomed).
//!
//! A line whose symbol lays dashes or symbols out along the whole of it is in
//! the tile whole, not cut ([`crate::mot_symbols::SymbolEntry::whole`]), because
//! how Mapper spaces them depends on the length of the line: drawn from a piece
//! they would not meet the neighbouring tile's. A symbol which shows at least
//! one dot or tag along a line shows it once per line, not once per section
//! between dash points as Mapper does ([`Renderer::for_tiles`]): the dash
//! points a mapper put to keep a short piece of a line bare are kept.

use tiny_skia::{
    BlendMode, FillRule, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform,
};

use crate::geometry::Rect;
use crate::map::{Coord, Map, Object, ObjectKind, PathObject, Point, Symbol, TextObject};
use crate::mot::{
    ground_per_mercator, lat_lon, node_flag, Shape, Tile, TileId, TileObject, COVERAGE_SYMBOL,
};
use crate::mot_symbols::SymbolTable;
use crate::renderer::Renderer;
use crate::xml_reader::read_xml_map_str;

/// How far past the window, in millimetres of paper, an object may reach and
/// still be drawn into it: the widest symbols are a few millimetres.
const REACH_MM: f64 = 10.0;

/// How the areas maps cover, which a tile holds as objects of the
/// [`COVERAGE_SYMBOL`], are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageStyle {
    /// Not drawn.
    None,
    /// As an outline only: the edge of what a map covers, over the map.
    Outline,
    /// As a tinted area with an outline: where there are no objects to draw,
    /// at the zooms too coarse for them, this is all that says a map is there.
    Filled,
}

/// Draws tiles. It keeps the symbol set and one map to put each tile's objects
/// in, so it is made once and used for as many tiles as there are; it is not
/// shared between threads, each of which makes its own.
pub struct TileRenderer {
    template: Map,
    /// For each id of the symbol table, its symbol's index in the template.
    symbol_index: Vec<Option<usize>>,
    /// The scale the symbol set is drawn at in its reference map.
    symbol_scale: f64,
}

impl TileRenderer {
    /// A renderer for the symbols of `table`, drawn as its reference map draws them.
    pub fn new(table: &SymbolTable) -> TileRenderer {
        let (mut template, _) =
            read_xml_map_str(table.reference()).expect("the shipped symbol sets are well-formed");
        template.objects.clear();
        // Every pattern turned with the map and laid out from the object's
        // pattern origin (see the module's doc).
        fn turn_with_the_map(symbols: &mut [Symbol]) {
            for symbol in symbols {
                match symbol {
                    Symbol::Area(a) => a.patterns.iter_mut().for_each(|p| p.rotatable = true),
                    Symbol::Combined(c) => turn_with_the_map(&mut c.owned_parts),
                    _ => {}
                }
            }
        }
        turn_with_the_map(&mut template.symbols);
        let symbol_scale = f64::from(template.scale_denominator);
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
            symbol_scale,
        }
    }

    /// The scale (its denominator) at which the symbols are as big on the ground
    /// as on the maps drawn to the standard: the one the symbol set's reference
    /// map is drawn at. What to give [`TileRenderer::render`] for a tile to look
    /// as the maps it came from.
    pub fn symbol_scale(&self) -> f64 {
        self.symbol_scale
    }

    /// `view`, drawn at 1:`scale` into a square of `pixels` pixels, from the
    /// objects of `tile`, which is the tile `data`. At a `scale` other than
    /// [`TileRenderer::symbol_scale`] the symbols are bigger or smaller on the
    /// ground than on the maps. `view` is `data` or a part of
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
        coverage: CoverageStyle,
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
        // The data tile's latitude, not the view's: the same paper for every view
        // of the tile, or a pattern's elements would move from view to view.
        let data_size = TileId::size(data.z);
        let (lat, _) = lat_lon(data_west + data_size / 2.0, data_north - data_size / 2.0);
        // Millimetres of paper per Mercator meter, and per unit of this tile.
        // East-west and north-south: the paper is the ground's, and Mercator
        // stretches the two a little differently.
        let (gx, gy) = ground_per_mercator(lat);
        let per_meter = (gx * 1000.0 / scale, gy * 1000.0 / scale);
        let grid_m = f64::from(tile.grid_cm) / 100.0;
        let per_unit = (grid_m * per_meter.0, grid_m * per_meter.1);
        // The window to draw, in paper millimetres from the data tile's corner.
        let window = Rect::new(
            (view_west - data_west) * per_meter.0,
            (data_north - view_north) * per_meter.1,
            view_size * per_meter.0,
            view_size * per_meter.1,
        );
        let near = window.adjusted(-REACH_MM, -REACH_MM, REACH_MM, REACH_MM);

        self.template.scale_denominator = scale.round().clamp(1.0, f64::from(i32::MAX)) as i32;
        let mut objects = Vec::new();
        // The same window in the tile's own units, to cull objects before the work
        // of building them: a view into a tile which holds thousands.
        let (u0, u1) = (near.left() / per_unit.0, near.right() / per_unit.0);
        let (v0, v1) = (near.top() / per_unit.1, near.bottom() / per_unit.1);
        for o in &tile.objects {
            let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for n in &o.nodes {
                (x0, x1) = (x0.min(n.x), x1.max(n.x));
                (y0, y1) = (y0.min(n.y), y1.max(n.y));
            }
            let outside = f64::from(x1) < u0
                || f64::from(x0) > u1
                || f64::from(y1) < v0
                || f64::from(y0) > v1;
            if !outside {
                objects.extend(self.object(o, per_unit));
            }
        }
        self.template.objects = objects;

        let mut pixmap = Pixmap::new(pixels, pixels).ok_or("cannot make an image that size")?;
        let ppm = (
            f64::from(pixels) / window.width(),
            f64::from(pixels) / window.height(),
        );
        let transform = Transform::from_translate(-window.left() as f32, -window.top() as f32)
            .post_concat(Transform::from_scale(ppm.0 as f32, ppm.1 as f32));
        // Built for the window only: a large patterned area (stony ground,
        // say) is laid out where it is seen, not over the whole tile.
        Renderer::for_tiles(&self.template, window).paint_rect(
            &mut pixmap.as_mut(),
            transform,
            Some(window),
            &[],
        );
        self.template.objects.clear();
        if coverage != CoverageStyle::None {
            draw_coverage(
                &mut pixmap,
                tile,
                coverage,
                pixel_map(data, tile, view, pixels),
            );
        }
        Ok(pixmap)
    }

    /// The map's object for a tile's, in paper millimetres; `None` where its
    /// symbol is not in the template.
    fn object(&self, o: &TileObject, per_unit: (f64, f64)) -> Option<Object> {
        let index = (*self.symbol_index.get(usize::from(o.symbol))?)?;
        let point = |x: i32, y: i32| (f64::from(x) * per_unit.0, f64::from(y) * per_unit.1);
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
                    .map(|(w, h)| (f64::from(w) * per_unit.0, f64::from(h) * per_unit.1)),
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

/// How a node of the tile `data`, in its grid units, falls in the `pixels` square
/// `view` is drawn into.
fn pixel_map(
    data: TileId,
    tile: &Tile,
    view: TileId,
    pixels: u32,
) -> impl Fn(i32, i32) -> (f32, f32) {
    let (view_west, view_north) = view.origin();
    let (data_west, data_north) = data.origin();
    let grid_m = f64::from(tile.grid_cm) / 100.0;
    let per_meter = f64::from(pixels) / TileId::size(view.z);
    let (dx, dy) = (view_west - data_west, data_north - view_north);
    move |x, y| {
        (
            ((f64::from(x) * grid_m - dx) * per_meter) as f32,
            ((f64::from(y) * grid_m - dy) * per_meter) as f32,
        )
    }
}

/// The area the maps of `tile` cover, as a mask over the `pixels` square `view`
/// is drawn into: what a layer above hides of the layers below it. `None` where
/// the tile has no coverage.
pub fn coverage_mask(data: TileId, tile: &Tile, view: TileId, pixels: u32) -> Option<Mask> {
    let to_pixel = pixel_map(data, tile, view, pixels);
    let mut mask = Mask::new(pixels, pixels)?;
    let mut any = false;
    for o in tile.objects.iter().filter(|o| o.symbol == COVERAGE_SYMBOL) {
        if o.nodes.len() < 3 {
            continue;
        }
        let mut path = PathBuilder::new();
        for (i, n) in o.nodes.iter().enumerate() {
            let (x, y) = to_pixel(n.x, n.y);
            if i == 0 {
                path.move_to(x, y);
            } else {
                path.line_to(x, y);
            }
        }
        path.close();
        if let Some(path) = path.finish() {
            mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
            any = true;
        }
    }
    any.then_some(mask)
}

/// Puts the drawings of layers one over another, the one with the lowest
/// priority first: each hides, inside the coverage it comes with, what is below
/// it, and is drawn over the rest. Where a place is mapped in two symbol sets,
/// only the higher shows. `None` for no layers.
pub fn composite(layers: Vec<(Pixmap, Option<Mask>)>) -> Option<Pixmap> {
    let mut layers = layers.into_iter();
    // Nothing is below the lowest, so its coverage hides nothing.
    let (mut canvas, _) = layers.next()?;
    let clear = Paint {
        blend_mode: BlendMode::Clear,
        ..Paint::default()
    };
    for (pixmap, mask) in layers {
        if let (Some(mask), Some(rect)) = (
            mask,
            tiny_skia::Rect::from_xywh(0.0, 0.0, canvas.width() as f32, canvas.height() as f32),
        ) {
            canvas.fill_rect(rect, &clear, Transform::identity(), Some(&mask));
        }
        canvas.draw_pixmap(
            0,
            0,
            pixmap.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }
    Some(canvas)
}

/// Draws the coverage areas of `tile` on `pixmap`, `to_pixel` turning a node into
/// a pixel position. The edges a tile cut an area along (flagged
/// [`node_flag::CLIPPED`]) are not outlined, or the border of every tile would
/// show.
fn draw_coverage(
    pixmap: &mut Pixmap,
    tile: &Tile,
    style: CoverageStyle,
    to_pixel: impl Fn(i32, i32) -> (f32, f32),
) {
    let mut fill = Paint::default();
    fill.set_color_rgba8(30, 110, 230, 40);
    fill.anti_alias = true;
    let mut line = Paint::default();
    line.set_color_rgba8(30, 110, 230, 220);
    line.anti_alias = true;
    let stroke = Stroke {
        width: 2.0,
        ..Stroke::default()
    };
    let cut = node_flag::CLIPPED as u8;
    for o in tile.objects.iter().filter(|o| o.symbol == COVERAGE_SYMBOL) {
        let points: Vec<(f32, f32)> = o.nodes.iter().map(|n| to_pixel(n.x, n.y)).collect();
        if points.len() < 3 {
            continue;
        }
        if style == CoverageStyle::Filled {
            let mut path = PathBuilder::new();
            path.move_to(points[0].0, points[0].1);
            for p in &points[1..] {
                path.line_to(p.0, p.1);
            }
            path.close();
            if let Some(path) = path.finish() {
                pixmap.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);
            }
        }
        for (i, pair) in points.windows(2).enumerate() {
            if o.nodes[i].flags & cut != 0 {
                continue;
            }
            let mut edge = PathBuilder::new();
            edge.move_to(pair[0].0, pair[0].1);
            edge.line_to(pair[1].0, pair[1].1);
            if let Some(edge) = edge.finish() {
                pixmap.stroke_path(&edge, &line, &stroke, Transform::identity(), None);
            }
        }
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
        let pixmap = renderer
            .render(id, &tile, id, SCALE, 512, CoverageStyle::None)
            .unwrap();
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

    /// A tile with a coverage ring over the middle half of it, whose west edge
    /// lies on the tile's own border and is flagged as a cut.
    fn tile_with_coverage() -> Tile {
        let side = (TileId::size(14) * 100.0 / 5.0) as i32;
        let (a, b) = (side / 4, side * 3 / 4);
        let cut = node_flag::CLIPPED as u8;
        let mut tile = Tile::new(5);
        tile.objects.push(TileObject {
            id: 9,
            symbol: COVERAGE_SYMBOL,
            rotation: 0.0,
            shape: Shape::Path(None),
            nodes: vec![
                node(0, a, cut), // the west edge runs down the tile's border
                node(0, b, 0),
                node(b, b, 0),
                node(b, a, 0),
                node(0, a, node_flag::CLOSE_POINT as u8),
            ],
        });
        tile
    }

    #[test]
    fn coverage_is_a_tint_below_the_symbols_and_an_outline_over_them() {
        let table = SymbolTable::isom();
        let mut renderer = TileRenderer::new(&table);
        let id = TileId {
            z: 14,
            x: 8700,
            y: 5900,
        };
        let tile = tile_with_coverage();
        let draw = |renderer: &mut TileRenderer, style| {
            renderer.render(id, &tile, id, SCALE, 256, style).unwrap()
        };
        let none = draw(&mut renderer, CoverageStyle::None);
        assert!(none.pixels().iter().all(|p| p.alpha() == 0), "not drawn");
        // Filled: the inside is tinted, outside it is clear.
        let filled = draw(&mut renderer, CoverageStyle::Filled);
        let inside = pixel(&filled, 0.4, 0.5);
        assert!(inside[3] > 10 && inside[2] > inside[0], "{inside:?}");
        assert_eq!(pixel(&filled, 0.9, 0.5)[3], 0);
        // Outline: the inside is clear, and the edge is drawn, but not the one
        // that lies on the tile's border, which is a cut and not the map's.
        let outline = draw(&mut renderer, CoverageStyle::Outline);
        assert_eq!(pixel(&outline, 0.4, 0.5)[3], 0);
        assert!(pixel(&outline, 0.75, 0.5)[3] > 100, "the east edge");
        assert_eq!(pixel(&outline, 0.004, 0.5)[3], 0, "the cut edge");
    }

    #[test]
    fn a_layer_hides_the_ones_below_it_inside_its_coverage_only() {
        let id = TileId {
            z: 14,
            x: 8700,
            y: 5900,
        };
        let tile = tile_with_coverage();
        // Coverage over the middle half, the west edge on the tile's border.
        let mask = coverage_mask(id, &tile, id, 64).expect("a tile with coverage has a mask");
        let size = |colour: tiny_skia::Color| {
            let mut p = Pixmap::new(64, 64).unwrap();
            p.fill(colour);
            p
        };
        let red = size(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        let mut clear = Pixmap::new(64, 64).unwrap();
        clear.fill(tiny_skia::Color::TRANSPARENT);
        // The upper layer draws nothing: inside its coverage the lower one is
        // hidden, outside it, it shows.
        let stacked = composite(vec![(red.clone(), None), (clear, Some(mask))]).unwrap();
        assert_eq!(
            pixel(&stacked, 0.4, 0.5)[3],
            0,
            "hidden inside the coverage"
        );
        assert_eq!(
            pixel(&stacked, 0.9, 0.5),
            [255, 0, 0, 255],
            "shows outside it"
        );
        // And one with something to draw is drawn over what it hides.
        let blue = size(tiny_skia::Color::from_rgba8(0, 0, 255, 255));
        let mask = coverage_mask(id, &tile, id, 64).unwrap();
        let stacked = composite(vec![(red, None), (blue, Some(mask))]).unwrap();
        assert_eq!(pixel(&stacked, 0.4, 0.5), [0, 0, 255, 255]);
        assert!(composite(Vec::new()).is_none());
        assert!(coverage_mask(id, &Tile::new(5), id, 64).is_none());
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
        let pixmap = renderer
            .render(data, &tile, view, SCALE, 256, CoverageStyle::None)
            .unwrap();
        let half = 20.0 / TileId::size(15);
        let [r, _, _, a] = pixel(&pixmap, 0.6 + half, 0.6 + half);
        assert!(a == 255 && r < 60, "{a} {r}");
        // The bottom right quarter has none of it.
        let view = TileId {
            z: 15,
            x: 17401,
            y: 11801,
        };
        let pixmap = renderer
            .render(data, &tile, view, SCALE, 256, CoverageStyle::None)
            .unwrap();
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
        assert!(renderer
            .render(data, &tile, elsewhere, SCALE, 64, CoverageStyle::None)
            .is_err());
        let coarser = TileId {
            z: 13,
            x: 4350,
            y: 2950,
        };
        assert!(renderer
            .render(data, &tile, coarser, SCALE, 64, CoverageStyle::None)
            .is_err());
        tile.objects[0].symbol = 60_000;
        let pixmap = renderer
            .render(data, &tile, data, SCALE, 64, CoverageStyle::None)
            .unwrap();
        assert!(pixmap.pixels().iter().all(|p| p.alpha() == 0));
    }

    /// A large area of a dot-pattern symbol (with a curved edge and a hole)
    /// drawn through a small window: laying the pattern out in the window
    /// only must not move a single dot.
    #[test]
    fn a_pattern_laid_out_in_the_window_only_draws_the_same_pixels() {
        use crate::map::coord_flag::{CLOSE_POINT, CURVE_START};
        let (mut map, _) = read_xml_map_str(SymbolTable::isom().reference()).unwrap();
        map.objects.clear();
        for (code, rotation) in [("211", 0.0), ("210", 0.4), ("212", 2.0)] {
            let index = map
                .symbols
                .iter()
                .position(|s| s.code() == code && !s.is_helper_symbol())
                .unwrap();
            let mut object = Object::new(ObjectKind::Path(PathObject {
                pattern_rotation: rotation,
                pattern_origin: Point::new(3.3, -7.1),
            }));
            object.symbol_id = map.symbol_ids[index];
            object.symbol_index = Some(index);
            object.coords = vec![
                Coord::new(-12.0, -10.0, 0),
                Coord::new(105.0, -9.0, CURVE_START),
                Coord::new(120.0, 25.0, 0),
                Coord::new(118.0, 75.0, 0),
                Coord::new(102.0, 108.0, 0),
                Coord::new(-8.0, 100.0, 0),
                Coord::new(-12.0, -10.0, CLOSE_POINT),
                Coord::new(38.0, 38.0, 0),
                Coord::new(50.0, 38.0, 0),
                Coord::new(50.0, 50.0, 0),
                Coord::new(38.0, 38.0, CLOSE_POINT),
            ];
            map.objects.clear();
            map.objects.push(object);
            for window in [
                Rect::new(30.9, 35.2, 20.0, 20.0),
                Rect::new(105.0, 60.0, 25.0, 25.0),
            ] {
                let transform =
                    Transform::from_translate(-window.left() as f32, -window.top() as f32)
                        .post_concat(Transform::from_scale(10.0, 10.0));
                let draw = |renderer: Renderer| {
                    let mut pixmap = Pixmap::new(256, 256).unwrap();
                    renderer.paint_rect(&mut pixmap.as_mut(), transform, Some(window), &[]);
                    pixmap
                };
                let whole = draw(Renderer::new(&map));
                let within = draw(Renderer::new_within(&map, window));
                assert!(
                    whole.pixels().iter().any(|p| p.alpha() > 0),
                    "{code}: drawn"
                );
                assert!(whole.data() == within.data(), "{code} at {window:?}");
            }
        }
    }

    #[test]
    fn each_symbol_set_is_drawn_at_the_scale_of_its_reference_map() {
        assert_eq!(
            TileRenderer::new(&SymbolTable::isom()).symbol_scale(),
            10_000.0
        );
        assert_eq!(
            TileRenderer::new(&SymbolTable::issprom()).symbol_scale(),
            4_000.0
        );
    }

    #[test]
    fn a_line_is_as_wide_on_the_ground_as_on_the_map_at_the_symbol_scale() {
        // A contour of ISOM is 0.14 mm on a 1:15000 map, 2.1 m on the ground; the
        // 1:10000 symbol set draws it 0.21 mm wide, the same 2.1 m at its scale.
        let table = SymbolTable::isom();
        let mut renderer = TileRenderer::new(&table);
        let id = TileId {
            z: 14,
            x: 8700,
            y: 5900,
        };
        let size = TileId::size(id.z);
        let units = (size * 100.0 / 5.0) as i32;
        let mut tile = Tile::new(5);
        tile.objects.push(TileObject {
            id: 0,
            symbol: table.id_of("101").unwrap(),
            rotation: 0.0,
            shape: Shape::Path(None),
            nodes: vec![node(units / 2, 0, 0), node(units / 2, units, 0)],
        });
        let scale = renderer.symbol_scale();
        let (lat, _) = lat_lon(id.origin().0 + size / 2.0, id.origin().1 - size / 2.0);
        // 2048 pixels for about 2.4 km of Mercator: a pixel about 0.85 m of ground.
        let pixels = 2048;
        let ground_per_pixel = size * lat.to_radians().cos() / f64::from(pixels);
        let pixmap = renderer
            .render(id, &tile, id, scale, pixels, CoverageStyle::None)
            .unwrap();
        let row = pixels as usize / 3 * pixels as usize;
        let wide = pixmap.pixels()[row..row + pixels as usize]
            .iter()
            .map(|p| f64::from(p.alpha()) / 255.0)
            .sum::<f64>();
        let meters = wide * ground_per_pixel;
        assert!((meters - 2.1).abs() < 0.3, "{meters} m");
    }

    /// The tile `data` with one square area of the symbol `code`, `side` Mercator
    /// meters across around the Mercator point `middle`, its pattern laid out
    /// from the Mercator point `origin`.
    fn tile_with_an_area(
        table: &SymbolTable,
        data: TileId,
        code: &str,
        middle: (f64, f64),
        side: f64,
        origin: (f64, f64),
    ) -> Tile {
        let (west, north) = data.origin();
        let unit = |(x, y): (f64, f64)| {
            (
                ((x - west) / 0.05).round() as i32,
                ((north - y) / 0.05).round() as i32,
            )
        };
        let (x0, y0) = unit((middle.0 - side / 2.0, middle.1 + side / 2.0));
        let (x1, y1) = unit((middle.0 + side / 2.0, middle.1 - side / 2.0));
        let mut tile = Tile::new(5);
        tile.objects.push(TileObject {
            id: 0,
            symbol: table.id_of(code).unwrap(),
            rotation: 0.0,
            shape: Shape::Path(Some(crate::mot::Pattern {
                rotation: 0.0246,
                origin: unit(origin),
            })),
            nodes: vec![
                node(x0, y0, 0),
                node(x1, y0, 0),
                node(x1, y1, 0),
                node(x0, y1, 0),
                node(x0, y0, node_flag::CLOSE_POINT as u8),
            ],
        });
        tile
    }

    /// How many pixels of two images of one size differ by more than a shade
    /// of antialiasing: an edge a hundredth of a pixel away (the 5 cm grid
    /// rounds a point from each tile's own corner) is the same picture.
    fn differing(a: &Pixmap, b: &Pixmap) -> usize {
        a.pixels()
            .iter()
            .zip(b.pixels())
            .filter(|(p, q)| {
                let d = |x: u8, y: u8| x.abs_diff(y);
                d(p.red(), q.red())
                    .max(d(p.green(), q.green()))
                    .max(d(p.blue(), q.blue()))
                    .max(d(p.alpha(), q.alpha()))
                    > 32
            })
            .count()
    }

    fn middle(id: TileId) -> (f64, f64) {
        let (west, north) = id.origin();
        let size = TileId::size(id.z);
        (west + size / 2.0, north - size / 2.0)
    }

    /// Four views side by side are their parent view, dot for dot, though the
    /// pattern is laid out from 900 km away (a map whose paper origin is at a
    /// UTM easting of 0): every view of a data tile is drawn on one paper.
    #[test]
    fn the_views_of_a_tile_lay_a_pattern_out_alike() {
        let table = SymbolTable::isom();
        let mut renderer = TileRenderer::new(&table);
        let data = TileId {
            z: 14,
            x: 8683,
            y: 5931,
        };
        let parent = TileId {
            z: 18,
            x: 8683 * 16 + 7,
            y: 5931 * 16 + 7,
        };
        let (mx, my) = middle(parent);
        let tile = tile_with_an_area(
            &table,
            data,
            "211",
            (mx, my),
            400.0,
            (mx - 892_000.0, my - 74_000.0),
        );
        let whole = renderer
            .render(data, &tile, parent, SCALE, 512, CoverageStyle::None)
            .unwrap();
        let mut stitched = Pixmap::new(512, 512).unwrap();
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let view = TileId {
                z: 19,
                x: parent.x * 2 + dx,
                y: parent.y * 2 + dy,
            };
            let part = renderer
                .render(data, &tile, view, SCALE, 256, CoverageStyle::None)
                .unwrap();
            stitched.draw_pixmap(
                (256 * dx) as i32,
                (256 * dy) as i32,
                part.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
        let dots = whole
            .pixels()
            .iter()
            .filter(|p| p.alpha() > 0 && p.red() < 128)
            .count();
        assert!(dots > 2000, "the view is dotted: {dots}");
        let off = differing(&whole, &stitched);
        assert!(off < 100, "{off} pixels differ");
    }

    /// The same area, with the pattern origin near it that `tiles build` stores,
    /// in a tile of zoom 14 and in one of zoom 15 (another corner, another
    /// latitude) draws the same dots: a rotatable pattern (stony ground) and one
    /// that Mapper lays out square to the paper (cultivated land) alike.
    #[test]
    fn a_pattern_is_drawn_alike_from_any_tile_holding_it() {
        let table = SymbolTable::isom();
        let mut renderer = TileRenderer::new(&table);
        let coarse = TileId {
            z: 14,
            x: 8683,
            y: 5931,
        };
        let fine = TileId {
            z: 15,
            x: 8683 * 2 + 1,
            y: 5931 * 2 + 1,
        };
        let view = TileId {
            z: 17,
            x: fine.x * 4 + 1,
            y: fine.y * 4 + 2,
        };
        let (mx, my) = middle(view);
        for code in ["211", "412"] {
            let origin = (mx + 37.3, my - 81.9);
            let drawn = |renderer: &mut TileRenderer, data: TileId| {
                let tile = tile_with_an_area(&table, data, code, (mx, my), 600.0, origin);
                renderer
                    .render(data, &tile, view, SCALE, 512, CoverageStyle::None)
                    .unwrap()
            };
            let (a, b) = (drawn(&mut renderer, coarse), drawn(&mut renderer, fine));
            let off = differing(&a, &b);
            assert!(off < 100, "{code}: {off} pixels differ");
        }
    }

    #[test]
    fn a_fill_covers_the_same_ground_at_any_scale() {
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
                .render(id, &tile, id, scale, 512, CoverageStyle::None)
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

    #[test]
    fn a_dash_point_keeps_a_short_piece_of_a_line_bare_and_a_short_line_shows_its_tag() {
        // A 518 fence, whose set shows at least one tag: Mapper puts one in each
        // section between dash points, a tile one in the line.
        let (mut map, _) =
            read_xml_map_str(crate::osm_orienteering::Standard::Issprom.reference()).unwrap();
        map.objects.clear();
        let index = map.symbols.iter().position(|s| s.code() == "518").unwrap();
        let symbol_id = map.symbol_ids[index];
        let window = Rect::new(-10.0, -10.0, 40.0, 40.0);
        // How much is drawn of a line through `points` (mm, flags).
        let mut drawn = |points: &[(f64, f64, i32)], tiles: bool| -> usize {
            let mut object = Object::new(ObjectKind::Path(PathObject::default()));
            object.symbol_id = symbol_id;
            object.symbol_index = Some(index);
            object.coords = points
                .iter()
                .map(|&(x, y, f)| Coord::new(x, y, f))
                .collect();
            map.objects = vec![object];
            let renderer = if tiles {
                Renderer::for_tiles(&map, window)
            } else {
                Renderer::new_within(&map, window)
            };
            renderer.renderables.len()
        };
        let dash = crate::map::coord_flag::DASH_POINT;

        // 20 mm with tags, then a 1.4 mm return the mapper kept bare.
        let long = [(0.0, 0.0, 0), (20.0, 0.0, 0)];
        let with_return = [(0.0, 0.0, 0), (20.0, 0.0, dash), (20.0, 1.4, 0)];
        assert_eq!(drawn(&with_return, true), drawn(&long, true));
        assert!(drawn(&with_return, false) > drawn(&long, false));

        // A 2 mm fence split by a dash point: one tag, as without the dash
        // point, where Mapper puts one in each half.
        let short = [(0.0, 0.0, 0), (2.0, 0.0, 0)];
        let split = [(0.0, 0.0, 0), (1.0, 0.0, dash), (2.0, 0.0, 0)];
        assert_eq!(drawn(&split, true), drawn(&short, false));
        assert!(drawn(&split, false) > drawn(&split, true));
    }
}
