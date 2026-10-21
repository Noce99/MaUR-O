//! What OpenStreetMap features look like: a symbol set reproducing
//! [openstreetmap-carto](https://github.com/gravitystorm/openstreetmap-carto),
//! the "Standard" style of openstreetmap.org, at zoom 16, and the tag rules
//! deciding which feature gets which symbol.
//!
//! # From pixels to millimetres
//!
//! Carto is written in pixels, for one zoom at a time, and a tile pixel is
//! taken to be [`PIXEL_MM`] = 0.28 mm — the "standardized rendering pixel"
//! of the OGC, which Mapnik's own scale denominators assume too. So a
//! symbol here is carto's width in pixels times 0.28, and the map is drawn
//! at the scale at which one zoom-16 pixel covers what it covers on the
//! ground (see [`crate::osm_convert::zoom_scale`]). Rendered at the matching
//! resolution, the map and a screenshot of openstreetmap.org line up pixel
//! for pixel.
//!
//! # Drawing order
//!
//! In Mapper the colour table *is* the drawing order: everything in a colour
//! is drawn before everything in any colour above it. Carto draws layer by
//! layer instead — landcover, water, buildings, tunnels, barriers, road
//! casings, road fills, trees — and that sequence is what the table below
//! encodes, top of the map first. Two things carto does need more than that:
//!
//! - A road is two strokes, a wide casing and a narrower fill. Every casing
//!   colour sits below every fill colour, so where two roads meet their
//!   fills merge over both casings, as they do on openstreetmap.org.
//! - Carto draws landcover polygons largest first, so a small park sits on a
//!   large residential area whatever its type. A colour table cannot sort
//!   by size; it orders the *types* instead, those which usually lie inside
//!   others (a pitch, a playground, grass) above those which usually
//!   surround them (residential, farmland).
//!
//! Colours are named as carto names its variables, and computed the way it
//! computes them: `darken(@forest, 10%)` is [`adjust`]`("#add19e", -0.10,
//! 0.0)`, the same HSL arithmetic Less does.

use std::collections::HashMap;

use crate::map::{
    cap_style, fill_pattern_type, join_style, AreaSymbol, Color, CombinedSymbol, FillPattern,
    LineSymbol, PointSymbol, Symbol,
};
use crate::osm::Tags;

/// The size of one carto pixel on the paper, in mm.
pub const PIXEL_MM: f64 = 0.28;

/// The zoom level whose look the symbol set reproduces.
pub const ZOOM: u32 = 16;

/// Carto pixels to mm.
fn px(pixels: f64) -> f64 {
    pixels * PIXEL_MM
}

/// Red, green, blue in `[0, 1]` from `#rrggbb`.
///
/// Panics on anything else: the colours are this module's own literals.
fn rgb(hex: &str) -> (f64, f64, f64) {
    let hex = hex.trim_start_matches('#');
    let channel = |i: usize| {
        f64::from(u8::from_str_radix(&hex[i..i + 2], 16).expect("a #rrggbb colour")) / 255.0
    };
    (channel(0), channel(2), channel(4))
}

