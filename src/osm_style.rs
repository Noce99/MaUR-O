//! What OpenStreetMap features look like: a symbol set reproducing
//! [openstreetmap-carto](https://github.com/gravitystorm/openstreetmap-carto),
//! the "Standard" style of openstreetmap.org, at one of the zooms 13 to 17,
//! and the tag rules deciding which feature gets which symbol.
//!
//! # From pixels to millimetres
//!
//! Carto is written in pixels, for one zoom at a time, and a tile pixel is
//! taken to be [`PIXEL_MM`] = 0.28 mm — the "standardized rendering pixel"
//! of the OGC, which Mapnik's own scale denominators assume too. So a
//! symbol here is carto's width in pixels times 0.28.
//!
//! # Levels of detail
//!
//! [`Details`] picks the zoom: what carto shows there, how wide it draws it,
//! and the scale the map is drawn at, a round one near the zoom's own (see
//! [`Details::scale`]). Carto writes a width as a cascade of
//! `[zoom >= n] { line-width: … }` overrides, and so do the tables below,
//! with [`at`]; a feature carto starts drawing at some zoom is left out
//! below it ([`Style::area_symbols`] and the other rules).
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

use crate::bundle::WIDTH_FACTORS;
use crate::bus::Mode;
use crate::map::{
    cap_style, coord_flag, fill_pattern_type, join_style, AreaSymbol, Color, CombinedSymbol, Coord,
    Element, FillPattern, LineSymbol, Object, ObjectKind, PathObject, PointSymbol, Symbol,
    TextSymbol,
};
use crate::osm::Tags;

/// The size of one carto pixel on the paper, in mm.
pub const PIXEL_MM: f64 = 0.28;

/// How much a map shows, as a level from 0 to [`Details::MAX`]: each
/// level is carto at one zoom, 13 to 17, drawn at a scale of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Details(u8);

impl Details {
    /// The most detailed level.
    pub const MAX: u8 = 4;

    /// Each level's carto zoom and scale denominator, coarsest first.
    const LEVELS: [(u32, i32); 5] = [
        (13, 24_000),
        (14, 17_000),
        (15, 10_000),
        (16, 5_000),
        (17, 2_500),
    ];

    /// The level `level`, or `None` past [`Details::MAX`].
    pub fn new(level: u8) -> Option<Details> {
        (level <= Details::MAX).then_some(Details(level))
    }

    /// The level, from 0 to [`Details::MAX`].
    pub fn level(self) -> u8 {
        self.0
    }

    /// The carto zoom whose look the level reproduces.
    pub fn zoom(self) -> u32 {
        Details::LEVELS[usize::from(self.0)].0
    }

    /// The scale denominator the map is drawn at: 1:24 000, 1:17 000,
    /// 1:10 000, 1:5 000 and 1:2 500. At Bologna's latitude each is the
    /// zoom's own scale (the one at which a tile pixel is [`PIXEL_MM`] on
    /// the paper) or somewhat larger, so that the map is a little less
    /// crowded than the screen.
    pub fn scale(self) -> i32 {
        Details::LEVELS[usize::from(self.0)].1
    }
}

impl Default for Details {
    /// Level 3: zoom 16 at 1:5 000.
    fn default() -> Details {
        Details(3)
    }
}

/// What carto sets at `zoom`, given as its cascade of `[zoom >= n]`
/// overrides, `(n, value)` from the lowest zoom up: the last one at or below
/// `zoom` holds, the first one below its own zoom.
pub fn at(zoom: u32, steps: &[(u32, f64)]) -> f64 {
    steps
        .iter()
        .rev()
        .find(|(from, _)| *from <= zoom)
        .unwrap_or(&steps[0])
        .1
}

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
    hsl(h, s, l)
}

