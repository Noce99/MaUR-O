//! Takes a symbol set apart into the kinds a random map draws with.
//!
//! Step one of generating a random map: before anything can be drawn, the
//! symbols a map file offers have to be sorted into what they are *for*. A
//! generator needs to know which symbols cover the ground opaquely, which
//! cover it while letting what is under them show through, which run along a
//! line, which sit at a point and which are lettering — because those are
//! the questions "what goes in this cell" and "what may be drawn over it"
//! turn into.
//!
//! Five kinds, then, and every symbol of a set falls in exactly one:
//!
//! * [`SymbolKind::OpaqueArea`] — an area with a fully opaque fill. Drawn
//!   over anything, it hides it.
//! * [`SymbolKind::TransparentArea`] — an area with something see-through
//!   about it: a fill colour with an opacity below one, or no fill colour at
//!   all, which is how a symbol that is only a pattern of dots or dashes is
//!   spelled.
//! * [`SymbolKind::Line`], [`SymbolKind::Point`], [`SymbolKind::Text`] — the
//!   other three personalities, as the format has them.
//!
//! Combined symbols are not a kind of their own here. The renderer has to
//! know that a road is an area and a line drawn over the same object; a
//! generator only has to know it is the sort of thing which covers ground,
//! so a combined symbol takes the kind of the strongest personality it is
//! built out of — an area if any part is one, then a line, then a point,
//! then text. A part which is itself combined is followed the same way.

use crate::map::{is_color, Map, Symbol};

/// How deep a combined symbol's parts are followed before giving up. A symbol
/// which is its own part is not a thing Mapper writes, but a file is a file.
const MAX_PART_DEPTH: usize = 16;

/// What a symbol is for, as a generator asks about it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolKind {
    /// An area which covers what is under it completely.
    OpaqueArea,
    /// An area which lets some of what is under it through.
    TransparentArea,
    /// A line.
    Line,
    /// A point.
    Point,
    /// Lettering.
    Text,
}

impl SymbolKind {
    /// The name this kind goes by in reports.
    pub fn name(self) -> &'static str {
        match self {
            SymbolKind::OpaqueArea => "opaque area",
            SymbolKind::TransparentArea => "transparent area",
            SymbolKind::Line => "line",
            SymbolKind::Point => "point",
            SymbolKind::Text => "text",
        }
    }
}

/// One symbol of the set, in the two ways it needs naming: where it is, and
/// what to call it.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Index into [`Map::symbols`].
    pub index: usize,
    /// The symbol id the file gives it, which is what an object drawn with it
    /// refers to. -1 for a symbol the file left unnumbered.
    pub id: i32,
    /// The symbol number, e.g. "501.2".
    pub code: String,
    /// What the symbol set calls it, e.g. "Paved area, bounding line".
    pub name: String,
}

/// A symbol set sorted into the five kinds.
///
/// Hidden and helper symbols are left out of all five: they draw nothing, and
/// a generated map which used one would come out with an empty cell and no
/// way to tell that apart from a rendering bug.
#[derive(Debug, Default)]
pub struct Catalogue {
    /// The areas which cover what is under them.
    pub opaque_areas: Vec<Entry>,
    /// The areas which do not.
    pub transparent_areas: Vec<Entry>,
    /// The lines.
    pub lines: Vec<Entry>,
    /// The point symbols.
    pub points: Vec<Entry>,
    /// The text symbols.
    pub texts: Vec<Entry>,
}

impl Catalogue {
    /// Sorts every visible symbol of `map` into its kind.
    ///
    /// The map must have had [`Map::resolve_references`] run on it, or a
    /// combined symbol has no parts to be judged by and comes out as text.
    pub fn of(map: &Map) -> Catalogue {
        let mut catalogue = Catalogue::default();
        for (index, symbol) in map.symbols.iter().enumerate() {
            if !symbol.is_visible() {
                continue;
            }
            let entry = Entry {
                index,
                id: map.symbol_ids.get(index).copied().unwrap_or(-1),
                code: code_of(symbol).to_string(),
                name: name_of(symbol).to_string(),
            };
            match kind_of(map, symbol) {
                SymbolKind::OpaqueArea => catalogue.opaque_areas.push(entry),
                SymbolKind::TransparentArea => catalogue.transparent_areas.push(entry),
                SymbolKind::Line => catalogue.lines.push(entry),
                SymbolKind::Point => catalogue.points.push(entry),
                SymbolKind::Text => catalogue.texts.push(entry),
            }
        }
        catalogue
    }