/// `#rrggbb` with its HSL lightness and saturation moved by `lightness` and
/// `saturation`, each an absolute amount in `[-1, 1]` — Less's `lighten`/
/// `darken` and `saturate`/`desaturate`, which carto's colours are written
/// in. Each result is clamped to `[0, 1]`, as Less clamps it.
pub fn adjust(hex: &str, lightness: f64, saturation: f64) -> String {
    let (r, g, b) = rgb(hex);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    let (h, s) = if d == 0.0 {
        (0.0, 0.0)
    } else {
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (h * 60.0, s)
    };
    let l = (l + lightness).clamp(0.0, 1.0);
    let s = (s + saturation).clamp(0.0, 1.0);

    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

/// The colour table, top of the map first: `(key, #rrggbb, opacity)`.
///
/// The key is what the symbols below name a colour by; it doubles as the
/// colour's name in the map file.
fn palette() -> Vec<(&'static str, String, f64)> {
    let c = |hex: &str| hex.to_string();
    let forest = "#add19e";
    let park = "#c8facc";
    let leisure = adjust(park, 0.05, 0.0);
    let pitch = "#88e0be";
    let parking = "#eeeeee";
    let place_of_worship = "#d0d0d0";
    let building = "#d9d0c9";
    let building_major = adjust(building, -0.10, 0.0);
    vec![
        // #trees, drawn over the roads.
        ("tree", adjust(forest, -0.10, 0.0), 0.6),
        // #roads-fill: railways, then road fills by importance, then paths.
        ("rail light", c("#ffffff"), 1.0),
        ("rail", c("#707070"), 1.0),
        ("rail service", c("#aaaaaa"), 1.0),
        ("light rail", c("#666666"), 1.0),
        ("tram", c("#6e6e6e"), 1.0),
        ("motorway fill", c("#e892a2"), 1.0),
        ("trunk fill", c("#f9b29c"), 1.0),
        ("primary fill", c("#fcd6a4"), 1.0),
        ("secondary fill", c("#f7fabf"), 1.0),
        ("minor fill", c("#ffffff"), 1.0),
        ("road fill", c("#dddddd"), 1.0),
        ("living street fill", c("#ededed"), 1.0),
        ("pedestrian fill", c("#dddde8"), 1.0),
        ("track", c("#996600"), 0.8),
        ("bridleway", c("#008000"), 1.0),
        ("cycleway", c("#0000ff"), 1.0),
        ("footway", c("#fa8072"), 1.0),
        ("path halo", c("#ffffff"), 0.4),
        // #highway-area-fill.
        ("pedestrian area fill", c("#dddde8"), 1.0),
        // #roads-casing.
        ("motorway casing", c("#dc2a67"), 1.0),
        ("trunk casing", c("#c84e2f"), 1.0),
        ("primary casing", c("#a06b00"), 1.0),
        ("secondary casing", c("#707d05"), 1.0),
        ("tertiary casing", c("#8f8f8f"), 1.0),
        ("minor casing", c("#bbbbbb"), 1.0),
        ("pedestrian casing", c("#999999"), 1.0),
        // #highway-area-casing.
        ("pedestrian area casing", c("#808080"), 1.0),
        // #barriers.
        ("barrier", c("#444444"), 1.0),
        ("hedge", c(forest), 1.0),
        ("city wall", adjust("#444444", 0.30, 0.0), 1.0),
        // #landuse-overlay.
        ("military line", c("#ff5555"), 0.24),
        ("military hatch", c("#ff5555"), 0.12),
        ("military", c("#ff5555"), 0.08),
        // #tunnels.
        ("motorway tunnel fill", adjust("#e892a2", 0.10, 0.0), 1.0),
        ("trunk tunnel fill", adjust("#f9b29c", 0.10, 0.0), 1.0),
        ("primary tunnel fill", adjust("#fcd6a4", 0.10, 0.0), 1.0),
        ("secondary tunnel fill", adjust("#f7fabf", 0.05, 0.0), 1.0),
        ("minor tunnel fill", adjust("#ffffff", -0.05, 0.0), 1.0),
        ("tunnel casing", c("#808080"), 1.0),
        ("rail tunnel", c("#909090"), 1.0),
        // #buildings.
        (
            "building major line",
            adjust(&building_major, -0.15, 0.0),
            1.0,
        ),
        ("building line", adjust(building, -0.15, 0.0), 1.0),
        ("building major", building_major.clone(), 1.0),
        ("building", c(building), 1.0),
        // #bridge.
        ("bridge", c("#b8b8b8"), 1.0),
        // #water-areas and #water-lines.
        ("water", c("#aad3df"), 1.0),
        // #landcover: each outline directly above its own fill, the types
        // that usually lie inside others above those that surround them.
        ("platform", c("#bbbbbb"), 1.0),
        (
            "pitch line",
            adjust(&adjust(pitch, -0.20, 0.0), 0.0, -0.10),
            1.0,
        ),
        ("pitch", c(pitch), 1.0),
        ("leisure line", adjust(&leisure, -0.60, 0.0), 1.0),
        ("leisure", leisure.clone(), 1.0),
        (
            "parking line",
            adjust(&adjust(parking, -0.40, 0.0), 0.0, 0.20),
            1.0,
        ),
        ("parking", c(parking), 1.0),
        (
            "place of worship line",
            adjust(place_of_worship, -0.30, 0.0),
            1.0,
        ),
        ("place of worship", c(place_of_worship), 1.0),
        ("grass", c("#cdebb0"), 1.0),
        ("park", c(park), 1.0),
        ("cemetery", c("#aacbaf"), 1.0),
        ("allotments", c("#c9e1bf"), 1.0),
        ("orchard", c("#aedfa3"), 1.0),
        ("scrub", c("#c8d7ab"), 1.0),
        ("forest", c(forest), 1.0),
        ("heath", c("#d6d99f"), 1.0),
        ("sand", c("#f5e9c6"), 1.0),
        ("societal amenities", c("#ffffe5"), 1.0),
        ("retail line", c("#d99c95"), 1.0),
        ("retail", c("#ffd6d1"), 1.0),
        ("commercial line", c("#d1b2b0"), 1.0),
        ("commercial", c("#f2dad9"), 1.0),
        ("garages", c("#dfddce"), 1.0),
        ("construction", c("#c7c7b4"), 1.0),
        ("industrial line", c("#c6b3c3"), 1.0),
        ("industrial", c("#ebdbe8"), 1.0),
        ("residential line", c("#b9b9b9"), 1.0),
        ("residential", c("#e0dfdf"), 1.0),
        ("farmyard line", c("#d1b48c"), 1.0),
        ("farmyard", c("#f5dcba"), 1.0),
        ("farmland line", c("#c7c9ae"), 1.0),
        ("farmland", c("#eef0d5"), 1.0),
        // Map { background-color: @land-color; }
        ("land", c("#f2efe9"), 1.0),
    ]
}

/// One road class at zoom 16: carto's total width (casing included) and
/// casing width, both in pixels, and its colours.
struct Road {
    key: &'static str,
    width: f64,
    casing_width: f64,
    casing: &'static str,
    fill: &'static str,
    tunnel_fill: &'static str,
}

/// Carto's road widths at zoom 16 (`roads.mss`: `@<class>-width-z16`, or
/// the nearest lower zoom which defines one, and `@<kind>-casing-width-z16`).
fn roads() -> Vec<Road> {
    let road = |key, width, casing_width, casing, fill, tunnel_fill| Road {
        key,
        width,
        casing_width,
        casing,
        fill,
        tunnel_fill,
    };
    vec![
        road(
            "motorway",
            10.0,
            0.7,
            "motorway casing",
            "motorway fill",
            "motorway tunnel fill",
        ),
        road(
            "motorway_link",
            7.8,
            0.7,
            "motorway casing",
            "motorway fill",
            "motorway tunnel fill",
        ),
        road(
            "trunk",
            10.0,
            0.7,
            "trunk casing",
            "trunk fill",
            "trunk tunnel fill",
        ),
        road(
            "trunk_link",
            7.8,
            0.7,
            "trunk casing",
            "trunk fill",
            "trunk tunnel fill",
        ),
        road(
            "primary",
            10.0,
            0.7,
            "primary casing",
            "primary fill",
            "primary tunnel fill",
        ),
        road(
            "primary_link",
            7.8,
            0.7,
            "primary casing",
            "primary fill",
            "primary tunnel fill",
        ),
        road(
            "secondary",
            10.0,
            0.7,
            "secondary casing",
            "secondary fill",
            "secondary tunnel fill",
        ),
        road(
            "secondary_link",
            7.0,
            0.7,
            "secondary casing",
            "secondary fill",
            "secondary tunnel fill",
        ),
        road(
            "tertiary",
            10.0,
            0.6,
            "tertiary casing",
            "minor fill",
            "minor tunnel fill",
        ),
        road(
            "tertiary_link",
            7.0,
            0.6,
            "tertiary casing",
            "minor fill",
            "minor tunnel fill",
        ),
        road(
            "residential",
            6.0,
            0.6,
            "minor casing",
            "minor fill",
            "minor tunnel fill",
        ),
        road(
            "unclassified",
            6.0,
            0.6,
            "minor casing",
            "minor fill",
            "minor tunnel fill",
        ),
        road(
            "living_street",
            6.0,
            0.6,
            "minor casing",
            "living street fill",
            "minor tunnel fill",
        ),
        road(
            "pedestrian",
            6.0,
            0.6,
            "pedestrian casing",
            "pedestrian fill",
            "minor tunnel fill",
        ),
        road(
            "road",
            3.5,
            0.6,
            "minor casing",
            "road fill",
            "minor tunnel fill",
        ),
        road(
            "service",
            3.5,
            0.6,
            "minor casing",
            "minor fill",
            "minor tunnel fill",
        ),
        road(
            "minor_service",
            2.0,
            0.6,
            "minor casing",
            "minor fill",
            "minor tunnel fill",
        ),
    ]
}

/// How a footway, cycleway or path is paved, as carto's `int_surface`
/// groups the `surface` tag.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    Paved,
    Unpaved,
    Unknown,
}

