//! That leaving a renderable out because it misses the region being drawn
//! does not change the drawing.
//!
//! This is the property a viewport renderer rests on. Culling a renderable
//! whose bounding box misses the region is sound only if it could not have
//! put ink there, and a bounding box too small for its geometry -- a stroke
//! measured without its pen width, a text path measured in font units --
//! would show up here as a mark which goes missing when the cull is on.
//!
//! The test is therefore culled against *un*culled at one and the same
//! transform and pixmap, where the two must agree to the bit. Comparing a
//! tile against the same region of a whole-map rendering would not be that
//! test: `tiny_skia`'s transform is `f32`, so shifting one by a whole number
//! of pixels does not shift the rasterization by exactly that much, and the
//! antialiased edges move by a fraction of a pixel. That difference is real
//! but is not culling, and it is why `paint_rect` is documented as drawing a
//! region rather than as tiling a larger image.
//!
//! The regions are deliberately small and offset, so that shapes cross their
//! boundaries rather than sitting comfortably inside one.

use maur_o::geometry::Rect;
use maur_o::renderer::Renderer;
use maur_o::xml_reader::read_xml_map_str;
use tiny_skia::{Pixmap, Transform};

const RESOLUTION: f64 = 3.0;

/// The map, the frame-enlarged extent it is measured by, and its scale.
fn load(map_path: &str) -> (maur_o::map::Map, Rect, f64) {
    let content = std::fs::read_to_string(map_path).unwrap();
    let (map, _) = read_xml_map_str(&content).unwrap();
    let pixel_per_mm = RESOLUTION * map.scale_denominator as f64 / 1000.0;
    // A small frame, so that the edges of the drawing are inside the image.
    let extent = Renderer::new(&map).extent().adjusted(-5.0, -5.0, 5.0, 5.0);
    (map, extent, pixel_per_mm)
}

/// Every region of a map, culled and unculled, must come out the same.
#[test]
fn culling_a_region_draws_what_not_culling_it_draws() {
    let (map, extent, pixel_per_mm) = load("tests/data/shapes.xmap");
    let renderer = Renderer::new(&map);
    let width = (extent.width() * pixel_per_mm).round() as u32;
    let height = (extent.height() * pixel_per_mm).round() as u32;
    let base = Transform::from_translate(-extent.left() as f32, -extent.top() as f32).post_concat(
        Transform::from_scale(pixel_per_mm as f32, pixel_per_mm as f32),
    );

    // A region size which divides neither dimension, so the shapes are cut in
    // different places each time.
    let size = 37;
    let mut regions = 0;
    let mut inked = 0;

    for py in (0..height).step_by(size as usize) {
        for px in (0..width).step_by(size as usize) {
            let (w, h) = (size.min(width - px), size.min(height - py));
            let transform = base.post_translate(-(px as f32), -(py as f32));
            let clip = Rect::from_ltrb(
                extent.left() + px as f64 / pixel_per_mm,
                extent.top() + py as f64 / pixel_per_mm,
                extent.left() + (px + w) as f64 / pixel_per_mm,
                extent.top() + (py + h) as f64 / pixel_per_mm,
            );

            let mut culled = Pixmap::new(w, h).unwrap();
            culled.fill(tiny_skia::Color::WHITE);
            renderer.paint_rect(&mut culled.as_mut(), transform, Some(clip), &[]);

            let mut unculled = Pixmap::new(w, h).unwrap();
            unculled.fill(tiny_skia::Color::WHITE);
            renderer.paint_rect(&mut unculled.as_mut(), transform, None, &[]);

            assert_eq!(
                culled.data(),
                unculled.data(),
                "culling changed the region at ({px},{py})"
            );
            regions += 1;
            if unculled.pixels().iter().any(|p| p.red() != 255) {
                inked += 1;
            }
        }
    }

    assert!(
        regions > 1,
        "the map should have needed more than one region"
    );
    assert!(
        inked > 1,
        "most regions should have had something drawn in them"
    );
}

/// Culling is what makes a tile cheap; this checks it is actually happening,
/// by asking for a region far outside the map and getting back blank paper.
#[test]
fn a_region_off_the_map_is_left_blank() {
    let (map, extent, pixel_per_mm) = load("tests/data/shapes.xmap");
    let far = extent.adjusted(10_000.0, 10_000.0, 10_000.0, 10_000.0);
    let renderer = Renderer::new(&map);

    let mut pixmap = Pixmap::new(64, 64).unwrap();
    pixmap.fill(tiny_skia::Color::WHITE);
    let page_transform =
        Transform::from_translate(-far.left() as f32, -far.top() as f32).post_concat(
            Transform::from_scale(pixel_per_mm as f32, pixel_per_mm as f32),
        );
    renderer.paint_rect(&mut pixmap.as_mut(), page_transform, Some(far), &[]);

    assert!(
        pixmap
            .pixels()
            .iter()
            .all(|p| p.alpha() == 255 && p.red() == 255 && p.green() == 255 && p.blue() == 255),
        "a region off the map should be blank"
    );
}

/// Hiding every symbol the map uses leaves nothing drawn; hiding none leaves
/// the map as it was.
#[test]
fn hidden_symbols_are_left_undrawn() {
    let (map, extent, pixel_per_mm) = load("tests/data/shapes.xmap");
    let renderer = Renderer::new(&map);
    let width = (extent.width() * pixel_per_mm).round() as u32;
    let height = (extent.height() * pixel_per_mm).round() as u32;
    let page_transform =
        Transform::from_translate(-extent.left() as f32, -extent.top() as f32).post_concat(
            Transform::from_scale(pixel_per_mm as f32, pixel_per_mm as f32),
        );

    let all_ids: Vec<i32> = map.objects.iter().map(|o| o.symbol_id).collect();
    assert!(!all_ids.is_empty(), "the fixture should have objects");

    let mut hidden = Pixmap::new(width, height).unwrap();
    hidden.fill(tiny_skia::Color::WHITE);
    renderer.paint_rect(&mut hidden.as_mut(), page_transform, None, &all_ids);
    assert!(
        hidden
            .pixels()
            .iter()
            .all(|p| p.red() == 255 && p.green() == 255 && p.blue() == 255),
        "hiding every symbol should leave blank paper"
    );

    let mut shown = Pixmap::new(width, height).unwrap();
    shown.fill(tiny_skia::Color::WHITE);
    renderer.paint_rect(&mut shown.as_mut(), page_transform, None, &[]);
    assert!(
        shown
            .pixels()
            .iter()
            .any(|p| p.red() != 255 || p.green() != 255 || p.blue() != 255),
        "hiding nothing should draw the map"
    );
}
