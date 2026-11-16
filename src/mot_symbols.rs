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

/// One symbol of a table.
#[derive(Clone, Debug)]
pub struct SymbolEntry {
    /// The symbol's standard code, e.g. "302.1".
    pub code: String,
    /// What the standard calls it.
    pub name: String,
    /// What it draws.
    pub kind: SymbolKind,
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
            entries.push(SymbolEntry {
                code: code.to_string(),
                name: symbol.name().to_string(),
                kind: SymbolKind::of(symbol),
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