    /// Every kind with the symbols sorted into it, in a fixed order, for
    /// counting and reporting.
    pub fn by_kind(&self) -> [(SymbolKind, &[Entry]); 5] {
        [
            (SymbolKind::OpaqueArea, &self.opaque_areas),
            (SymbolKind::TransparentArea, &self.transparent_areas),
            (SymbolKind::Line, &self.lines),
            (SymbolKind::Point, &self.points),
            (SymbolKind::Text, &self.texts),
        ]
    }

    /// How many symbols were sorted, over all five kinds.
    pub fn len(&self) -> usize {
        self.by_kind().iter().map(|(_, list)| list.len()).sum()
    }

    /// Whether the set held nothing which draws anything.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The first symbol of any kind whose name is exactly `name`.
    ///
    /// Names are not unique in a symbol set — a symbol and the part of a
    /// combined symbol it was made from often share one — so this finds the
    /// first the file lists, which is the one a person naming a symbol out
    /// loud means.
    pub fn named(&self, name: &str) -> Option<&Entry> {
        self.by_kind()
            .into_iter()
            .flat_map(|(_, list)| list.iter())
            .filter(|entry| entry.name == name)
            .min_by_key(|entry| entry.index)
    }
}

/// What kind `symbol` is.
pub fn kind_of(map: &Map, symbol: &Symbol) -> SymbolKind {
    match symbol {
        Symbol::Area(_) => area_kind(map, symbol, 0),
        Symbol::Line(_) => SymbolKind::Line,
        Symbol::Point(_) => SymbolKind::Point,
        Symbol::Text(_) => SymbolKind::Text,
        Symbol::Combined(_) => {
            // The strongest personality the symbol is built out of: what it
            // does to the ground under it is decided by the part which does
            // the most to it.
            if contains(map, symbol, 0, &|part| matches!(part, Symbol::Area(_))) {
                area_kind(map, symbol, 0)
            } else if contains(map, symbol, 0, &|part| matches!(part, Symbol::Line(_))) {
                SymbolKind::Line
            } else if contains(map, symbol, 0, &|part| matches!(part, Symbol::Point(_))) {
                SymbolKind::Point
            } else {
                SymbolKind::Text
            }
        }
    }
}

/// Which of the two area kinds a symbol with an area personality is.
///
/// Opaque takes one area part which covers the ground on its own: a symbol
/// combining a solid fill with a see-through one still hides what is under
/// it.
fn area_kind(map: &Map, symbol: &Symbol, depth: usize) -> SymbolKind {
    if contains(map, symbol, depth, &|part| match part {
        // An area with no fill colour is a pattern over bare ground, and one
        // whose colour is not fully opaque is blended with what it covers;
        // either way, something under it shows through.
        Symbol::Area(area) => {
            is_color(area.color) && map.color(area.color).is_some_and(|c| c.opacity >= 1.0)
        }
        _ => false,
    }) {
        SymbolKind::OpaqueArea
    } else {
        SymbolKind::TransparentArea
    }
}

/// Whether the symbol, or any symbol it is built out of, satisfies `wanted`.
fn contains(map: &Map, symbol: &Symbol, depth: usize, wanted: &dyn Fn(&Symbol) -> bool) -> bool {
    if wanted(symbol) {
        return true;
    }
    match symbol {
        Symbol::Combined(combined) if depth < MAX_PART_DEPTH => map
            .parts(combined)
            .iter()
            .any(|part| contains(map, part, depth + 1, wanted)),
        _ => false,
    }
}

/// What the symbol set calls a symbol.
fn name_of(symbol: &Symbol) -> &str {
    match symbol {
        Symbol::Point(s) => &s.name,
        Symbol::Line(s) => &s.name,
        Symbol::Area(s) => &s.name,
        Symbol::Text(s) => &s.name,
        Symbol::Combined(s) => &s.name,
    }
}

