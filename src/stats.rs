//! What a map contains, counted.
//!
//! Not what it looks like and not whether it is correct: how many objects it
//! has, which symbols it actually uses and how much of each, how far its
//! lines run and how much ground its areas cover. The things a panel beside
//! the map shows, and the box the map occupies.
//!
//! ```no_run
//! use maur_o::{stats, xml_reader};
//!
//! let (map, _) = xml_reader::read_xml_map_str("<map/>").unwrap();
//! let counted = stats::stats(&map);
//! println!("{} objects, {} symbols in use", counted.object_count, counted.symbol_count);
//! ```
//!
//! Lengths and areas are on the ground -- metres and square metres -- because
//! that is what they are asked for. A kilometre of track is a kilometre of
//! track; that it is 6.7 cm of paper at 1:15000 is not the interesting half.

use crate::geometry::{flatten, Rect};
use crate::map::{Map, Point, Symbol};

/// The scale assumed for a map that does not say.
const DEFAULT_MAP_SCALE: f64 = 15000.0;

/// How much ground a millimetre of paper covers.
fn meters_per_mm(map: &Map) -> f64 {
    let scale = if map.scale_denominator > 0 {
        f64::from(map.scale_denominator)
    } else {
        DEFAULT_MAP_SCALE
    };
    scale / 1000.0
}

/// The box every coordinate of the map falls inside, in mm.
///
/// The *coordinates*, not what is drawn from them: a line's width falls
/// outside this and [`crate::renderer::Renderer::extent`] is the one that
/// accounts for it. This is the box to place things against, which is why it
/// does not move when a symbol's width changes.
///
/// `None` for a map with nothing on it.
pub fn coordinate_bounds(map: &Map) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for object in &map.objects {
        for coord in &object.coords {
            bounds = Some(match bounds {
                None => Rect::from_ltrb(coord.x, coord.y, coord.x, coord.y),
                Some(b) => Rect::from_ltrb(
                    b.left().min(coord.x),
                    b.top().min(coord.y),
                    b.right().max(coord.x),
                    b.bottom().max(coord.y),
                ),
            });
        }
    }
    bounds
}

/// What kind of thing a symbol draws, as a word.
fn kind_name(symbol: &Symbol) -> &'static str {
    match symbol {
        Symbol::Point(_) => "point",
        Symbol::Line(_) => "line",
        Symbol::Area(_) => "area",
        Symbol::Text(_) => "text",
        Symbol::Combined(_) => "combined",
    }
}

/// The symbols a symbol draws with: itself, or the parts of a combination.
fn leaves<'m>(symbol: &'m Symbol, map: &'m Map, out: &mut Vec<&'m Symbol>) {
    let Symbol::Combined(combined) = symbol else {
        out.push(symbol);
        return;
    };
    for part in &combined.parts {
        match *part {
            crate::map::PartRef::Shared(i) => {
                if let Some(part) = map.symbols.get(i) {
                    leaves(part, map, out);
                }
            }
            crate::map::PartRef::Private(i) => {
                if let Some(part) = combined.owned_parts.get(i) {
                    leaves(part, map, out);
                }
            }
            crate::map::PartRef::None => {}
        }
    }
}

/// One symbol of the map, and how much of the map is drawn with it.
#[derive(Clone, Debug)]
pub struct SymbolUse {
    /// The symbol's place in [`Map::symbols`].
    pub index: usize,
    /// What the symbol set numbers it.
    pub code: String,
    /// What the symbol set calls it.
    pub name: String,
    /// What it draws: point, line, area, text, or combined.
    pub kind: &'static str,
    /// How many objects are drawn with it.
    pub count: usize,
    /// How far its lines run, on the ground, in metres. Zero unless it draws
    /// lines.
    pub length_m: f64,
    /// How much ground its areas cover, in square metres, holes taken off.
    /// Zero unless it fills areas.
    pub area_m2: f64,
    /// The picture of the symbol the file carries, as the file carries it --
    /// a `data:` URI, usually. Empty where the file has none.
    pub icon_src: String,
}

/// How long a polyline is.
fn polyline_length(points: &[Point]) -> f64 {
    points
        .windows(2)
        .map(|p| (p[1].x - p[0].x).hypot(p[1].y - p[0].y))
        .sum()
}