fn surface(tags: &Tags) -> Surface {
    const PAVED: &[&str] = &[
        "paved",
        "asphalt",
        "cobblestone",
        "cobblestone:flattened",
        "sett",
        "concrete",
        "concrete:lanes",
        "concrete:plates",
        "paving_stones",
        "metal",
        "wood",
        "unhewn_cobblestone",
    ];
    const UNPAVED: &[&str] = &[
        "unpaved",
        "compacted",
        "dirt",
        "earth",
        "fine_gravel",
        "grass",
        "grass_paver",
        "gravel",
        "ground",
        "mud",
        "pebblestone",
        "salt",
        "sand",
        "woodchips",
        "clay",
        "ice",
        "snow",
        "rock",
    ];
    match tags.get("surface") {
        Some(s) if PAVED.contains(&s) => Surface::Paved,
        Some(s) if UNPAVED.contains(&s) => Surface::Unpaved,
        _ => Surface::Unknown,
    }
}

/// The symbol set and the rules for using it.
pub struct Style {
    /// The colour table, in drawing order (index 0 on top).
    pub colors: Vec<Color>,
    /// Every symbol, with the id objects refer to it by.
    pub symbols: Vec<(i32, Symbol)>,
    ids: HashMap<String, i32>,
}

/// Builds [`Style`]: the colour keys resolved to priorities, and each symbol
/// given the next id.
struct Builder {
    color_index: HashMap<&'static str, i32>,
    style: Style,
}

