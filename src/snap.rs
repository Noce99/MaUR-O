//! What a course setter can put a control on.
//!
//! Placing a control asks the map two questions -- is there a point feature
//! near enough to snap to, and what is the feature here called -- and both
//! are answered by proximity to objects rather than by drawing anything. This
//! builds the table those questions are asked of: every object that has a
//! code, a kind and some geometry, with its anchor points and the box they
//! fall in.
//!
//! ```no_run
//! use maur_o::{snap::snap_table, xml_reader};
//!
//! let (map, _) = xml_reader::read_xml_map_str("<map/>").unwrap();
//! for entry in snap_table(&map) {
//!     println!("{} {:?}", entry.code, entry.kind);
//! }
//! ```
//!
//! # Anchor points, not drawn geometry
//!
//! The points here are the object's own coordinates, curve control points
//! included and curves not flattened. That is deliberate: a control is placed
//! within a couple of millimetres of a feature, and at that distance an
//! anchor point is as good an answer as a flattened one for a fraction of
//! the memory -- this table is built once per map and kept for as long as it
//! is open.

use crate::geometry::Rect;
use crate::map::{symbol_leaves, Map, Point, Symbol};

/// What kind of feature an object is, for the purpose of snapping to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A boulder, a knoll, a pit: something at a position.
    Point,
    /// A path, a stream, a fence: something along a line.
    Line,
    /// A marsh, a clearing, a lake: something covering ground.
    Area,
}

impl Kind {
    /// The kind as the word for it.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Point => "point",
            Kind::Line => "line",
            Kind::Area => "area",
        }
    }
}

/// One object, as something to snap to.
pub struct SnapEntry {
    /// The symbol's code, as the file gives it: "203", "310.1".
    pub code: String,
    /// What kind of feature it is.
    pub kind: Kind,
    /// The object's own anchor points, in mm on the paper.
    pub points: Vec<Point>,
    /// The box those points fall in, in mm.
    pub bounds: Rect,
}

/// What kind of feature an object of this symbol is.
///
/// A point part wins: a symbol that draws a boulder *and* a ring around it is
/// a boulder, and a control on it is on the boulder. That is the opposite of
/// the priority [`crate::runnability`] uses, where the same symbol is
/// whatever covers ground, because there the question is what a runner
/// crosses rather than what they are looking for.
fn kind_of(symbol: &Symbol, map: &Map) -> Option<Kind> {
    let mut resolved = Vec::new();
    symbol_leaves(symbol, map, &mut resolved);
    if resolved.iter().any(|s| matches!(s, Symbol::Point(_))) {
        return Some(Kind::Point);
    }
    if resolved.iter().any(|s| matches!(s, Symbol::Area(_))) {
        return Some(Kind::Area);
    }
    if resolved.iter().any(|s| matches!(s, Symbol::Line(_))) {
        return Some(Kind::Line);
    }
    None
}

/// Everything on the map worth snapping a control to.
///
/// Objects with no symbol, no code, no kind or no coordinates are left out
/// rather than carried as holes: nothing can be snapped to them, and the
/// table is queried far more often than it is built.
pub fn snap_table(map: &Map) -> Vec<SnapEntry> {
    let mut table = Vec::new();
    for object in &map.objects {
        let Some(index) = object.symbol_index else {
            continue;
        };
        let Some(symbol) = map.symbols.get(index) else {
            continue;
        };
        let code = symbol.code().trim();
        if code.is_empty() || object.coords.is_empty() {
            continue;
        }
        let Some(kind) = kind_of(symbol, map) else {
            continue;
        };

        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        let mut points = Vec::with_capacity(object.coords.len());
        for coord in &object.coords {
            min_x = min_x.min(coord.x);
            min_y = min_y.min(coord.y);
            max_x = max_x.max(coord.x);
            max_y = max_y.max(coord.y);
            points.push(Point::new(coord.x, coord.y));
        }

        table.push(SnapEntry {
            code: code.to_string(),
            kind,
            points,
            bounds: Rect::new(min_x, min_y, max_x - min_x, max_y - min_y),
        });
    }
    table
}