/// How much a closed polyline encloses, by the shoelace formula.
fn polyline_area(points: &[Point]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut sum = 0f64;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        sum += a.x * b.y - b.x * a.y;
    }
    (sum / 2.0).abs()
}

/// The rings of an object, straightened.
fn object_rings(object: &crate::map::Object) -> Vec<Vec<Point>> {
    let parts = flatten(&object.coords);
    let mut rings = Vec::new();
    for part in &parts {
        let points: Vec<Point> = part.points.clone();
        if points.len() > 1 {
            rings.push(points);
        }
    }
    rings
}

/// Which symbols the map actually uses, and how much of each.
///
/// Symbols the map defines but never draws with are left out: a symbol set
/// carries hundreds, and a map uses a fraction of them.
///
/// Ordered by code, as a symbol set is.
pub fn symbol_usage(map: &Map) -> Vec<SymbolUse> {
    let m_per_mm = meters_per_mm(map);
    let mut counts = vec![0usize; map.symbols.len()];
    let mut lengths = vec![0f64; map.symbols.len()];
    let mut areas = vec![0f64; map.symbols.len()];

    // What a symbol draws with is what decides whether an object of it is
    // measured as a line or as an area.
    let mut kinds: Vec<&'static str> = Vec::with_capacity(map.symbols.len());
    for symbol in &map.symbols {
        let mut resolved = Vec::new();
        leaves(symbol, map, &mut resolved);
        kinds.push(resolved.first().map_or("unknown", |s| kind_name(s)));
    }

    for object in &map.objects {
        let Some(index) = object.symbol_index else {
            continue;
        };
        if index >= counts.len() {
            continue;
        }
        counts[index] += 1;
        match kinds[index] {
            "line" => {
                let rings = object_rings(object);
                lengths[index] += rings.iter().map(|r| polyline_length(r)).sum::<f64>();
            }
            "area" => {
                // The first ring is the outside; the rest are holes in it.
                let rings = object_rings(object);
                let mut area = 0.0;
                for (i, ring) in rings.iter().enumerate() {
                    let a = polyline_area(ring);
                    area += if i == 0 { a } else { -a };
                }
                areas[index] += area;
            }
            _ => {}
        }
    }

    let mut used: Vec<SymbolUse> = map
        .symbols
        .iter()
        .enumerate()
        .filter(|(i, _)| counts[*i] > 0)
        .map(|(i, symbol)| SymbolUse {
            index: i,
            code: symbol.code().to_string(),
            name: symbol.name().to_string(),
            kind: kinds[i],
            count: counts[i],
            length_m: lengths[i] * m_per_mm,
            area_m2: areas[i] * m_per_mm * m_per_mm,
            icon_src: symbol.icon_src().to_string(),
        })
        .collect();

    // By code, as a number where it is one -- so 9 comes before 101.
    used.sort_by(|a, b| {
        let an = a.code.parse::<f64>().unwrap_or(0.0);
        let bn = b.code.parse::<f64>().unwrap_or(0.0);
        an.partial_cmp(&bn)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.code.cmp(&b.code))
    });
    used
}

/// A map, in numbers.
#[derive(Clone, Debug)]
pub struct Stats {
    /// Every object on the map, whatever it is drawn with.
    pub object_count: usize,
    /// How many symbols have at least one object.
    pub symbol_count: usize,
    /// The map's scale: 15000 for a 1:15000 map.
    pub scale: f64,
    /// How far every line on the map runs, on the ground, in metres.
    pub total_line_length_m: f64,
    /// How much ground every area covers, in square metres.
    pub total_area_m2: f64,
}

/// Counts up the whole map.
pub fn stats(map: &Map) -> Stats {
    let used = symbol_usage(map);
    Stats {
        object_count: map.objects.len(),
        symbol_count: used.len(),
        scale: if map.scale_denominator > 0 {
            f64::from(map.scale_denominator)
        } else {
            DEFAULT_MAP_SCALE
        },
        total_line_length_m: used.iter().map(|s| s.length_m).sum(),
        total_area_m2: used.iter().map(|s| s.area_m2).sum(),
    }
}