impl Builder {
    fn color(&self, key: &str) -> i32 {
        *self
            .color_index
            .get(key)
            .unwrap_or_else(|| panic!("no colour {key:?} in the palette"))
    }

    fn add(&mut self, key: &str, symbol: Symbol) {
        let id = self.style.symbols.len() as i32;
        let previous = self.style.ids.insert(key.to_string(), id);
        assert!(previous.is_none(), "symbol {key:?} defined twice");
        self.style.symbols.push((id, symbol));
    }

    /// A solid stroke `width` carto pixels wide.
    fn stroke(&self, color: &str, width: f64, round: bool) -> LineSymbol {
        LineSymbol {
            color: self.color(color),
            line_width: px(width),
            cap_style: if round {
                cap_style::ROUND
            } else {
                cap_style::FLAT
            },
            join_style: if round {
                join_style::ROUND
            } else {
                join_style::MITER
            },
            ..LineSymbol::default()
        }
    }

    /// A dashed stroke, carto's two-number `line-dasharray`.
    fn dashed(&self, color: &str, width: f64, round: bool, dash: f64, gap: f64) -> LineSymbol {
        LineSymbol {
            dashed: true,
            dash_length: px(dash),
            break_length: px(gap),
            ..self.stroke(color, width, round)
        }
    }

    fn fill(&self, color: &str) -> AreaSymbol {
        AreaSymbol {
            color: self.color(color),
            ..AreaSymbol::new()
        }
    }

    fn combined(name: &str, parts: Vec<Symbol>) -> Symbol {
        Symbol::Combined(CombinedSymbol {
            name: name.to_string(),
            part_ids: vec![-1; parts.len()],
            owned_parts: parts,
            ..CombinedSymbol::default()
        })
    }

    /// An area fill, with a `width`-pixel outline in `outline` where given.
    fn area(&mut self, key: &str, color: &str, outline: Option<(&str, f64)>) {
        let mut fill = self.fill(color);
        fill.name = key.to_string();
        let symbol = match outline {
            None => Symbol::Area(fill),
            Some((line_color, width)) => {
                let line = self.stroke(line_color, width, true);
                Builder::combined(key, vec![Symbol::Area(fill), Symbol::Line(line)])
            }
        };
        self.add(key, symbol);
    }

    fn line(&mut self, key: &str, mut parts: Vec<LineSymbol>) {
        let symbol = if parts.len() == 1 {
            let mut line = parts.remove(0);
            line.name = key.to_string();
            Symbol::Line(line)
        } else {
            Builder::combined(key, parts.into_iter().map(Symbol::Line).collect())
        };
        self.add(key, symbol);
    }
}

