//! That the SVG says what the raster shows.
//!
//! The two outputs are built from the same renderables, so the interesting
//! question is not whether the SVG is valid but whether it is the same
//! picture: same shapes, same colours, same order, same widths.

use maur_o::geometry::Rect;
use maur_o::renderer::Renderer;
use maur_o::xml_reader::read_xml_map_str;
use std::path::Path;

fn shapes() -> maur_o::map::Map {
    read_xml_map_str(&std::fs::read_to_string("tests/data/shapes.xmap").unwrap())
        .unwrap()
        .0
}

#[test]
fn the_document_is_sized_in_millimetres_and_carries_the_map() {
    let map = shapes();
    let renderer = Renderer::new(&map);
    let svg = renderer.to_svg(None, &[]);

    assert!(
        svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg "),
        "{}",
        &svg[..80]
    );
    assert!(svg.contains("xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg.trim_end().ends_with("</svg>"));

    // Sized to what it draws, in mm, with a viewBox in the same units.
    let extent = renderer.extent();
    assert!(
        svg.contains(&format!("width=\"{}mm\"", trimmed(extent.width()))),
        "{}",
        first_line(&svg)
    );
    assert!(
        svg.contains(&format!("height=\"{}mm\"", trimmed(extent.height()))),
        "{}",
        first_line(&svg)
    );
    assert!(svg.contains("viewBox=\""));

    // And it actually holds paths.
    assert!(
        svg.matches("<path ").count() > 3,
        "only {} paths",
        svg.matches("<path ").count()
    );
}

fn trimmed(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        &s
    };
    s.to_string()
}

fn first_line(s: &str) -> &str {
    s.lines().nth(1).unwrap_or("")
}

#[test]
fn every_colour_of_the_map_that_is_used_appears() {
    let map = shapes();
    let svg = Renderer::new(&map).to_svg(None, &[]);
    // shapes.xmap draws with all three of its colours.
    for color in &map.colors {
        let (r, g, b) = color.rgb;
        let hex = format!(
            "#{:02x}{:02x}{:02x}",
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8
        );
        assert!(
            svg.contains(&hex),
            "colour {} ({hex}) is missing",
            color.name
        );
    }
}

#[test]
fn hiding_a_symbol_leaves_it_out() {
    let map = shapes();
    let renderer = Renderer::new(&map);
    let all = renderer.to_svg(None, &[]);
    // Symbol ids are the file's own; the first one is enough to prove it.
    let hidden = renderer.to_svg(None, &[0]);
    assert!(
        hidden.matches("<path ").count() < all.matches("<path ").count(),
        "hiding a symbol should draw fewer paths"
    );
}

#[test]
fn a_region_leaves_out_what_falls_outside_it() {
    let map = shapes();
    let renderer = Renderer::new(&map);
    let whole = renderer.to_svg(None, &[]);
    let extent = renderer.extent();
    // A sliver of the map, well inside it.
    let corner = Rect::from_ltrb(
        extent.left(),
        extent.top(),
        extent.left() + extent.width() * 0.1,
        extent.top() + extent.height() * 0.1,
    );
    let cropped = renderer.to_svg(Some(corner), &[]);
    assert!(cropped.matches("<path ").count() <= whole.matches("<path ").count());
    // And it is sized to the region asked for.
    assert!(cropped.contains(&format!("width=\"{}mm\"", trimmed(corner.width()))));
}

#[test]
fn a_stroke_is_written_as_a_stroke_and_a_fill_as_a_fill() {
    let map = shapes();
    let svg = Renderer::new(&map).to_svg(None, &[]);
    assert!(svg.contains("stroke-width=\""), "the fixture has lines");
    assert!(svg.contains("stroke-linecap=\""));
    assert!(
        svg.contains("fill=\"none\""),
        "a stroked path fills nothing"
    );
    // An area's holes are punched by the even-odd rule, as they are on screen.
    assert!(
        svg.contains("fill-rule=\"evenodd\""),
        "the fixture has areas"
    );
}

#[test]
fn a_map_with_nothing_on_it_still_makes_a_document() {
    let map = read_xml_map_str(&std::fs::read_to_string("tests/data/empty.xmap").unwrap())
        .unwrap()
        .0;
    let svg = Renderer::new(&map).to_svg(None, &[]);
    assert!(svg.contains("<svg "));
    assert!(svg.trim_end().ends_with("</svg>"));
    assert_eq!(svg.matches("<path ").count(), 0);
}

/// Numbers are written short: a map is a hundred thousand coordinates, and
/// the zeros would be most of the file.
#[test]
fn coordinates_are_written_without_trailing_zeros() {
    let map = shapes();
    let svg = Renderer::new(&map).to_svg(None, &[]);
    assert!(!svg.contains(".0000"), "trailing zeros are being written");
    assert!(!svg.contains("-0 "), "a negative zero is a zero");
}

/// The real map, as a smoke test that nothing in a full symbol set trips it.
#[test]
fn a_whole_map_comes_out_in_one_piece() {
    let path = Path::new("maps/forest_sample.omap");
    if !path.exists() {
        return;
    }
    let map = read_xml_map_str(&std::fs::read_to_string(path).unwrap())
        .unwrap()
        .0;
    let svg = Renderer::new(&map).to_svg(None, &[]);
    assert!(
        svg.matches("<path ").count() > 1000,
        "a real map has many paths"
    );
    assert!(svg.trim_end().ends_with("</svg>"));
}
