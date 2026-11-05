//! That a host which supplies its own fonts gets its text set in them.
//!
//! A WebAssembly build has no system fonts to scan, so it hands the bytes it
//! wants to `init_font_database` instead. The failure this guards against is
//! a quiet one: `add_text` returns without adding anything when no face
//! matches, so a database which is empty, or which holds fonts under names
//! the map does not ask for, loses every piece of text on the map without
//! saying so.
//!
//! The database is a process-wide `OnceLock` and can only be set once, which
//! is why this is a test binary of its own.

use maur_o::map::Symbol;
use maur_o::renderer::Renderer;
use maur_o::text::init_font_database;
use maur_o::xml_reader::read_xml_map_str;
use tiny_skia::{Pixmap, Transform};

/// A font which is not the one the fixture asks for, standing in for the
/// arbitrary font a host happens to ship.
const FONT: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";

#[test]
fn text_is_set_in_a_font_the_host_supplied() {
    let Ok(font) = std::fs::read(FONT) else {
        eprintln!("skipped: {FONT} is not installed");
        return;
    };
    assert!(
        init_font_database(vec![font]),
        "the database should not have been built yet"
    );

    let content = std::fs::read_to_string("tests/data/shapes.xmap").unwrap();
    let (map, _) = read_xml_map_str(&content).unwrap();

    // The fixture asks for a font by name, and it is not the one just loaded
    // -- which is the point: the fallback has to carry it.
    let asked_for: Vec<&str> = map
        .symbols
        .iter()
        .filter_map(|s| match s {
            Symbol::Text(t) => Some(t.font_family.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !asked_for.is_empty(),
        "the fixture should have text symbols"
    );

    let text_ids: Vec<i32> = map
        .objects
        .iter()
        .filter(|o| {
            matches!(
                o.symbol_index.map(|i| &map.symbols[i]),
                Some(Symbol::Text(_))
            )
        })
        .map(|o| o.symbol_id)
        .collect();
    assert!(!text_ids.is_empty(), "the fixture should have text objects");

    let renderer = Renderer::new(&map);
    let pixel_per_mm = 3.0 * map.scale_denominator as f64 / 1000.0;
    let extent = renderer.extent().adjusted(-5.0, -5.0, 5.0, 5.0);
    let width = (extent.width() * pixel_per_mm).round() as u32;
    let height = (extent.height() * pixel_per_mm).round() as u32;
    let transform =
        Transform::from_translate(-extent.left() as f32, -extent.top() as f32).post_concat(
            Transform::from_scale(pixel_per_mm as f32, pixel_per_mm as f32),
        );

    let paint = |hidden: &[i32]| {
        let mut pixmap = Pixmap::new(width, height).unwrap();
        pixmap.fill(tiny_skia::Color::WHITE);
        renderer.paint_rect(&mut pixmap.as_mut(), transform, None, hidden);
        pixmap
    };

    let with_text = paint(&[]);
    let without_text = paint(&text_ids);
    assert_ne!(
        with_text.data(),
        without_text.data(),
        "hiding the text changed nothing, so none was drawn: the supplied \
         font did not carry the families {asked_for:?}"
    );
}