/// Carto's look at zoom 16, as a symbol set.
pub fn carto_style() -> Style {
    let colors: Vec<Color> = palette()
        .into_iter()
        .map(|(key, hex, opacity)| {
            let (r, g, b) = rgb(&hex);
            let rgb = (r as f32, g as f32, b as f32);
            Color {
                name: key.to_string(),
                rgb,
                opacity,
            }
        })
        .collect();
    let mut b = Builder {
        color_index: palette()
            .iter()
            .enumerate()
            .map(|(i, (key, _, _))| (*key, i as i32))
            .collect(),
        style: Style {
            colors,
            symbols: Vec::new(),
            ids: HashMap::new(),
        },
    };

    // Landcover (landcover.mss). Outlines at zoom 16 where carto has them.
    b.area("land", "land", None);
    b.area(
        "residential",
        "residential",
        Some(("residential line", 0.5)),
    );
    b.area("retail", "retail", Some(("retail line", 0.5)));
    b.area("commercial", "commercial", Some(("commercial line", 0.5)));
    b.area("industrial", "industrial", Some(("industrial line", 0.5)));
    b.area("farmland", "farmland", Some(("farmland line", 0.5)));
    b.area("farmyard", "farmyard", Some(("farmyard line", 0.5)));
    for key in [
        "garages",
        "construction",
        "grass",
        "park",
        "cemetery",
        "allotments",
        "orchard",
        "scrub",
        "forest",
        "heath",
        "sand",
        "societal amenities",
        "platform",
        "bridge",
        "water",
    ] {
        b.area(key, key, None);
    }
    b.area("leisure", "leisure", None);
    b.area("playground", "leisure", Some(("leisure line", 0.3)));
    b.area("pitch", "pitch", Some(("pitch line", 0.5)));
    b.area("parking", "parking", Some(("parking line", 0.3)));
    b.area(
        "place of worship",
        "place of worship",
        Some(("place of worship line", 0.3)),
    );
    b.area(
        "pedestrian area",
        "pedestrian area fill",
        Some(("pedestrian area casing", 1.0)),
    );

    // Military land (#landuse-overlay): a faint red tint, carto's
    // military_red_hatch.svg — 45° lines a pixel wide, 8 pixels apart
    // horizontally — and a 2 pixel outline.
    let mut military = b.fill("military");
    military.name = "military".to_string();
    military.patterns.push(FillPattern {
        pattern_type: fill_pattern_type::LINE,
        angle: std::f64::consts::FRAC_PI_4,
        line_spacing: px(8.0 / std::f64::consts::SQRT_2),
        line_color: b.color("military hatch"),
        line_width: px(std::f64::consts::FRAC_1_SQRT_2),
        ..FillPattern::default()
    });
    let outline = b.stroke("military line", 2.0, true);
    b.add(
        "military",
        Builder::combined(
            "military",
            vec![Symbol::Area(military), Symbol::Line(outline)],
        ),
    );

    // Buildings (buildings.mss).
    b.area("building", "building", Some(("building line", 0.75)));
    b.area(
        "building major",
        "building major",
        Some(("building major line", 0.75)),
    );

    // Roads (roads.mss): the casing the full width, the fill inset by the
    // casing width on either side. A tunnel's casing is dashed, its fill
    // lighter.
    for road in roads() {
        let casing = b.stroke(road.casing, road.width, true);
        let fill = b.stroke(road.fill, road.width - 2.0 * road.casing_width, true);
        b.line(&format!("highway {}", road.key), vec![casing, fill]);

        let casing = b.dashed("tunnel casing", road.width, false, 4.0, 2.0);
        let fill = b.stroke(road.tunnel_fill, road.width - 2.0 * road.casing_width, true);
        b.line(&format!("highway {} tunnel", road.key), vec![casing, fill]);
    }

    // Paths: a faint white halo two pixels wider, then the dashed line.
    let footway = |b: &Builder, dash, gap| {
        vec![
            b.stroke("path halo", 1.3 + 2.0, true),
            b.dashed("footway", 1.3, true, dash, gap),
        ]
    };
    // Carto's unknown-surface pattern is 1,4,2,3: one dash and gap each
    // side of 1.5 and 3.5 averages it.
    let parts = footway(&b, 3.0, 3.5);
    b.line("footway paved", parts);
    let parts = footway(&b, 1.0, 4.0);
    b.line("footway unpaved", parts);
    let parts = footway(&b, 1.5, 3.5);
    b.line("footway", parts);
    let cycleway = |b: &Builder, dash, gap| {
        vec![
            b.stroke("path halo", 0.9 + 2.0, true),
            b.dashed("cycleway", 0.9, true, dash, gap),
        ]
    };
    let parts = cycleway(&b, 3.0, 3.5);
    b.line("cycleway paved", parts);
    let parts = cycleway(&b, 1.0, 4.0);
    b.line("cycleway unpaved", parts);
    let parts = cycleway(&b, 1.5, 3.5);
    b.line("cycleway", parts);
    let parts = vec![
        b.stroke("path halo", 1.2 + 2.0, true),
        b.dashed("bridleway", 1.2, false, 4.0, 2.0),
    ];
    b.line("bridleway", parts);
    let parts = vec![
        b.stroke("path halo", 3.0 + 2.0, true),
        b.dashed("footway", 3.0, false, 2.0, 1.0),
    ];
    b.line("steps", parts);
    for (key, dash) in [
        ("track", Some((5.0, 4.0))),
        ("track grade1", None),
        ("track grade2", Some((11.0, 4.0))),
        ("track grade3", Some((7.0, 5.0))),
        ("track grade4", Some((4.0, 6.0))),
        ("track grade5", Some((2.0, 8.0))),
    ] {
        let width = if key == "track grade1" || key == "track grade2" {
            0.75
        } else {
            1.5
        };
        let line = match dash {
            Some((dash, gap)) => b.dashed("track", width, true, dash, gap),
            None => b.stroke("track", width, true),
        };
        let parts = vec![b.stroke("path halo", width + 2.0, true), line];
        b.line(key, parts);
    }

    // Railways: a grey line with a white dash down the middle.
    let parts = vec![
        b.stroke("rail", 3.0, false),
        b.dashed("rail light", 1.0, false, 8.0, 9.0),
    ];
    b.line("rail", parts);
    let parts = vec![
        b.stroke("rail service", 2.0, false),
        b.dashed("rail light", 0.8, false, 8.0, 9.0),
    ];
    b.line("rail service", parts);
    let parts = vec![b.dashed("rail tunnel", 3.0, false, 5.0, 2.0)];
    b.line("rail tunnel", parts);
    let parts = vec![b.stroke("light rail", 2.0, false)];
    b.line("light rail", parts);
    let parts = vec![b.stroke("tram", 1.5, false)];
    b.line("tram", parts);
    let parts = vec![b.dashed("tram", 1.5, false, 5.0, 3.0)];
    b.line("tram tunnel", parts);
    let parts = vec![b.stroke("tram", 0.5, false)];
    b.line("tram service", parts);

    // Waterways (water.mss).
    // A canal is a stream 1.4 times as wide; a ditch is a stream's zoom-14
    // width until zoom 17.
    for (key, width) in [
        ("river", 8.0),
        ("canal", 3.0 * 1.4),
        ("stream", 3.0),
        ("ditch", 2.0),
    ] {
        let parts = vec![b.stroke("water", width, true)];
        b.line(key, parts);
    }

    // Barriers (landcover.mss, #barriers).
    let parts = vec![b.stroke("barrier", 0.4, false)];
    b.line("barrier", parts);
    let parts = vec![b.stroke("hedge", 1.5, false)];
    b.line("hedge", parts);
    let parts = vec![b.stroke("city wall", 1.0, false)];
    b.line("city wall", parts);

    // Trees (amenity-points.mss, #trees): a 2.5 px dot, a 2.5 px line.
    let parts = vec![b.stroke("tree", 2.5, true)];
    b.line("tree row", parts);
    let tree = PointSymbol {
        name: "tree".to_string(),
        inner_radius: px(2.5) / 2.0,
        inner_color: b.color("tree"),
        ..PointSymbol::new()
    };
    b.add("tree", Symbol::Point(tree));

    for (id, symbol) in &mut b.style.symbols {
        set_code(symbol, &(*id + 1).to_string());
    }
    b.style
}