/// `#rrggbb` of a hue in degrees, a saturation and a lightness in `[0, 1]`.
fn hsl(h: f64, s: f64, l: f64) -> String {
    let h = h.rem_euclid(360.0);
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
/// colour's name in the map file. A few colours are darker at low `zoom`.
fn palette(zoom: u32) -> Vec<(&'static str, String, f64)> {
    let c = |hex: &str| hex.to_string();
    let forest = "#add19e";
    let park = "#c8facc";
    let leisure = adjust(park, 0.05, 0.0);
    let pitch = "#88e0be";
    let parking = "#eeeeee";
    let place_of_worship = "#d0d0d0";
    let building = "#d9d0c9";
    let building_major = adjust(building, -0.10, 0.0);
    // Buildings are filled darker, and unoutlined, at zoom 14.
    let (building_fill, building_major_fill) = match zoom {
        ..=14 => (
            adjust(building, -0.04, 0.0),
            adjust(&building_major, -0.10, 0.0),
        ),
        15 => (c(building), adjust(&building_major, -0.05, 0.0)),
        _ => (c(building), building_major.clone()),
    };
    vec![
        // The legend's own text, which lies outside the map.
        ("legend text", c("#000000"), 1.0),
        // #trees, drawn over the roads.
        ("tree", adjust(forest, -0.10, 0.0), 0.6),
        // #roads-fill: railways, then road fills by importance, then paths.
        ("rail light", c("#ffffff"), 1.0),
        ("rail", c("#707070"), 1.0),
        ("rail service", c("#aaaaaa"), 1.0),
        ("light rail", c("#666666"), 1.0),
        ("tram", c("#6e6e6e"), 1.0),
        ("monorail", c("#777777"), 1.0),
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
        ("building major", building_major_fill, 1.0),
        ("building", building_fill, 1.0),
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

/// One road class: carto's total width (casing included) as its cascade
/// of zooms, which casing width takes off either side for the fill, its
/// colours, and the zoom it is first drawn at.
struct Road {
    key: &'static str,
    from: u32,
    width: &'static [(u32, f64)],
    casing_width: Casing,
    casing: &'static str,
    fill: &'static str,
    tunnel_fill: &'static str,
}

/// Which of carto's casing widths a road's fill is inset by.
#[derive(Clone, Copy)]
enum Casing {
    /// `@major-casing-width-z*`: motorways, trunk and primary roads.
    Major,
    /// `@secondary-casing-width-z*`: secondary roads.
    Secondary,
    /// `@casing-width-z*`: everything else, links included.
    Minor,
}

impl Casing {
    fn width(self, zoom: u32) -> f64 {
        at(
            zoom,
            match self {
                Casing::Major => &[(13, 0.5), (14, 0.6), (15, 0.7), (17, 1.0)],
                Casing::Secondary => &[(13, 0.35), (15, 0.7), (17, 1.0)],
                Casing::Minor => &[(13, 0.5), (14, 0.55), (15, 0.6), (17, 0.8)],
            },
        )
    }
}

/// Carto's roads (`roads.mss`: `@<class>-width-z*`, and the zoom its
/// `#roads-casing` rule starts at).
fn roads() -> Vec<Road> {
    const MAJOR: &[(u32, f64)] = &[(13, 6.0), (15, 10.0), (17, 18.0)];
    const MAJOR_LINK: &[(u32, f64)] = &[(13, 4.0), (15, 7.8), (17, 12.0)];
    const PRIMARY: &[(u32, f64)] = &[(13, 5.0), (15, 10.0), (17, 18.0)];
    const SECONDARY: &[(u32, f64)] = &[(13, 5.0), (15, 9.0), (16, 10.0), (17, 18.0)];
    const SECONDARY_LINK: &[(u32, f64)] = &[(13, 4.0), (15, 7.0), (17, 12.0)];
    const TERTIARY: &[(u32, f64)] = &[(13, 4.0), (14, 5.0), (15, 9.0), (16, 10.0), (17, 18.0)];
    const TERTIARY_LINK: &[(u32, f64)] = &[(13, 3.0), (15, 7.0), (17, 12.0)];
    const RESIDENTIAL: &[(u32, f64)] = &[(13, 2.5), (14, 3.0), (15, 5.0), (16, 6.0), (17, 12.0)];
    const LIVING_STREET: &[(u32, f64)] = &[(13, 2.0), (14, 3.0), (15, 5.0), (16, 6.0), (17, 12.0)];
    const PEDESTRIAN: &[(u32, f64)] = &[(14, 3.0), (15, 5.0), (16, 6.0), (17, 12.0)];
    const SERVICE: &[(u32, f64)] = &[(14, 2.0), (16, 3.5), (17, 7.0)];
    const MINOR_SERVICE: &[(u32, f64)] = &[(16, 2.0), (17, 3.5)];
    let motorway = ("motorway casing", "motorway fill", "motorway tunnel fill");
    let trunk = ("trunk casing", "trunk fill", "trunk tunnel fill");
    let primary = ("primary casing", "primary fill", "primary tunnel fill");
    let secondary = (
        "secondary casing",
        "secondary fill",
        "secondary tunnel fill",
    );
    let tertiary = ("tertiary casing", "minor fill", "minor tunnel fill");
    let minor = ("minor casing", "minor fill", "minor tunnel fill");
    let living = ("minor casing", "living street fill", "minor tunnel fill");
    let pedestrian = ("pedestrian casing", "pedestrian fill", "minor tunnel fill");
    let unknown = ("minor casing", "road fill", "minor tunnel fill");
    [
        ("motorway", 13, MAJOR, Casing::Major, motorway),
        ("motorway_link", 13, MAJOR_LINK, Casing::Minor, motorway),
        ("trunk", 13, MAJOR, Casing::Major, trunk),
        ("trunk_link", 13, MAJOR_LINK, Casing::Minor, trunk),
        ("primary", 13, PRIMARY, Casing::Major, primary),
        ("primary_link", 13, MAJOR_LINK, Casing::Minor, primary),
        ("secondary", 13, SECONDARY, Casing::Secondary, secondary),
        (
            "secondary_link",
            13,
            SECONDARY_LINK,
            Casing::Minor,
            secondary,
        ),
        ("tertiary", 13, TERTIARY, Casing::Minor, tertiary),
        ("tertiary_link", 13, TERTIARY_LINK, Casing::Minor, tertiary),
        ("residential", 13, RESIDENTIAL, Casing::Minor, minor),
        ("unclassified", 13, RESIDENTIAL, Casing::Minor, minor),
        ("living_street", 13, LIVING_STREET, Casing::Minor, living),
        ("pedestrian", 14, PEDESTRIAN, Casing::Minor, pedestrian),
        ("road", 14, SERVICE, Casing::Minor, unknown),
        ("service", 14, SERVICE, Casing::Minor, minor),
        ("minor_service", 16, MINOR_SERVICE, Casing::Minor, minor),
    ]
    .into_iter()
    .map(
        |(key, from, width, casing_width, (casing, fill, tunnel_fill))| Road {
            key,
            from,
            width,
            casing_width,
            casing,
            fill,
            tunnel_fill,
        },
    )
    .collect()
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

/// Whether a path, a track or steps are closed to those they are for, as
/// carto's `carto_highway_int_access` decides: the access tag for the
/// path's own users where there is one, the general one otherwise.
fn closed(highway: &str, tags: &Tags) -> bool {
    let access = |mode: &str| {
        tags.get(mode)
            .filter(|v| *v != "unknown")
            .or(tags.get("access"))
    };
    let value = match highway {
        // A path promoted for its designated users is open to them.
        "path" if tags.is("bicycle", "designated") || tags.is("horse", "designated") => None,
        "footway" | "steps" | "path" => access("foot"),
        "cycleway" => access("bicycle"),
        "bridleway" => access("horse"),
        "track" => tags.get("access"),
        _ => return false,
    };
    matches!(
        value,
        Some("no" | "permit" | "private" | "agricultural" | "forestry" | "agricultural;forestry")
    )
}

/// What a legend entry shows for a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sample {
    /// A small rectangle filled with it.
    Area,
    /// A short straight line drawn with it.
    Line,
    /// One point drawn with it.
    Point,
    /// A piece of text set in it.
    Text,
    /// A short bar across, as a stop is drawn across a bundle.
    Bar,
}

/// The symbol set and the rules for using it.
pub struct Style {
    /// The colour table, in drawing order (index 0 on top).
    pub colors: Vec<Color>,
    /// Every symbol, with the id objects refer to it by.
    pub symbols: Vec<(i32, Symbol)>,
    /// The level of detail the symbols are drawn for.
    pub details: Details,
    ids: HashMap<String, i32>,
    /// The symbols carto only draws from some zoom on, by id: those whose
    /// zoom is above the level's are left out.
    from: HashMap<i32, u32>,
}

/// Builds [`Style`]: the colour keys resolved to priorities, and each symbol
/// given the next id.
struct Builder {
    color_index: HashMap<String, i32>,
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

    /// Carto draws `key` from `zoom` on only.
    fn from(&mut self, key: &str, zoom: u32) {
        let id = self.style.ids[key];
        self.style.from.insert(id, zoom);
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

/// Carto's look at the zoom of `details`, as a symbol set.
pub fn carto_style(details: Details) -> Style {
    carto_style_with_bus_lines(&[], details)
}

/// The colours bus lines are drawn in, one per line in the order they are
/// asked for, starting over after the last. Strong enough to read over
/// every carto colour, and far enough apart to tell neighbours apart.
pub const BUS_COLORS: [&str; 10] = [
    "#e6194b", "#4363d8", "#3cb44b", "#f58231", "#911eb4", "#00a0b0", "#f032e6", "#9a6324",
    "#000075", "#808000",
];

/// The colour of the `index`th line on a map: [`BUS_COLORS`] for the first
/// ten, and then hues a golden angle apart — each as far as it can be from
/// all the ones before it — so that a map of every bus in the city does not
/// give two neighbouring lines the same colour every ten lines.
pub fn line_color(index: usize) -> String {
    match BUS_COLORS.get(index) {
        Some(color) => color.to_string(),
        None => {
            let n = (index - BUS_COLORS.len()) as f64;
            // Alternating a little in lightness tells apart two hues the
            // golden angle has brought round close to each other.
            let lightness = if index.is_multiple_of(2) { 0.40 } else { 0.50 };
            hsl(n * 137.507_764, 0.75, lightness)
        }
    }
}

/// How wide a bus or train line is drawn, in carto pixels, outside a bundle
/// or in one narrow enough: thinner, by the factors of
/// [`WIDTH_FACTORS`], in a bundle which would otherwise be too wide.
pub fn line_width_px(mode: Mode) -> f64 {
    match mode {
        Mode::Bus => 3.0,
        Mode::Train => 4.5,
    }
}

/// A stop circle of a bus or train line: the white dot's radius and the
/// width of the ring around it, in carto pixels.
pub fn stop_circle_px(mode: Mode) -> (f64, f64) {
    match mode {
        Mode::Bus => (2.4, 1.4),
        Mode::Train => (3.2, 1.8),
    }
}

/// The arrow beside a stop: a triangle this long and this wide at its base,
/// in carto pixels.
pub const ARROW_PX: (f64, f64) = (5.0, 4.0);

/// A bus or train line, as far as its look goes: what kind of line, its
/// number, and its colour as `#rrggbb`.
#[derive(Clone, Debug, PartialEq)]
pub struct BusLook {
    /// A bus or a train line.
    pub mode: Mode,
    /// The line number, e.g. `"13"` or `"T2"`.
    pub reference: String,
    /// The line's colour, `#rrggbb`.
    pub color: String,
}

/// Carto's look at the zoom of `details`, with bus and train lines drawn over
/// everything.
///
/// Each line gets three symbols, keyed by its kind and number: `bus <n>` or
/// `train <n>` for the route (a stroke in the line's colour, 3 px for a bus
/// and 4.5 px for a train), `… stop` for a stop (a white dot ringed in the
/// line's colour, larger for a station) and `… label` for the number at
/// either end (white bold text on a blob of the line's colour, like the
/// badge on the vehicle itself). The lines' colours go on top of the table,
/// in the order the lines are given; above them the white of the stops, then
/// every badge, then the labels' white. They are as wide on the paper at
/// every level of detail.
pub fn carto_style_with_bus_lines(lines: &[BusLook], details: Details) -> Style {
    let zoom = details.zoom();
    let mut entries: Vec<(String, String, f64)> = Vec::new();
    if !lines.is_empty() {
        entries.push(("bus label".to_string(), "#ffffff".to_string(), 1.0));
    }
    // A badge in its line's colour, but above every line: a number at a
    // terminus many lines share is not hidden under the others' routes.
    for line in lines {
        entries.push((
            format!("{} {} badge", line.mode.key(), line.reference),
            line.color.clone(),
            1.0,
        ));
    }
    if !lines.is_empty() {
        entries.push(("bus stop".to_string(), "#ffffff".to_string(), 1.0));
        entries.push(("stop bar outline".to_string(), "#333333".to_string(), 1.0));
    }
    for line in lines {
        entries.push((
            format!("{} {}", line.mode.key(), line.reference),
            line.color.clone(),
            1.0,
        ));
    }
    entries.extend(
        palette(zoom)
            .into_iter()
            .map(|(key, hex, opacity)| (key.to_string(), hex, opacity)),
    );

    let colors: Vec<Color> = entries
        .iter()
        .map(|(key, hex, opacity)| {
            let (r, g, b) = rgb(hex);
            Color {
                name: key.clone(),
                rgb: (r as f32, g as f32, b as f32),
                opacity: *opacity,
            }
        })
        .collect();
    let mut b = Builder {
        color_index: entries
            .iter()
            .enumerate()
            .map(|(i, (key, _, _))| (key.clone(), i as i32))
            .collect(),
        style: Style {
            colors,
            symbols: Vec::new(),
            details,
            ids: HashMap::new(),
            from: HashMap::new(),
        },
    };

    // Landcover (landcover.mss). Outlines where carto has them at this zoom:
    // the faint ones of built-up land and farmland from zoom 16, those of
    // smaller grounds from zoom 15.
    let faint = |line| (zoom >= 16).then_some((line, 0.5));
    let thin = |line, width| (zoom >= 15).then_some((line, width));
    b.area("land", "land", None);
    b.area("residential", "residential", faint("residential line"));
    b.area("retail", "retail", faint("retail line"));
    b.area("commercial", "commercial", faint("commercial line"));
    b.area("industrial", "industrial", faint("industrial line"));
    b.area("farmland", "farmland", faint("farmland line"));
    b.area("farmyard", "farmyard", faint("farmyard line"));
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
    b.from("platform", 16);
    b.area("leisure", "leisure", None);
    b.area("playground", "leisure", thin("leisure line", 0.3));
    b.area("pitch", "pitch", thin("pitch line", 0.5));
    b.area("parking", "parking", thin("parking line", 0.3));
    b.from("parking", 14);
    b.area(
        "place of worship",
        "place of worship",
        thin("place of worship line", 0.3),
    );
    b.area(
        "pedestrian area",
        "pedestrian area fill",
        Some(("pedestrian area casing", 1.0)),
    );
    b.from("pedestrian area", 15);

    // Military land (#landuse-overlay): a faint red tint, carto's
    // military_red_hatch.svg — 45° lines a pixel wide, 8 pixels apart
    // horizontally — and an outline, 1 pixel wide below zoom 15 and 2 from it.
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
    let outline = b.stroke("military line", at(zoom, &[(13, 1.0), (15, 2.0)]), true);
    b.add(
        "military",
        Builder::combined(
            "military",
            vec![Symbol::Area(military), Symbol::Line(outline)],
        ),
    );

    // Buildings (buildings.mss), from zoom 14, outlined from zoom 15.
    b.area("building", "building", thin("building line", 0.75));
    b.area(
        "building major",
        "building major",
        thin("building major line", 0.75),
    );
    b.from("building", 14);
    b.from("building major", 14);

    // Roads (roads.mss): the casing the full width, the fill inset by the
    // casing width on either side. A tunnel's casing is dashed, its fill
    // lighter.
    for road in roads() {
        let width = at(zoom, road.width);
        let fill_width = width - 2.0 * road.casing_width.width(zoom);
        let key = format!("highway {}", road.key);
        let casing = b.stroke(road.casing, width, true);
        let fill = b.stroke(road.fill, fill_width, true);
        b.line(&key, vec![casing, fill]);
        b.from(&key, road.from);

        let key = format!("highway {} tunnel", road.key);
        let casing = b.dashed("tunnel casing", width, false, 4.0, 2.0);
        let fill = b.stroke(road.tunnel_fill, fill_width, true);
        b.line(&key, vec![casing, fill]);
        b.from(&key, road.from);
    }

    // Paths: from zoom 15 a faint white halo two pixels wider, then the
    // dashed line; below it, the line alone, dotted whatever the surface.
    let path = |b: &Builder, color: &str, width: f64, round: bool, dash: (f64, f64)| {
        let line = b.dashed(color, width, round, dash.0, dash.1);
        if zoom >= 15 {
            vec![b.stroke("path halo", width + 2.0, true), line]
        } else {
            vec![line]
        }
    };
    // Carto's unknown-surface patterns, 1,3,2,4 and 1,4,2,3, have one dash
    // and gap each side of 1.5 and 3.5 on average.
    let (paved, unknown, unpaved) = match zoom {
        ..=14 => ((1.0, 3.0), (1.0, 3.0), (1.0, 3.0)),
        15 => ((2.0, 3.5), (1.5, 3.5), (1.0, 4.0)),
        16 => ((3.0, 3.5), (1.5, 3.5), (1.0, 4.0)),
        _ => ((3.0, 3.0), (1.5, 3.5), (1.0, 4.0)),
    };
    let width = at(zoom, &[(14, 0.7), (15, 1.0), (16, 1.3)]);
    for (key, dash) in [
        ("footway paved", paved),
        ("footway unpaved", unpaved),
        ("footway", unknown),
    ] {
        let parts = path(&b, "footway", width, true, dash);
        b.line(key, parts);
        b.from(key, 14);
    }
    let width = at(zoom, &[(13, 0.7), (15, 0.9)]);
    for (key, dash) in [
        ("cycleway paved", paved),
        ("cycleway unpaved", unpaved),
        ("cycleway", unknown),
    ] {
        let parts = path(&b, "cycleway", width, true, dash);
        b.line(key, parts);
    }
    let width = at(zoom, &[(13, 0.3), (15, 1.2)]);
    let parts = path(&b, "bridleway", width, false, (4.0, 2.0));
    b.line("bridleway", parts);
    let width = at(zoom, &[(14, 0.7), (15, 3.0)]);
    let parts = path(&b, "footway", width, false, (2.0, 1.0));
    b.line("steps", parts);
    b.from("steps", 14);
    // A track's dashes are shorter below zoom 15, and every grade as thin.
    let low = zoom < 15;
    for (key, dash, high_dash) in [
        ("track", Some((5.0, 4.0)), Some((5.0, 4.0))),
        ("track grade1", None, None),
        ("track grade2", Some((8.8, 3.2)), Some((11.0, 4.0))),
        ("track grade3", Some((5.6, 4.0)), Some((7.0, 5.0))),
        ("track grade4", Some((3.2, 4.8)), Some((4.0, 6.0))),
        ("track grade5", Some((1.6, 6.4)), Some((2.0, 8.0))),
    ] {
        let width = match key {
            _ if low => 0.5,
            "track grade1" | "track grade2" => 0.75,
            _ => 1.5,
        };
        let parts = match if low { dash } else { high_dash } {
            Some(dash) => path(&b, "track", width, true, dash),
            None if low => vec![b.stroke("track", width, true)],
            None => vec![
                b.stroke("path halo", width + 2.0, true),
                b.stroke("track", width, true),
            ],
        };
        b.line(key, parts);
    }

    // Railways: a grey line with a white dash down the middle.
    let rail_dash = if zoom >= 15 { (8.0, 9.0) } else { (8.0, 8.0) };
    let parts = vec![
        b.stroke("rail", 3.0, false),
        b.dashed("rail light", 1.0, false, rail_dash.0, rail_dash.1),
    ];
    b.line("rail", parts);
    let parts = vec![
        b.stroke("rail service", 2.0, false),
        b.dashed("rail light", 0.8, false, 8.0, 9.0),
    ];
    b.line("rail service", parts);
    let parts = vec![b.dashed("rail tunnel", 3.0, false, 5.0, 2.0)];
    b.line("rail tunnel", parts);
    let width = at(zoom, &[(13, 1.5), (14, 2.0)]);
    let parts = vec![b.stroke("light rail", width, false)];
    b.line("light rail", parts);
    let width = at(zoom, &[(13, 0.75), (14, 1.0), (15, 1.5), (17, 2.0)]);
    let parts = vec![b.stroke("tram", width, false)];
    b.line("tram", parts);
    let parts = vec![b.dashed("tram", width, false, 5.0, 3.0)];
    b.line("tram tunnel", parts);
    let width = at(zoom, &[(15, 0.5), (17, 1.0)]);
    let parts = vec![b.stroke("tram", width, false)];
    b.line("tram service", parts);
    b.from("tram service", 15);
    let parts = vec![
        b.stroke("path halo", 4.0, true),
        b.dashed("monorail", 3.0, true, 2.0, 3.0),
    ];
    b.line("monorail", parts);
    b.from("monorail", 14);

    // Waterways (water.mss). A canal is a stream 1.4 times as wide; a ditch
    // is a stream until zoom 14, and no wider until zoom 17.
    let stream = at(
        zoom,
        &[(13, 1.4), (14, 2.0), (15, 2.5), (16, 3.0), (17, 3.5)],
    );
    for (key, width) in [
        (
            "river",
            at(
                zoom,
                &[(13, 3.0), (14, 4.5), (15, 6.0), (16, 8.0), (17, 10.0)],
            ),
        ),
        ("canal", stream * 1.4),
        ("stream", stream),
        ("ditch", at(zoom, &[(13, 1.4), (14, 2.0), (17, 3.0)])),
    ] {
        let parts = vec![b.stroke("water", width, true)];
        b.line(key, parts);
    }

    // Barriers (landcover.mss, #barriers): city walls from zoom 15, with a
    // wall's line down the middle from zoom 17, the others from zoom 16.
    let parts = vec![b.stroke("barrier", 0.4, false)];
    b.line("barrier", parts);
    b.from("barrier", 16);
    let parts = vec![b.stroke("hedge", at(zoom, &[(16, 1.5), (17, 2.0)]), false)];
    b.line("hedge", parts);
    b.from("hedge", 16);
    let width = at(zoom, &[(15, 1.0), (16, 1.5), (17, 2.0)]);
    let mut parts = vec![b.stroke("city wall", width, false)];
    if zoom >= 17 {
        parts.push(b.stroke("barrier", 0.4, false));
    }
    b.line("city wall", parts);
    b.from("city wall", 15);

    // Trees (amenity-points.mss, #trees), from zoom 16: a dot, a line, 2.5
    // px wide and 5 from zoom 17.
    let width = at(zoom, &[(16, 2.5), (17, 5.0)]);
    let parts = vec![b.stroke("tree", width, true)];
    b.line("tree row", parts);
    let tree = PointSymbol {
        name: "tree".to_string(),
        inner_radius: px(width) / 2.0,
        inner_color: b.color("tree"),
        ..PointSymbol::new()
    };
    b.add("tree", Symbol::Point(tree));
    b.from("tree row", 16);
    b.from("tree", 16);

    // Bus and train lines: the route, the stops and the number at either end.
    for line in lines {
        let key = format!("{} {}", line.mode.key(), line.reference);
        let width = line_width_px(line.mode);
        let (stop_radius, ring) = stop_circle_px(line.mode);
        let route = b.stroke(&key, width, true);
        b.line(&key, vec![route]);
        // Thinner, for a bundle too wide at the full width: `bus 13 w2` is
        // the line at the third of the bundle widths.
        for (class, factor) in WIDTH_FACTORS.iter().enumerate().skip(1) {
            let thinner = b.stroke(&key, width * factor, true);
            b.line(&format!("{key} w{class}"), vec![thinner]);
        }
        let stop = PointSymbol {
            name: format!("{key} stop"),
            inner_radius: px(stop_radius),
            inner_color: b.color("bus stop"),
            outer_width: px(ring),
            outer_color: b.color(&key),
            ..PointSymbol::new()
        };
        b.add(&format!("{key} stop"), Symbol::Point(stop));
        let label = TextSymbol {
            name: format!("{key} label"),
            font_family: "Noto Sans".to_string(),
            font_size: px(13.0),
            bold: true,
            color: b.color("bus label"),
            framing: true,
            framing_color: b.color(&format!("{key} badge")),
            framing_mode: 1,
            framing_line_half_width: px(2.8),
            ..TextSymbol::default()
        };
        b.add(&format!("{key} label"), Symbol::Text(label));
        // How often the line runs, written along it: smaller than the
        // number at its ends, and turned with the route.
        let headway = TextSymbol {
            name: format!("{key} headway"),
            font_family: "Noto Sans".to_string(),
            font_size: px(9.0),
            bold: true,
            is_rotatable: true,
            color: b.color("bus label"),
            framing: true,
            framing_color: b.color(&format!("{key} badge")),
            framing_mode: 1,
            framing_line_half_width: px(1.6),
            ..TextSymbol::default()
        };
        b.add(&format!("{key} headway"), Symbol::Text(headway));
    }

    // A stop on a bundle: a white bar outlined in dark grey across it, with
    // round ends, like a station on a transit map. A stop of several lines
    // off every bundle: a circle in the same colours. And beside either, a
    // dark triangle pointing the way the buses stopping there run, drawn
    // pointing right and turned.
    if !lines.is_empty() {
        let outline = b.stroke("stop bar outline", 2.4, true);
        let fill = b.stroke("bus stop", 1.6, true);
        b.line("transit stop bar", vec![outline, fill]);
        let (radius, ring) = stop_circle_px(Mode::Bus);
        let stop = PointSymbol {
            name: "transit stop".to_string(),
            inner_radius: px(radius),
            inner_color: b.color("bus stop"),
            outer_width: px(ring),
            outer_color: b.color("stop bar outline"),
            ..PointSymbol::new()
        };
        b.add("transit stop", Symbol::Point(stop));
        let (length, width) = (px(ARROW_PX.0), px(ARROW_PX.1));
        let mut triangle = Object::new(ObjectKind::Path(PathObject::default()));
        triangle.coords = vec![
            Coord::new(length / 2.0, 0.0, 0),
            Coord::new(-length / 2.0, -width / 2.0, 0),
            Coord::new(-length / 2.0, width / 2.0, 0),
            Coord::new(length / 2.0, 0.0, coord_flag::CLOSE_POINT),
        ];
        let arrow = PointSymbol {
            name: "transit stop arrow".to_string(),
            is_rotatable: true,
            elements: vec![Element {
                symbol: Symbol::Area(b.fill("stop bar outline")),
                object: triangle,
            }],
            ..PointSymbol::new()
        };
        b.add("transit stop arrow", Symbol::Point(arrow));
    }

    // The legend's title and captions.
    let legend_text = |name: &str, size: f64, bold: bool| TextSymbol {
        name: name.to_string(),
        font_family: "Noto Sans".to_string(),
        font_size: px(size),
        bold,
        color: b.color("legend text"),
        ..TextSymbol::default()
    };
    let title = legend_text("legend title", 16.0, true);
    let caption = legend_text("legend text", 10.0, false);
    b.add("legend title", Symbol::Text(title));
    b.add("legend text", Symbol::Text(caption));

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

    /// The key a symbol was defined with, e.g. `"highway primary"`.
    pub fn key(&self, id: i32) -> Option<&str> {
        self.ids
            .iter()
            .find(|(_, &i)| i == id)
            .map(|(key, _)| key.as_str())
    }

    /// The symbol with this id.
    pub fn symbol(&self, id: i32) -> Option<&Symbol> {
        self.symbols.iter().find(|(i, _)| *i == id).map(|(_, s)| s)
    }

    /// What a legend shows for a symbol: a patch of area, a stretch of line,
    /// a single point, or a piece of text.
    pub fn sample(&self, id: i32) -> Option<Sample> {
        let sample = match self.symbol(id)? {
            Symbol::Area(_) => Sample::Area,
            Symbol::Line(_) => Sample::Line,
            Symbol::Point(_) => Sample::Point,
            Symbol::Text(_) => Sample::Text,
            Symbol::Combined(c) if c.owned_parts.iter().any(|p| matches!(p, Symbol::Area(_))) => {
                Sample::Area
            }
            Symbol::Combined(_) => Sample::Line,
        };
        Some(sample)
    }

    /// What the legend calls a symbol, in Italian: `None` for those it does
    /// not list — the land under everything, the legend's own text, a bus or
    /// train line's stops and numbers, which get an entry of their own.
    pub fn caption(&self, id: i32) -> Option<String> {
        let key = self.key(id)?;
        if key == "land" || key.starts_with("legend ") {
            return None;
        }
        for mode in ["bus", "train"] {
            if let Some(reference) = key.strip_prefix(mode).and_then(|k| k.strip_prefix(' ')) {
                let thinner = reference
                    .rsplit_once(" w")
                    .is_some_and(|(_, class)| class.parse::<usize>().is_ok());
                if thinner
                    || reference.ends_with(" stop")
                    || reference.ends_with(" label")
                    || reference.ends_with(" headway")
                {
                    return None;
                }
                let word = if mode == "bus" { "Bus" } else { "Treno" };
                return Some(format!("{word} {reference}"));
            }
        }
        if let Some(road) = key.strip_prefix("highway ") {
            let (road, tunnel) = match road.strip_suffix(" tunnel") {
                Some(road) => (road, true),
                None => (road, false),
            };
            let name = match road {
                "motorway" => "Autostrada",
                "motorway_link" => "Svincolo autostradale",
                "trunk" => "Superstrada",
                "trunk_link" => "Svincolo di superstrada",
                "primary" => "Strada principale",
                "primary_link" => "Svincolo di strada principale",
                "secondary" => "Strada secondaria",
                "secondary_link" => "Svincolo di strada secondaria",
                "tertiary" => "Strada di quartiere",
                "tertiary_link" => "Svincolo di strada di quartiere",
                "residential" => "Strada residenziale",
                "unclassified" => "Strada locale",
                "living_street" => "Via in zona residenziale",
                "pedestrian" => "Via pedonale",
                "road" => "Strada",
                "service" => "Strada di servizio",
                "minor_service" => "Accesso o corsia di parcheggio",
                _ => return None,
            };
            return Some(if tunnel {
                format!("{name} in galleria")
            } else {
                name.to_string()
            });
        }
        let caption = match key {
            "residential" => "Zona residenziale",
            "retail" => "Zona commerciale (negozi)",
            "commercial" => "Zona commerciale (uffici)",
            "industrial" => "Zona industriale o ferroviaria",
            "farmland" => "Terreno agricolo",
            "farmyard" => "Aia, fattoria",
            "garages" => "Garage",
            "construction" => "Cantiere",
            "grass" => "Prato, giardino",
            "park" => "Parco",
            "cemetery" => "Cimitero",
            "allotments" => "Orti",
            "orchard" => "Frutteto, vigneto",
            "scrub" => "Arbusteto",
            "forest" => "Bosco",
            "heath" => "Brughiera",
            "sand" => "Sabbia",
            "societal amenities" => "Scuola, università, ospedale",
            "platform" => "Banchina",
            "bridge" => "Ponte",
            "water" => "Acqua",
            "leisure" => "Impianto sportivo",
            "playground" => "Parco giochi",
            "pitch" => "Campo sportivo",
            "parking" => "Parcheggio",
            "place of worship" => "Area di culto",
            "pedestrian area" => "Piazza, area pedonale",
            "military" => "Zona militare",
            "building" => "Edificio",
            "building major" => "Chiesa, stazione",
            "footway paved" => "Marciapiede, sentiero pavimentato",
            "footway unpaved" => "Sentiero sterrato",
            "footway" => "Sentiero pedonale",
            "cycleway paved" => "Pista ciclabile",
            "cycleway unpaved" => "Pista ciclabile sterrata",
            "cycleway" => "Pista ciclabile",
            "bridleway" => "Ippovia",
            "steps" => "Scalinata",
            "track" => "Carrareccia",
            "track grade1" => "Carrareccia pavimentata",
            "track grade2" => "Carrareccia compatta",
            "track grade3" | "track grade4" | "track grade5" => "Carrareccia sterrata",
            "rail" => "Ferrovia",
            "rail service" => "Binario di servizio",
            "rail tunnel" => "Ferrovia in galleria",
            "light rail" => "Metropolitana leggera",
            "tram" => "Tranvia",
            "tram tunnel" => "Tranvia in galleria",
            "tram service" => "Binario tranviario di servizio",
            "monorail" => "Monorotaia",
            "river" => "Fiume",
            "canal" => "Canale",
            "stream" => "Torrente",
            "ditch" => "Fosso",
            "barrier" => "Muro, recinzione",
            "hedge" => "Siepe",
            "city wall" => "Mura cittadine",
            "tree row" => "Filare di alberi",
            "tree" => "Albero",
            _ => return None,
        };
        Some(caption.to_string())
    }

    /// Whether carto draws the symbol with this id at the level's zoom.
    fn shown(&self, id: i32) -> bool {
        self.from
            .get(&id)
            .is_none_or(|&from| self.details.zoom() >= from)
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
                "swimming_pool" if self.details.zoom() >= 14 => "water",
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
            .filter(|&id| self.shown(id))
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
                "monorail" => Some("monorail"),
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
        out.retain(|&id| self.shown(id));
        out
    }

    /// The symbol for a road or path, as carto groups `highway` values.
    fn highway_key(&self, highway: &str, tags: &Tags, tunnel: bool) -> Option<String> {
        // A path closed to those it is for waits until zoom 15.
        if self.details.zoom() < 15 && closed(highway, tags) {
            return None;
        }
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
        let mut out = Vec::new();
        if tags.is("natural", "tree") {
            out.push(self.id("tree"));
        }
        out.retain(|&id| self.shown(id));
        out
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
    fn line_colours_after_the_first_ten_do_not_repeat() {
        let colors: Vec<String> = (0..200).map(line_color).collect();
        assert_eq!(colors[0], BUS_COLORS[0]);
        assert_eq!(colors[9], BUS_COLORS[9]);
        let unique: std::collections::HashSet<&String> = colors.iter().collect();
        assert_eq!(unique.len(), 200);
        assert_eq!(hsl(0.0, 1.0, 0.5), "#ff0000");
    }

    #[test]
    fn every_casing_is_drawn_below_every_road_fill() {
        let style = carto_style(Details::default());
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
        let style = carto_style(Details::default());
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
    fn the_last_step_of_a_cascade_at_or_below_the_zoom_holds() {
        let steps = [(13, 2.5), (15, 5.0), (17, 12.0)];
        assert_eq!(at(13, &steps), 2.5);
        assert_eq!(at(14, &steps), 2.5);
        assert_eq!(at(16, &steps), 5.0);
        assert_eq!(at(18, &steps), 12.0);
        // Below the first step, the first one.
        assert_eq!(at(12, &steps), 2.5);
    }

    #[test]
    fn each_level_is_a_carto_zoom_at_a_scale_of_its_own() {
        let levels: Vec<(u32, i32)> = (0..=Details::MAX)
            .map(|level| Details::new(level).unwrap())
            .map(|d| (d.zoom(), d.scale()))
            .collect();
        assert_eq!(
            levels,
            [
                (13, 24_000),
                (14, 17_000),
                (15, 10_000),
                (16, 5_000),
                (17, 2_500)
            ]
        );
        assert_eq!(Details::new(5), None);
        assert_eq!(Details::default().zoom(), 16);
    }

    #[test]
    fn a_feature_shows_from_the_zoom_carto_starts_drawing_it_at() {
        let style = |level| carto_style(Details::new(level).unwrap());
        let line = |level, pairs: &[(&str, &str)]| style(level).line_symbols(&tags(pairs));
        let area = |level, pairs: &[(&str, &str)]| style(level).area_symbols(&tags(pairs), false);
        let footway = [("highway", "footway")];
        let private = [("highway", "footway"), ("access", "private")];
        let driveway = [("highway", "service"), ("service", "driveway")];
        let building = [("building", "yes")];
        let fence = [("barrier", "fence")];

        assert!(line(0, &footway).is_empty());
        assert_eq!(line(1, &footway).len(), 1);
        // Closed to walkers, a footway waits for zoom 15.
        assert!(line(1, &private).is_empty());
        assert_eq!(line(2, &private).len(), 1);
        assert!(line(2, &driveway).is_empty());
        assert_eq!(line(3, &driveway).len(), 1);
        assert!(area(0, &building).is_empty());
        assert_eq!(area(1, &building).len(), 1);
        assert!(line(2, &fence).is_empty());
        assert_eq!(line(3, &fence).len(), 1);
        // Trees from zoom 16.
        assert!(style(2)
            .point_symbols(&tags(&[("natural", "tree")]))
            .is_empty());
        assert_eq!(
            style(3).point_symbols(&tags(&[("natural", "tree")])).len(),
            1
        );
        // A residential road everywhere.
        for level in 0..=Details::MAX {
            assert_eq!(line(level, &[("highway", "residential")]).len(), 1);
        }
    }

    #[test]
    fn roads_widen_with_the_zoom() {
        let casing_width = |level| {
            let style = carto_style(Details::new(level).unwrap());
            match style.symbol(style.id("highway residential")) {
                Some(Symbol::Combined(road)) => match &road.owned_parts[0] {
                    Symbol::Line(casing) => casing.line_width / PIXEL_MM,
                    _ => panic!("a road's parts are lines"),
                },
                _ => panic!("a road is a combined symbol"),
            }
        };
        let widths: Vec<f64> = (0..=Details::MAX).map(casing_width).collect();
        for (width, carto) in widths.iter().zip([2.5, 3.0, 5.0, 6.0, 12.0]) {
            assert!((width - carto).abs() < 1e-9, "{widths:?}");
        }
    }

    #[test]
    fn tags_pick_symbols_the_way_carto_groups_them() {
        let style = carto_style(Details::default());
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
        assert_eq!(
            line(&[("railway", "monorail"), ("bridge", "viaduct")]),
            vec![style.id("monorail")]
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