/// The symbol number a symbol carries.
fn code_of(symbol: &Symbol) -> &str {
    match symbol {
        Symbol::Point(s) => &s.code,
        Symbol::Line(s) => &s.code,
        Symbol::Area(s) => &s.code,
        Symbol::Text(s) => &s.code,
        Symbol::Combined(s) => &s.code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{AreaSymbol, Color, CombinedSymbol, LineSymbol, PointSymbol, TextSymbol};

    /// A map with two colours: an opaque one and a half transparent one.
    fn map_with(symbols: Vec<Symbol>) -> Map {
        let mut map = Map::new();
        map.colors = vec![
            Color {
                name: "solid".to_string(),
                rgb: (0.0, 0.0, 0.0),
                opacity: 1.0,
            },
            Color {
                name: "washed out".to_string(),
                rgb: (0.0, 0.0, 0.0),
                opacity: 0.5,
            },
        ];
        map.symbol_ids = (0..symbols.len() as i32).collect();
        map.symbols = symbols;
        map.resolve_references();
        map
    }

    fn area(color: i32) -> Symbol {
        Symbol::Area(AreaSymbol {
            color,
            ..AreaSymbol::new()
        })
    }

    fn combined(part_ids: Vec<i32>) -> Symbol {
        Symbol::Combined(CombinedSymbol {
            part_ids,
            ..Default::default()
        })
    }

    #[test]
    fn an_area_is_opaque_only_where_nothing_shows_through_it() {
        let map = map_with(vec![area(0), area(1), area(-1)]);
        assert_eq!(kind_of(&map, &map.symbols[0]), SymbolKind::OpaqueArea);
        // A colour with an opacity below one.
        assert_eq!(kind_of(&map, &map.symbols[1]), SymbolKind::TransparentArea);
        // No fill colour at all: a symbol which is only its patterns.
        assert_eq!(kind_of(&map, &map.symbols[2]), SymbolKind::TransparentArea);
    }

    #[test]
    fn the_other_three_personalities_are_themselves() {
        let map = map_with(vec![
            Symbol::Line(LineSymbol::default()),
            Symbol::Point(PointSymbol::new()),
            Symbol::Text(TextSymbol::default()),
        ]);
        assert_eq!(kind_of(&map, &map.symbols[0]), SymbolKind::Line);
        assert_eq!(kind_of(&map, &map.symbols[1]), SymbolKind::Point);
        assert_eq!(kind_of(&map, &map.symbols[2]), SymbolKind::Text);
    }

    #[test]
    fn a_combined_symbol_takes_the_kind_of_its_strongest_part() {
        // A road: an opaque fill and the line which bounds it.
        let map = map_with(vec![
            area(0),
            Symbol::Line(LineSymbol::default()),
            combined(vec![0, 1]),
            // A line and a point symbol: no area, so a line.
            combined(vec![1, 4]),
            Symbol::Point(PointSymbol::new()),
        ]);
        assert_eq!(kind_of(&map, &map.symbols[2]), SymbolKind::OpaqueArea);
        assert_eq!(kind_of(&map, &map.symbols[3]), SymbolKind::Line);
    }

    #[test]
    fn one_opaque_part_is_enough_to_hide_the_ground() {
        let map = map_with(vec![area(1), area(0), combined(vec![0, 1])]);
        assert_eq!(kind_of(&map, &map.symbols[2]), SymbolKind::OpaqueArea);
    }

    #[test]
    fn a_combined_symbol_of_see_through_areas_stays_see_through() {
        let map = map_with(vec![area(1), area(-1), combined(vec![0, 1])]);
        assert_eq!(kind_of(&map, &map.symbols[2]), SymbolKind::TransparentArea);
    }

    #[test]
    fn a_hidden_or_helper_symbol_is_in_no_kind_at_all() {
        let map = map_with(vec![
            area(0),
            Symbol::Area(AreaSymbol {
                color: 0,
                is_hidden: true,
                ..AreaSymbol::new()
            }),
            Symbol::Area(AreaSymbol {
                color: 0,
                is_helper_symbol: true,
                ..AreaSymbol::new()
            }),
        ]);
        let catalogue = Catalogue::of(&map);
        assert_eq!(catalogue.len(), 1);
        assert_eq!(catalogue.opaque_areas.len(), 1);
    }

    #[test]
    fn a_symbol_is_found_by_the_name_the_set_gives_it() {
        let mut map = map_with(vec![
            area(0),
            Symbol::Line(LineSymbol {
                name: "Paved area, bounding line".to_string(),
                code: "501.2".to_string(),
                ..Default::default()
            }),
        ]);
        map.resolve_references();
        let catalogue = Catalogue::of(&map);
        let found = catalogue.named("Paved area, bounding line").unwrap();
        assert_eq!(found.index, 1);
        assert_eq!(found.id, 1);
        assert_eq!(found.code, "501.2");
        assert!(catalogue.named("No such symbol").is_none());
    }
}