fn set_code(symbol: &mut Symbol, code: &str) {
    let slot = match symbol {
        Symbol::Point(s) => &mut s.code,
        Symbol::Line(s) => &mut s.code,
        Symbol::Area(s) => &mut s.code,
        Symbol::Text(s) => &mut s.code,
        Symbol::Combined(s) => &mut s.code,
    };
    *slot = code.to_string();
}

impl Style {
    /// The id of the symbol called `key`.
    ///
    /// Panics where there is none: every key is one of this module's own.
    pub fn id(&self, key: &str) -> i32 {
        *self
            .ids
            .get(key)
            .unwrap_or_else(|| panic!("no symbol {key:?} in the style"))
    }

    /// The symbol the whole map is laid on: carto's land colour.
    pub fn background(&self) -> i32 {
        self.id("land")
    }

    /// The area symbols a closed way or a multipolygon with these tags is
    /// drawn with, bottom first. Empty for anything not drawn as an area.
    ///
    /// `multipolygon` says which of the two it is: a closed `highway` way is
    /// a loop of road unless tagged `area=yes`, a multipolygon is an area
    /// regardless — a square like Piazza Maggiore is one.
    pub fn area_symbols(&self, tags: &Tags, multipolygon: bool) -> Vec<i32> {
        let mut keys: Vec<&str> = Vec::new();
        if let Some(v) = tags.get("landuse") {
            keys.push(match v {
                "residential" => "residential",
                "retail" => "retail",
                "commercial" => "commercial",
                "industrial" | "railway" => "industrial",
                "garages" => "garages",
                "construction" | "brownfield" => "construction",
                "farmland" | "greenhouse_horticulture" => "farmland",
                "farmyard" => "farmyard",
                "meadow" | "grass" | "village_green" | "flowerbed" => "grass",
                "allotments" => "allotments",
                "orchard" | "vineyard" | "plant_nursery" => "orchard",
                "forest" => "forest",
                "cemetery" => "cemetery",
                "religious" => "place of worship",
                "recreation_ground" => "leisure",
                "reservoir" | "basin" => "water",
                "military" => "military",
                _ => "",
            });
        }
        if let Some(v) = tags.get("natural") {
            keys.push(match v {
                "wood" => "forest",
                "scrub" => "scrub",
                "grassland" => "grass",
                "heath" => "heath",
                "sand" | "beach" => "sand",
                "water" => "water",
                _ => "",
            });
        }
        if tags.is("waterway", "riverbank") {
            keys.push("water");
        }
        if let Some(v) = tags.get("leisure") {
            keys.push(match v {
                "park" => "park",
                "garden" => "grass",
                "playground" | "fitness_station" => "playground",
                "pitch" | "track" => "pitch",
                "sports_centre" | "stadium" | "dog_park" | "recreation_ground" => "leisure",
                "swimming_pool" => "water",
                _ => "",
            });
        }
        if let Some(v) = tags.get("amenity") {
            keys.push(match v {
                "parking" => "parking",
                "school" | "university" | "college" | "kindergarten" | "hospital" => {
                    "societal amenities"
                }
                "place_of_worship" if !tags.get("building").is_some_and(|b| b != "no") => {
                    "place of worship"
                }
                "grave_yard" => "cemetery",
                _ => "",
            });
        }
        if tags.is("man_made", "bridge") {
            keys.push("bridge");
        }
        if matches!(tags.get("highway"), Some("pedestrian" | "footway"))
            && (multipolygon || tags.is("area", "yes"))
        {
            keys.push("pedestrian area");
        }
        if tags.is("railway", "platform")
            || tags.is("highway", "platform")
            || (tags.is("public_transport", "platform") && tags.is("area", "yes"))
        {
            keys.push("platform");
        }
        if tags.get("building").is_some_and(|v| v != "no") {
            let major = tags.is("amenity", "place_of_worship")
                || tags.is("building", "train_station")
                || tags.is("public_transport", "station")
                || tags.is("aeroway", "terminal");
            keys.push(if major { "building major" } else { "building" });
        }
        keys.into_iter()
            .filter(|k| !k.is_empty())
            .map(|k| self.id(k))
            .collect()
    }

