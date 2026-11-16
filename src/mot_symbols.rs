//! The symbol table of a MOT layer.
//!
//! A layer is one world map drawn with one symbol set, and the set is defined
//! once, here, instead of in every map. An object in a tile carries only the
//! id of its symbol in this table. See `mds/MOT-Format.md`.
//!
//! Ids are an index into the table, assigned once and never renumbered: a new
//! edition of a standard only appends symbols (or marks some deprecated), so
//! stored objects and their history stay valid. Every symbol also keeps its
//! standard code ("101", "302.1"), which is how another edition's, or another
//! standard's, symbols are matched to it: [`SymbolTable::closest`].
//!
//! Each symbol also carries the rules that say how it is generalised for the
//! zoom levels coarser than the stored detail: [`ZoomRule`]. The world cannot
//! be drawn whole at once, so what is kept at each zoom is the symbol's
//! business, not the converter's.
//!
//! For now the only table is ISOM 2017-2, built from the reference map the
//! crate already ships; its ids are the order of that map's symbols. This is
//! provisional until the table is frozen as a file of its own.

use std::collections::HashMap;

use crate::map::Symbol;
use crate::osm_orienteering::ISOM;
use crate::xml_reader::read_xml_map_str;

/// What a symbol draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    /// A symbol for a single position.
    Point,
    /// A symbol for a line.
    Line,
    /// A symbol for an area.
    Area,
    /// A symbol for text.
    Text,
    /// Several symbols drawn over one object: a line or an area.
    Combined,
}

impl SymbolKind {
    /// The kind of `symbol`.
    pub fn of(symbol: &Symbol) -> SymbolKind {
        match symbol {
            Symbol::Point(_) => SymbolKind::Point,
            Symbol::Line(_) => SymbolKind::Line,
            Symbol::Area(_) => SymbolKind::Area,
            Symbol::Text(_) => SymbolKind::Text,
            Symbol::Combined(_) => SymbolKind::Combined,
        }
    }

    /// Whether one of this kind can stand in for one of `other`. A combined
    /// symbol is made of lines and areas, so it can stand in for either.
    pub fn compatible(self, other: SymbolKind) -> bool {
        use SymbolKind::*;
        self == other
            || (self == Combined && matches!(other, Line | Area))
            || (other == Combined && matches!(self, Line | Area))
    }
}

/// How a symbol is generalised for the zoom levels coarser than the finest one
/// stored, where everything is kept as drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomRule {
    /// The coarsest zoom the symbol appears at: an object of it is left out of
    /// every level below this one.
    pub min_zoom: u8,
    /// The size, in pixels of a 256 pixel tile, below which an object is
    /// dropped as too small to see: its longer side for an area, its length for
    /// a line. 0 keeps every object, as for a point.
    pub min_size_px: f32,
    /// How far, in the same pixels, a line or an area may be moved to have fewer
    /// nodes. 0 leaves the geometry alone.
    pub simplify_px: f32,
}

/// The rule of a symbol with this code and kind.
///
/// Provisional, like the table: what matters at a zoom is what a reader can
/// still tell at a glance at that scale, so the large land covers and the roads
/// come first, then the contours and the buildings, and the small features only
/// at the finest zoom. By the family of the code, `302.1` being `302`'s.
fn zoom_rule(code: &str, kind: SymbolKind) -> ZoomRule {
    let family = code.split('.').next().unwrap_or("");
    let number: u32 = family.parse().unwrap_or(0);
    let min_zoom = match number {
        // Water, marsh.
        301 => 11,
        302 | 304 | 306 | 307 => 12,
        308 => 13,
        // Vegetation and cultivation, the large areas.
        401..=415 => 12,
        416 => 13,
        // Roads, tracks, paths, railways.
        501..=503 | 509 | 511 => 12,
        504 | 505 | 510 | 512 => 13,
        // Areas that shall not be entered, buildings.
        520 => 12,
        521 | 522 => 13,
        // Contours, cliffs, stony and bare ground.
        102 => 12,
        101 | 201 | 210..=212 => 13,
        // The north lines of a map.
        601 => 12,
        _ => match kind {
            SymbolKind::Area | SymbolKind::Combined => 13,
            _ => 14,
        },
    };
    let (min_size_px, simplify_px) = match kind {
        SymbolKind::Area | SymbolKind::Combined => (4.0, 0.5),
        SymbolKind::Line => (8.0, 0.5),
        SymbolKind::Point | SymbolKind::Text => (0.0, 0.0),
    };
    ZoomRule {
        min_zoom,
        min_size_px,
        simplify_px,
    }
}

/// One symbol of a table.
#[derive(Clone, Debug)]
pub struct SymbolEntry {
    /// The symbol's standard code, e.g. "302.1".
    pub code: String,
    /// What the standard calls it.
    pub name: String,
    /// What it draws.
    pub kind: SymbolKind,
    /// How it is generalised for the coarser zoom levels.
    pub rule: ZoomRule,
}

/// How [`SymbolTable::closest`] found a symbol, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    /// The same code.
    Code,
    /// The code of the symbol this one is a variant of ("302" for "302.1").
    Parent,
    /// The same name, of a compatible kind.
    Name,
}

/// The symbols of a layer.
pub struct SymbolTable {
    /// The standard the table is, e.g. "ISOM 2017-2".
    pub standard: &'static str,
    entries: Vec<SymbolEntry>,
    by_code: HashMap<String, u16>,
}