    /// The line symbols a way with these tags is drawn with. Empty for
    /// anything not drawn as a line, which includes a way tagged as an area.
    pub fn line_symbols(&self, tags: &Tags) -> Vec<i32> {
        let mut out = Vec::new();
        let tunnel = tags.get("tunnel").is_some_and(|v| v != "no");

        if let Some(highway) = tags.get("highway") {
            if !tags.is("area", "yes") {
                if let Some(key) = self.highway_key(highway, tags, tunnel) {
                    out.push(self.id(&key));
                }
            }
        }

        if let Some(railway) = tags.get("railway") {
            let key = match railway {
                "rail" | "narrow_gauge" | "preserved" if tunnel => Some("rail tunnel"),
                "rail" if tags.get("service").is_some() => Some("rail service"),
                "rail" | "preserved" => Some("rail"),
                "narrow_gauge" | "funicular" => Some("rail service"),
                "light_rail" if !tunnel => Some("light rail"),
                "tram" if tags.get("service").is_some() => Some("tram service"),
                "tram" if tunnel => Some("tram tunnel"),
                "tram" => Some("tram"),
                _ => None,
            };
            if let Some(key) = key {
                out.push(self.id(key));
            }
        }

        if let Some(waterway) = tags.get("waterway") {
            let key = match waterway {
                _ if tunnel => None,
                "river" => Some("river"),
                "canal" => Some("canal"),
                "stream" => Some("stream"),
                "ditch" | "drain" => Some("ditch"),
                _ => None,
            };
            if let Some(key) = key {
                out.push(self.id(key));
            }
        }

        if let Some(barrier) = tags.get("barrier") {
            let key = match barrier {
                "hedge" => Some("hedge"),
                "city_wall" => Some("city wall"),
                "wall" | "fence" | "retaining_wall" | "guard_rail" | "handrail" | "chain"
                | "railing" => Some("barrier"),
                _ => None,
            };
            if let Some(key) = key {
                out.push(self.id(key));
            }
        }
        if tags.is("historic", "citywalls") && tags.get("barrier").is_none() {
            out.push(self.id("city wall"));
        }

        if tags.is("natural", "tree_row") {
            out.push(self.id("tree row"));
        }
        out
    }

    /// The symbol for a road or path, as carto groups `highway` values.
    fn highway_key(&self, highway: &str, tags: &Tags, tunnel: bool) -> Option<String> {
        let by_surface = |base: &str| match surface(tags) {
            Surface::Paved => format!("{base} paved"),
            Surface::Unpaved => format!("{base} unpaved"),
            Surface::Unknown => base.to_string(),
        };
        let road = |class: &str| {
            Some(if tunnel {
                format!("highway {class} tunnel")
            } else {
                format!("highway {class}")
            })
        };
        match highway {
            "motorway" | "motorway_link" | "trunk" | "trunk_link" | "primary" | "primary_link"
            | "secondary" | "secondary_link" | "tertiary" | "tertiary_link" | "residential"
            | "unclassified" | "living_street" | "pedestrian" | "road" => road(highway),
            "service" => {
                let minor = matches!(
                    tags.get("service"),
                    Some("parking_aisle" | "driveway" | "drive-through")
                );
                road(if minor { "minor_service" } else { "service" })
            }
            "footway" => Some(by_surface("footway")),
            "path" if tags.is("bicycle", "designated") => Some(by_surface("cycleway")),
            "path" if tags.is("horse", "designated") => Some("bridleway".to_string()),
            "path" => Some(by_surface("footway")),
            "cycleway" => Some(by_surface("cycleway")),
            "bridleway" => Some("bridleway".to_string()),
            "steps" => Some("steps".to_string()),
            "track" => Some(match tags.get("tracktype") {
                Some(grade @ ("grade1" | "grade2" | "grade3" | "grade4" | "grade5")) => {
                    format!("track {grade}")
                }
                _ => "track".to_string(),
            }),
            _ => None,
        }
    }