impl SymbolTable {
    /// The ISOM 2017-2 table.
    pub fn isom() -> SymbolTable {
        let (map, _) = read_xml_map_str(ISOM).expect("the shipped symbol sets are well-formed");
        let mut entries = Vec::new();
        let mut by_code = HashMap::new();
        for symbol in &map.symbols {
            let code = symbol.code();
            // The first symbol with a code is the standard's; a later one with
            // the same code is a helper.
            if code.is_empty() || symbol.is_helper_symbol() || by_code.contains_key(code) {
                continue;
            }
            by_code.insert(code.to_string(), entries.len() as u16);
            let kind = SymbolKind::of(symbol);
            entries.push(SymbolEntry {
                code: code.to_string(),
                name: symbol.name().to_string(),
                kind,
                rule: zoom_rule(code, kind),
            });
        }
        SymbolTable {
            standard: "ISOM 2017-2",
            entries,
            by_code,
        }
    }

    /// How many symbols the table has.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table has no symbols.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The symbol with this id.
    pub fn get(&self, id: u16) -> Option<&SymbolEntry> {
        self.entries.get(usize::from(id))
    }

    /// The generalisation rule of the symbol with this id: the neutral one, which
    /// keeps everything from the finest zoom only, for an id the table lacks.
    pub fn rule(&self, id: u16) -> ZoomRule {
        self.get(id).map_or(
            ZoomRule {
                min_zoom: 14,
                min_size_px: 0.0,
                simplify_px: 0.0,
            },
            |e| e.rule,
        )
    }

    /// The id of the symbol with this code.
    pub fn id_of(&self, code: &str) -> Option<u16> {
        self.by_code.get(code).copied()
    }

    /// The symbol of this table that looks most like `symbol`, which is
    /// usually one of another edition or another standard. In order: the same
    /// code; the code of the symbol this is a variant of ("302.1" is a variant
    /// of "302"); the same name. Each must be of a compatible kind. `None`
    /// where nothing is close, and the object has to be dropped.
    pub fn closest(&self, symbol: &Symbol) -> Option<(u16, Match)> {
        let kind = SymbolKind::of(symbol);
        let fits = |id: u16| self.get(id).is_some_and(|e| e.kind.compatible(kind));

        let mut code = symbol.code();
        if let Some(id) = self.id_of(code).filter(|&id| fits(id)) {
            return Some((id, Match::Code));
        }
        while let Some(dot) = code.rfind('.') {
            code = &code[..dot];
            if let Some(id) = self.id_of(code).filter(|&id| fits(id)) {
                return Some((id, Match::Parent));
            }
        }
        let name = symbol.name();
        if !name.is_empty() {
            let found = self
                .entries
                .iter()
                .position(|e| e.kind.compatible(kind) && e.name.eq_ignore_ascii_case(name));
            if let Some(i) = found {
                return Some((i as u16, Match::Name));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::PointSymbol;

    #[test]
    fn the_isom_table_has_the_standards_symbols() {
        let table = SymbolTable::isom();
        assert!(table.len() > 100, "{} symbols", table.len());
        let building = table.id_of("521").expect("buildings are symbol 521");
        assert_eq!(table.get(building).unwrap().kind, SymbolKind::Area);
    }

    #[test]
    fn rules_put_the_large_and_the_important_first() {
        let table = SymbolTable::isom();
        let zoom = |code: &str| table.rule(table.id_of(code).unwrap()).min_zoom;
        // An index contour is read before a plain one, roads before paths, large
        // water before buildings, and a tree only at the finest zoom.
        assert!(zoom("102") < zoom("101"));
        assert!(zoom("503") < zoom("505"));
        assert!(zoom("301") < zoom("521"));
        assert_eq!(zoom("418"), 14);
        for id in 0..table.len() as u16 {
            let rule = table.rule(id);
            assert!((11..=14).contains(&rule.min_zoom), "{:?}", table.get(id));
            let kind = table.get(id).unwrap().kind;
            if matches!(kind, SymbolKind::Point | SymbolKind::Text) {
                assert_eq!((rule.min_size_px, rule.simplify_px), (0.0, 0.0));
            } else {
                assert!(rule.min_size_px > 0.0 && rule.simplify_px > 0.0);
            }
        }
    }

    #[test]
    fn a_variant_shares_the_rule_of_its_family() {
        assert_eq!(
            zoom_rule("301.1", SymbolKind::Area),
            zoom_rule("301", SymbolKind::Area)
        );
    }

    #[test]
    fn a_variant_falls_back_to_the_symbol_it_varies() {
        let table = SymbolTable::isom();
        let entry = table
            .entries
            .iter()
            .find(|e| e.kind == SymbolKind::Point && !e.code.contains('.'))
            .unwrap();
        let symbol = Symbol::Point(PointSymbol {
            code: format!("{}.77", entry.code),
            name: "no such name".into(),
            ..Default::default()
        });
        let (id, how) = table.closest(&symbol).unwrap();
        assert_eq!(table.get(id).unwrap().code, entry.code);
        assert_eq!(how, Match::Parent);
    }

    #[test]
    fn the_same_code_of_the_wrong_kind_is_not_a_match() {
        let table = SymbolTable::isom();
        // 521 is an area; a point symbol with that code and no known name
        // must not become a building.
        let symbol = Symbol::Point(PointSymbol {
            code: "521".into(),
            name: "no such name".into(),
            ..Default::default()
        });
        assert!(table.closest(&symbol).is_none());
    }

    #[test]
    fn something_unlike_everything_is_dropped() {
        let table = SymbolTable::isom();
        let symbol = Symbol::Point(PointSymbol {
            code: "9999".into(),
            name: "no such name".into(),
            ..Default::default()
        });
        assert!(table.closest(&symbol).is_none());
    }
}