    /// The point symbols a tagged node is drawn with.
    pub fn point_symbols(&self, tags: &Tags) -> Vec<i32> {
        if tags.is("natural", "tree") {
            vec![self.id("tree")]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        Tags(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn hsl_adjustments_match_what_less_computes() {
        // Values carto itself documents next to its variables.
        assert_eq!(adjust("#d9d0c9", -0.10, 0.0), "#c4b6ab"); // @building-major-fill
        assert_eq!(adjust("#ffffff", -0.05, 0.0), "#f2f2f2"); // @residential-tunnel-fill
        assert_eq!(adjust("#444444", 0.30, 0.0), "#919191");
        // A grey has no saturation to take away.
        assert_eq!(adjust("#808080", 0.0, -0.5), "#808080");
    }

    #[test]
    fn every_casing_is_drawn_below_every_road_fill() {
        let style = carto_style();
        let priority = |name: &str| style.colors.iter().position(|c| c.name == name).unwrap();
        let lowest_fill = ["motorway fill", "minor fill", "pedestrian fill"]
            .iter()
            .map(|n| priority(n))
            .max()
            .unwrap();
        let highest_casing = ["motorway casing", "minor casing", "pedestrian casing"]
            .iter()
            .map(|n| priority(n))
            .min()
            .unwrap();
        assert!(lowest_fill < highest_casing);
        // And the land is the bottom of everything.
        assert_eq!(priority("land"), style.colors.len() - 1);
    }

    #[test]
    fn a_road_fill_is_inset_by_its_casing_on_both_sides() {
        let style = carto_style();
        let (_, Symbol::Combined(residential)) =
            &style.symbols[style.id("highway residential") as usize]
        else {
            panic!("a road is a combined symbol");
        };
        let widths: Vec<f64> = residential
            .owned_parts
            .iter()
            .map(|p| match p {
                Symbol::Line(l) => l.line_width,
                _ => panic!("a road's parts are lines"),
            })
            .collect();
        assert!((widths[0] - 6.0 * PIXEL_MM).abs() < 1e-9);
        assert!((widths[1] - (6.0 - 1.2) * PIXEL_MM).abs() < 1e-9);
    }

    #[test]
    fn tags_pick_symbols_the_way_carto_groups_them() {
        let style = carto_style();
        let line = |pairs: &[(&str, &str)]| style.line_symbols(&tags(pairs));
        let area = |pairs: &[(&str, &str)]| style.area_symbols(&tags(pairs), false);

        assert_eq!(
            line(&[("highway", "residential")]),
            vec![style.id("highway residential")]
        );
        assert_eq!(
            line(&[("highway", "service"), ("service", "parking_aisle")]),
            vec![style.id("highway minor_service")]
        );
        assert_eq!(
            line(&[("highway", "primary"), ("tunnel", "yes")]),
            vec![style.id("highway primary tunnel")]
        );
        assert_eq!(
            line(&[("highway", "footway"), ("surface", "asphalt")]),
            vec![style.id("footway paved")]
        );
        assert_eq!(
            line(&[("highway", "path"), ("bicycle", "designated")]),
            vec![style.id("cycleway")]
        );
        // An area-tagged highway is an area, not a line.
        assert!(line(&[("highway", "pedestrian"), ("area", "yes")]).is_empty());
        assert_eq!(
            area(&[("highway", "pedestrian"), ("area", "yes")]),
            vec![style.id("pedestrian area")]
        );
        // A culverted stream is not drawn at all.
        assert!(line(&[("waterway", "stream"), ("tunnel", "culvert")]).is_empty());

        assert_eq!(area(&[("building", "yes")]), vec![style.id("building")]);
        assert_eq!(
            area(&[("building", "church"), ("amenity", "place_of_worship")]),
            vec![style.id("building major")]
        );
        assert!(area(&[("building", "no")]).is_empty());
        // A school building is both its grounds and a building, grounds first.
        assert_eq!(
            area(&[("amenity", "school"), ("building", "yes")]),
            vec![style.id("societal amenities"), style.id("building")]
        );
        assert!(area(&[("highway", "residential")]).is_empty());
        assert!(area(&[("highway", "pedestrian")]).is_empty());
        assert_eq!(
            style.area_symbols(&tags(&[("highway", "pedestrian")]), true),
            vec![style.id("pedestrian area")]
        );
    }
}
