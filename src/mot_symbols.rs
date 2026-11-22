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
//! The tables are ISOM 2017-2 and ISSprOM 2019-2, each built from the reference
//! map the crate already ships; its ids are the order of that map's symbols.
//! This is provisional until the tables are frozen as files of their own.

use std::collections::HashMap;

use crate::map::{symbol_leaves, Map, Symbol};
use crate::osm_orienteering::Standard;
use crate::xml_reader::read_xml_map_str;

/// What a symbol draws.
///
/// A combined symbol is several symbols drawn over one object, and what it
/// draws is what its parts draw: an area if any part fills one (a paved area
/// with its border), a line otherwise (a railway's rails and sleepers). That,
/// and not how the symbol is built, is what says whether one symbol can stand
/// in for another: an open line given a symbol that fills would be drawn as
/// the polygon it outlines.
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
}

impl SymbolKind {
    /// What `symbol`, one of `map`'s, draws.
    pub fn of(symbol: &Symbol, map: &Map) -> SymbolKind {
        match symbol {
            Symbol::Point(_) => SymbolKind::Point,
            Symbol::Line(_) => SymbolKind::Line,
            Symbol::Area(_) => SymbolKind::Area,
            Symbol::Text(_) => SymbolKind::Text,
            Symbol::Combined(_) => {
                let mut leaves = Vec::new();
                symbol_leaves(symbol, map, &mut leaves);
                if leaves.iter().any(|leaf| matches!(leaf, Symbol::Area(_))) {
                    SymbolKind::Area
                } else {
                    SymbolKind::Line
                }
            }
        }
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
            SymbolKind::Area => 13,
            _ => 14,
        },
    };
    let (min_size_px, simplify_px) = match kind {
        SymbolKind::Area => (4.0, 0.5),
        SymbolKind::Line => (8.0, 0.5),
        SymbolKind::Point | SymbolKind::Text => (0.0, 0.0),
    };
    ZoomRule {
        min_zoom,
        min_size_px,
        simplify_px,
    }
}

/// Whether a symbol lays things out along the whole of a line: dashes, a
/// dashed border, or point symbols at its start, along it or at its end.
fn lays_out_along(symbol: &Symbol, map: &Map) -> bool {
    spaces_out(symbol, map, true)
}

/// [`lays_out_along`], with dashed borders counted or not.
pub(crate) fn spaces_out(symbol: &Symbol, map: &Map, borders: bool) -> bool {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    leaves.iter().any(|leaf| match leaf {
        Symbol::Line(s) => {
            let drawn = |p: &Option<Box<crate::map::PointSymbol>>| {
                p.as_deref().is_some_and(|p| !p.is_empty())
            };
            // `dash_length` and `break_length` are set on a solid line too, and mean
            // nothing there: it is `dashed` which says it is dashed.
            s.dashed
                || (borders && (s.border.dashed || s.right_border.dashed))
                || drawn(&s.start_symbol)
                || drawn(&s.mid_symbol)
                || drawn(&s.end_symbol)
                || drawn(&s.dash_symbol)
        }
        _ => false,
    })
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
    /// Whether an object of it is kept whole in a tile instead of cut: its
    /// symbol lays dashes or symbols out along the *whole* line, and how Mapper
    /// spaces them depends on that length, so a piece of the line would not
    /// meet its neighbour's.
    pub whole: bool,
    /// Whether it lays dashes or symbols out along the middle of the line: as
    /// [`whole`](Self::whole), dashed borders left out. A piece of such a
    /// line would have them anew; a piece of a road with dashed borders only
    /// is still the road.
    pub spaced: bool,
    /// The width of the widest line it draws, in millimetres; 0 for none. What
    /// tells a [`SymbolMap`] which of a road's or a stairway's footprints a
    /// symbol is.
    pub width: f64,
    /// The priority, in the standard's colour order, of the topmost colour its
    /// areas are filled with (lower is drawn on top); `None` for no plain fill.
    pub fill: Option<i32>,
    /// The priority of the topmost colour an area of it shows, its patterns'
    /// too (the dots of open land with scattered trees).
    pub top: Option<i32>,
    /// The priority of the topmost colour it paints solid, fill or line.
    pub paint: Option<i32>,
    /// For a line, which side of it the symbol draws on, in mm: positive to the
    /// right of the line's direction, 0 for both alike
    /// ([`crate::mot_adapt::line_side`]).
    pub side: f64,
    /// For a line, whether it paints its middle in a colour (a road's infill),
    /// not only borders or symbols along it (a stairway's outline and steps).
    pub body: bool,
    /// For a line, the colour its middle is painted in.
    pub body_rgb: Option<(f32, f32, f32)>,
    /// For a line, whether it draws an outline: a border on either side.
    pub edged: bool,
    /// For a plain area (one colour, no patterns), that colour.
    pub fill_rgb: Option<(f32, f32, f32)>,
    /// Whether it draws a line as well as what it fills: an area with its own
    /// outline (a building, a paved area with its border).
    pub outlined: bool,
}

/// The widest line `symbol`, one of `map`'s, draws, in millimetres.
pub(crate) fn widest_line(symbol: &Symbol, map: &Map) -> f64 {
    let mut leaves = Vec::new();
    symbol_leaves(symbol, map, &mut leaves);
    leaves
        .iter()
        .filter_map(|leaf| match leaf {
            Symbol::Line(s) => Some(s.line_width),
            _ => None,
        })
        .fold(0.0, f64::max)
}

/// How [`SymbolTable::closest`] found a symbol, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    /// The same code.
    Code,
    /// The code of the symbol this one is a variant of ("302" for "302.1").
    Parent,
    /// The same name, of the same kind.
    Name,
    /// A line of the table's [`SymbolMap`].
    Table,
}

/// A cross-reference table: which symbol of a standard each symbol of a
/// family of maps is, for maps whose codes mean something other than the
/// standard's (an older edition, an older standard, a club's own set). Read
/// from a text file:
///
/// ```text
/// # The symbols of our maps, in ISSprOM 2019-2
/// [ISSprOM 2019-2]
/// 501.2                501.2 512.3      # an area becomes 512.3, a line 501.2
/// 501.0.5 "Traffico"   501.18
/// 501.0.5              501.6 501.7 501.8 501.9
/// 602                  -
/// ```
///
/// A section names the standard of the lines that follow. A line is a code of
/// the maps' own, optionally a name in quotes, and the codes of the standard it
/// becomes, or `-` for a symbol to leave out. Of those codes, the ones that
/// draw the same [kind](SymbolKind) of thing count, and of them the one whose
/// widest line is closest in width to the map's symbol's: a road becomes the
/// footprint it is drawn at. Where none is of the right kind, the symbol is
/// matched as if the line were not there. A name says the line is for the
/// symbols of that code whose name starts with it (ignoring case), and comes
/// before a line for the code alone. A map drawn to the standard itself, as it
/// says, is matched by its codes only.
#[derive(Clone, Debug, Default)]
pub struct SymbolMap {
    rules: Vec<MapRule>,
}

#[derive(Clone, Debug)]
struct MapRule {
    /// The line of the file, for messages.
    line: usize,
    standard: String,
    code: String,
    /// Lower case.
    name: Option<String>,
    /// Empty for a symbol left out.
    targets: Vec<String>,
}

impl SymbolMap {
    /// Reads a table from the text of its file.
    pub fn parse(text: &str) -> Result<SymbolMap, String> {
        let standards = [Standard::Isom, Standard::Issprom].map(|s| s.name());
        let mut rules = Vec::new();
        let mut standard: Option<&str> = None;
        for (i, raw) in text.lines().enumerate() {
            let line = i + 1;
            let err = |what: &str| format!("line {line}: {what}");
            // A comment runs from a `#` outside quotes to the end of the line.
            let mut quoted = false;
            let end = raw
                .char_indices()
                .find(|&(_, c)| {
                    if c == '"' {
                        quoted = !quoted;
                    }
                    c == '#' && !quoted
                })
                .map_or(raw.len(), |(at, _)| at);
            let text = raw[..end].trim();
            if text.is_empty() {
                continue;
            }
            if let Some(section) = text.strip_prefix('[') {
                let name = section
                    .strip_suffix(']')
                    .ok_or_else(|| err("a section is a standard's name in brackets"))?
                    .trim();
                standard = Some(
                    standards
                        .into_iter()
                        .find(|s| s.eq_ignore_ascii_case(name))
                        .ok_or_else(|| {
                            err(&format!(
                                "{name:?} is not a standard (one of {})",
                                standards.join(", ")
                            ))
                        })?,
                );
                continue;
            }
            let standard = standard.ok_or_else(|| err("a line before any [standard] section"))?;
            let (code, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
            let mut rest = rest.trim_start();
            let mut name = None;
            if let Some(quoted) = rest.strip_prefix('"') {
                let (inside, after) = quoted
                    .split_once('"')
                    .ok_or_else(|| err("a name without its closing quote"))?;
                name = Some(inside.trim().to_lowercase());
                rest = after;
            }
            let targets: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
            let targets = match targets.as_slice() {
                [] => return Err(err("no symbol to become (or `-` to leave it out)")),
                [dash] if dash == "-" => Vec::new(),
                _ if targets.iter().any(|t| t == "-") => {
                    return Err(err("`-` is a line's only target"))
                }
                _ => targets,
            };
            rules.push(MapRule {
                line,
                standard: standard.to_string(),
                code: code.to_string(),
                name,
                targets,
            });
        }
        Ok(SymbolMap { rules })
    }

    /// Whether the table has no lines.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// The symbols of a layer.
pub struct SymbolTable {
    /// The standard the table is, e.g. "ISOM 2017-2".
    pub standard: &'static str,
    which: Standard,
    entries: Vec<SymbolEntry>,
    by_code: HashMap<String, u16>,
    crossref: Vec<CrossRule>,
}

/// A line of a [`SymbolMap`], for one table.
struct CrossRule {
    line: usize,
    code: String,
    name: Option<String>,
    /// Ids of the table; empty for a symbol left out.
    targets: Vec<u16>,
}

impl SymbolTable {
    /// The ISOM 2017-2 table.
    pub fn isom() -> SymbolTable {
        SymbolTable::new(Standard::Isom)
    }

    /// The ISSprOM 2019-2 table.
    pub fn issprom() -> SymbolTable {
        SymbolTable::new(Standard::Issprom)
    }

    /// The table of a standard.
    pub fn new(which: Standard) -> SymbolTable {
        let (map, _) =
            read_xml_map_str(which.reference()).expect("the shipped symbol sets are well-formed");
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
            let kind = SymbolKind::of(symbol, &map);
            entries.push(SymbolEntry {
                code: code.to_string(),
                name: symbol.name().to_string(),
                kind,
                rule: zoom_rule(code, kind),
                whole: lays_out_along(symbol, &map),
                spaced: spaces_out(symbol, &map, false),
                width: widest_line(symbol, &map),
                fill: crate::mot_cut::fill_priority(symbol, &map),
                top: crate::mot_cut::top_priority(symbol, &map),
                paint: crate::mot_cut::paint_priority(symbol, &map),
                side: if kind == SymbolKind::Line {
                    crate::mot_adapt::line_side(symbol, &map)
                } else {
                    0.0
                },
                body: crate::mot_adapt::body(symbol, &map).is_some(),
                body_rgb: crate::mot_adapt::body(symbol, &map)
                    .and_then(|(color, _, _)| map.color(color))
                    .map(|c| c.rgb),
                edged: crate::mot_adapt::edged(symbol, &map),
                fill_rgb: match symbol {
                    Symbol::Area(a) if a.color >= 0 && a.patterns.is_empty() => {
                        map.color(a.color).map(|c| c.rgb)
                    }
                    _ => None,
                },
                outlined: kind == SymbolKind::Area && crate::mot_cut::draws_a_line(symbol, &map),
            });
        }
        SymbolTable {
            standard: which.name(),
            which,
            entries,
            by_code,
            crossref: Vec::new(),
        }
    }

    /// The table, matching a map's symbols by the lines of `symbols` for this
    /// standard before anything else. An error names a line whose code is not
    /// one of the standard's.
    pub fn with_symbol_map(mut self, symbols: &SymbolMap) -> Result<SymbolTable, String> {
        for rule in symbols.rules.iter().filter(|r| r.standard == self.standard) {
            let targets = rule
                .targets
                .iter()
                .map(|code| {
                    self.id_of(code).ok_or_else(|| {
                        format!(
                            "line {}: {code} is not a symbol of {}",
                            rule.line, self.standard
                        )
                    })
                })
                .collect::<Result<Vec<u16>, String>>()?;
            self.crossref.push(CrossRule {
                line: rule.line,
                code: rule.code.clone(),
                name: rule.name.clone(),
                targets,
            });
        }
        Ok(self)
    }

    /// The line of the [`SymbolMap`] for `symbol`, one of `map`'s.
    fn crossref_for(&self, symbol: &Symbol, map: &Map) -> Option<&CrossRule> {
        let own = map
            .symbol_set
            .as_deref()
            .is_some_and(|set| set.trim().eq_ignore_ascii_case(self.standard));
        if own {
            return None;
        }
        let code = symbol.code();
        let name = symbol.name().trim().to_lowercase();
        let of_code = || self.crossref.iter().filter(move |r| r.code == code);
        of_code()
            .find(|r| r.name.as_deref().is_some_and(|n| name.starts_with(n)))
            .or_else(|| of_code().find(|r| r.name.is_none()))
    }

    /// Whether the table's [`SymbolMap`] says to leave `symbol`, one of `map`'s,
    /// out.
    pub fn left_out(&self, symbol: &Symbol, map: &Map) -> bool {
        self.crossref_for(symbol, map)
            .is_some_and(|r| r.targets.is_empty())
    }

    /// The line of the [`SymbolMap`] that matched `symbol`, for messages.
    pub fn crossref_line(&self, symbol: &Symbol, map: &Map) -> Option<usize> {
        self.crossref_for(symbol, map).map(|r| r.line)
    }

    /// The plain area of the table (one colour, no patterns) whose colour is
    /// closest to `rgb`, where one is close: what fills a middle which a symbol
    /// of the table leaves empty and a map's own symbol paints in that colour.
    pub fn closest_plain_area(&self, rgb: (f32, f32, f32)) -> Option<u16> {
        // Out of 1 in each of red, green and blue: about as far as one tint of a
        // standard's colour is from the next.
        const CLOSE: f32 = 0.1;
        let distance = |c: (f32, f32, f32)| {
            ((c.0 - rgb.0).powi(2) + (c.1 - rgb.1).powi(2) + (c.2 - rgb.2).powi(2)).sqrt()
        };
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind == SymbolKind::Area)
            .filter_map(|(id, e)| Some((id, distance(e.fill_rgb?))))
            .filter(|&(_, d)| d <= CLOSE)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id as u16)
    }

    /// The table with this name ("ISOM 2017-2", "ISSprOM 2019-2"), as an archive
    /// records the layer it holds.
    pub fn by_name(name: &str) -> Option<SymbolTable> {
        [Standard::Isom, Standard::Issprom]
            .into_iter()
            .find(|s| s.name() == name)
            .map(SymbolTable::new)
    }

    /// The map the table's symbols are defined in, as the text of an `.omap`
    /// file: what a renderer draws the symbols from.
    pub fn reference(&self) -> &'static str {
        self.which.reference()
    }

    /// Which standard a map is drawn to. What the map says it is, where it says;
    /// otherwise the table whose symbols its objects use: a symbol with the code
    /// of a table's counts for it, and again if the name is the same too, by the
    /// number of objects it has. Failing that, by scale: 1:6000 or larger is
    /// sprint.
    pub fn detect(map: &Map) -> Standard {
        if let Some(set) = &map.symbol_set {
            let set = set.to_ascii_lowercase();
            if set.contains("issprom") {
                return Standard::Issprom;
            }
            if set.contains("isom") {
                return Standard::Isom;
            }
        }
        let mut objects = vec![0usize; map.symbols.len()];
        for o in &map.objects {
            if let Some(i) = o.symbol_index {
                objects[i] += 1;
            }
        }
        let score = |table: &SymbolTable| -> usize {
            let mut total = 0;
            for (i, symbol) in map.symbols.iter().enumerate() {
                if objects[i] == 0 || !symbol.is_visible() {
                    continue;
                }
                let kind = SymbolKind::of(symbol, map);
                let Some(entry) = table.id_of(symbol.code()).and_then(|id| table.get(id)) else {
                    continue;
                };
                if entry.kind != kind {
                    continue;
                }
                let same_name = entry.name.eq_ignore_ascii_case(symbol.name());
                total += objects[i] * (1 + usize::from(same_name));
            }
            total
        };
        let (isom, issprom) = (score(&SymbolTable::isom()), score(&SymbolTable::issprom()));
        match isom.cmp(&issprom) {
            std::cmp::Ordering::Greater => Standard::Isom,
            std::cmp::Ordering::Less => Standard::Issprom,
            std::cmp::Ordering::Equal
                if map.scale_denominator > 0 && map.scale_denominator <= 6000 =>
            {
                Standard::Issprom
            }
            std::cmp::Ordering::Equal => Standard::Isom,
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

    /// The coarsest zoom any symbol appears at: above it a tile has no objects,
    /// only coverage.
    pub fn min_object_zoom(&self) -> u8 {
        self.entries
            .iter()
            .map(|e| e.rule.min_zoom)
            .min()
            .unwrap_or(14)
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

    /// The symbol of this table that looks most like `symbol`, one of `map`'s,
    /// which is usually of another edition or another standard. In order: what
    /// the table's [`SymbolMap`] says; the same code; the code of the symbol
    /// this is a variant of ("302.1" is a variant of "302"); the same name.
    /// Each must draw the same [kind](SymbolKind) of thing. `None` where
    /// nothing is close, or the symbol map leaves it out, and the object has to
    /// be dropped.
    pub fn closest(&self, symbol: &Symbol, map: &Map) -> Option<(u16, Match)> {
        let kind = SymbolKind::of(symbol, map);
        let fits = |id: u16| self.get(id).is_some_and(|e| e.kind == kind);

        if let Some(rule) = self.crossref_for(symbol, map) {
            if rule.targets.is_empty() {
                return None;
            }
            let width = widest_line(symbol, map);
            let nearest = rule
                .targets
                .iter()
                .copied()
                .filter(|&id| fits(id))
                .min_by(|&a, &b| {
                    let off = |id: u16| (self.entries[usize::from(id)].width - width).abs();
                    off(a).total_cmp(&off(b))
                });
            if let Some(id) = nearest {
                return Some((id, Match::Table));
            }
        }

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
                .position(|e| e.kind == kind && e.name.eq_ignore_ascii_case(name));
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
    use crate::map::{AreaSymbol, CombinedSymbol, LineSymbol, PartRef, PointSymbol};

    #[test]
    fn the_isom_table_has_the_standards_symbols() {
        let table = SymbolTable::isom();
        assert!(table.len() > 100, "{} symbols", table.len());
        let building = table.id_of("521").expect("buildings are symbol 521");
        assert_eq!(table.get(building).unwrap().kind, SymbolKind::Area);
    }

    #[test]
    fn a_table_is_found_by_its_name_and_has_its_own_symbols() {
        for name in ["ISOM 2017-2", "ISSprOM 2019-2"] {
            let table = SymbolTable::by_name(name).unwrap();
            assert_eq!(table.standard, name);
            assert!(table.len() > 100);
        }
        assert!(SymbolTable::by_name("ISOM 2000").is_none());
        assert_ne!(
            SymbolTable::isom().reference().len(),
            SymbolTable::issprom().reference().len()
        );
    }

    #[test]
    fn the_standard_of_a_map_is_what_it_says_or_what_its_symbols_are() {
        let (mut map, _) = read_xml_map_str(SymbolTable::issprom().reference()).unwrap();
        map.symbol_set = Some("ISSprOM 2019-2".into());
        assert_eq!(SymbolTable::detect(&map), Standard::Issprom);
        map.symbol_set = Some("ISOM 2017-2".into());
        assert_eq!(SymbolTable::detect(&map), Standard::Isom);
        // Saying nothing, it is the symbols the objects use which decide, and not
        // the scale.
        map.symbol_set = None;
        map.scale_denominator = 15000;
        if !map.objects.is_empty() {
            assert_eq!(SymbolTable::detect(&map), Standard::Issprom);
        }
        // With no objects either, it is the scale.
        map.objects.clear();
        map.scale_denominator = 4000;
        assert_eq!(SymbolTable::detect(&map), Standard::Issprom);
        map.scale_denominator = 15000;
        assert_eq!(SymbolTable::detect(&map), Standard::Isom);
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
    fn dashed_lines_and_lines_with_symbols_along_them_are_kept_whole() {
        let table = SymbolTable::isom();
        let whole = |code: &str| table.get(table.id_of(code).unwrap()).unwrap().whole;
        // A contour is a plain line; a power line has its pylons along it, a
        // cliff its ticks, and a fence its dashes.
        assert!(!whole("101"));
        assert!(whole("510"));
        let wholes = (0..table.len() as u16)
            .filter(|&i| table.get(i).unwrap().whole)
            .count();
        assert!(wholes > 5 && wholes < table.len() / 2, "{wholes}");
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
        let (id, how) = table.closest(&symbol, &Map::default()).unwrap();
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
        assert!(table.closest(&symbol, &Map::default()).is_none());
    }

    #[test]
    fn something_unlike_everything_is_dropped() {
        let table = SymbolTable::isom();
        let symbol = Symbol::Point(PointSymbol {
            code: "9999".into(),
            name: "no such name".into(),
            ..Default::default()
        });
        assert!(table.closest(&symbol, &Map::default()).is_none());
    }

    #[test]
    fn a_combined_symbol_draws_an_area_if_any_part_fills_one() {
        // A paved area with its border: an area and a line.
        let mut map = Map::default();
        map.symbols.push(Symbol::Line(LineSymbol::default()));
        let paved = Symbol::Combined(CombinedSymbol {
            parts: vec![PartRef::Shared(0), PartRef::Private(0)],
            owned_parts: vec![Symbol::Area(AreaSymbol::default())],
            ..Default::default()
        });
        assert_eq!(SymbolKind::of(&paved, &map), SymbolKind::Area);
        // Rails and sleepers: lines only.
        let railway = Symbol::Combined(CombinedSymbol {
            parts: vec![PartRef::Shared(0), PartRef::Private(0)],
            owned_parts: vec![Symbol::Line(LineSymbol::default())],
            ..Default::default()
        });
        assert_eq!(SymbolKind::of(&railway, &map), SymbolKind::Line);
    }

    #[test]
    fn a_line_never_becomes_a_symbol_that_fills() {
        // ISSprOM's 501 is a paved area with its border, a combined symbol. A
        // map's own road line coded as a variant of it ("501.0.5") must not be
        // stored as 501, or each road would be drawn as the polygon it outlines.
        let table = SymbolTable::issprom();
        let paved = table.get(table.id_of("501").unwrap()).unwrap();
        assert_eq!(paved.kind, SymbolKind::Area);
        let road = Symbol::Line(LineSymbol {
            code: "501.0.5".into(),
            name: "Traffico".into(),
            ..Default::default()
        });
        let found = table.closest(&road, &Map::default());
        assert!(
            found.is_none_or(|(id, _)| table.get(id).unwrap().kind == SymbolKind::Line),
            "{:?}",
            found.map(|(id, _)| table.get(id).unwrap())
        );
        // An area of the same family still is one.
        let area = Symbol::Area(AreaSymbol {
            code: "501.0.1".into(),
            name: "Paved area, urban".into(),
            ..Default::default()
        });
        let (id, how) = table.closest(&area, &Map::default()).unwrap();
        assert_eq!(
            (table.get(id).unwrap().code.as_str(), how),
            ("501", Match::Parent)
        );
    }

    fn line(code: &str, name: &str, width: f64) -> Symbol {
        Symbol::Line(LineSymbol {
            code: code.into(),
            name: name.into(),
            line_width: width,
            ..Default::default()
        })
    }

    fn area(code: &str, name: &str) -> Symbol {
        Symbol::Area(AreaSymbol {
            code: code.into(),
            name: name.into(),
            ..Default::default()
        })
    }

    const CLUB: &str = "
        # a club's set
        [ISSprOM 2019-2]
        501.0.5 \"Traffico\"  501.16 501.17 501.18 501.19
        501.0.5             501.6 501.7 501.8 501.9   # by width
        501.2               512.3
        602                 -
        708                 710.1
        [ISOM 2017-2]
        501.0.5             501
    ";

    fn club_table() -> SymbolTable {
        SymbolTable::issprom()
            .with_symbol_map(&SymbolMap::parse(CLUB).unwrap())
            .unwrap()
    }

    fn code_of(table: &SymbolTable, symbol: &Symbol, map: &Map) -> Option<(String, Match)> {
        table
            .closest(symbol, map)
            .map(|(id, how)| (table.get(id).unwrap().code.clone(), how))
    }

    #[test]
    fn a_symbol_map_picks_the_footprint_closest_in_width() {
        let table = club_table();
        let map = Map::default();
        for (width, code) in [
            (0.35, "501.6"),
            (0.45, "501.7"),
            (0.7, "501.8"),
            (1.4, "501.9"),
        ] {
            let road = line("501.0.5", "Paved road, urban", width);
            assert_eq!(
                code_of(&table, &road, &map),
                Some((code.to_string(), Match::Table)),
                "{width}"
            );
        }
    }

    #[test]
    fn a_line_with_a_name_comes_before_the_code_alone() {
        let table = club_table();
        let road = line("501.0.5", "traffico intenso", 0.7);
        assert_eq!(
            code_of(&table, &road, &Map::default()),
            Some(("501.18".to_string(), Match::Table))
        );
    }

    #[test]
    fn a_symbol_map_line_of_another_kind_falls_back_to_the_code() {
        let table = club_table();
        let map = Map::default();
        // An area 501.2 is the old multilevel; there is no line among the
        // targets, so a line 501.2 is the standard's own.
        assert_eq!(
            code_of(&table, &area("501.2", "Multilevel"), &map),
            Some(("512.3".to_string(), Match::Table))
        );
        assert_eq!(
            code_of(&table, &line("501.2", "Step", 0.1), &map),
            Some(("501.2".to_string(), Match::Code))
        );
        assert_eq!(
            code_of(&table, &line("708", "Uncrossable boundary", 1.0), &map),
            Some(("708".to_string(), Match::Code))
        );
    }

    #[test]
    fn a_symbol_map_can_leave_a_symbol_out() {
        let table = club_table();
        let mark = Symbol::Point(PointSymbol {
            code: "602".into(),
            name: "Registration mark".into(),
            ..Default::default()
        });
        assert!(table.closest(&mark, &Map::default()).is_none());
        assert!(table.left_out(&mark, &Map::default()));
        assert!(!table.left_out(&area("501.2", "Multilevel"), &Map::default()));
    }

    #[test]
    fn a_map_drawn_to_the_standard_is_matched_by_its_codes() {
        let table = club_table();
        let map = Map {
            symbol_set: Some("ISSprOM 2019-2".into()),
            ..Default::default()
        };
        assert_eq!(
            code_of(&table, &area("501.2", "Multilevel"), &map),
            Some(("501".to_string(), Match::Parent))
        );
    }

    #[test]
    fn a_symbol_map_section_is_for_its_standard_only() {
        let isom = SymbolTable::isom()
            .with_symbol_map(&SymbolMap::parse(CLUB).unwrap())
            .unwrap();
        let road = line("501.0.5", "Traffico", 0.7);
        assert_eq!(
            code_of(&isom, &road, &Map::default()).map(|(c, _)| c),
            isom.closest(&road, &Map::default())
                .map(|(id, _)| isom.get(id).unwrap().code.clone())
        );
        assert_ne!(
            code_of(&isom, &road, &Map::default()).map(|(_, how)| how),
            Some(Match::Table),
            "ISOM's 501 is an area, not a line"
        );
    }

    #[test]
    fn a_short_line_still_shows_its_dots_or_tags() {
        // Or a short passable wall is a plain line, a cliff its top line, an
        // uncrossable fence a wall.
        let (map, _) = read_xml_map_str(Standard::Issprom.reference()).unwrap();
        for code in ["104", "104.1", "201", "202.1", "513.1", "518", "799"] {
            let symbol = map.symbols.iter().find(|s| s.code() == code).unwrap();
            let Symbol::Line(line) = symbol else {
                panic!("{code} is a line")
            };
            assert!(line.show_at_least_one_symbol, "{code}");
        }
    }

    #[test]
    fn a_small_fountain_turns_as_it_stands() {
        // No north arrow beside 311 in ISSprOM 2019-2, unlike 313.
        let (map, _) = read_xml_map_str(Standard::Issprom.reference()).unwrap();
        let point = |code: &str| match map.symbols.iter().find(|s| s.code() == code) {
            Some(Symbol::Point(p)) => p.is_rotatable,
            _ => panic!("{code} is a point"),
        };
        assert!(point("311"));
        assert!(!point("313"));
    }

    #[test]
    fn a_bad_symbol_map_says_which_line() {
        for (text, says) in [
            ("501 501", "line 1: a line before any [standard] section"),
            (
                "[ISSprOM 2019]\n",
                "line 1: \"ISSprOM 2019\" is not a standard",
            ),
            ("[ISSprOM 2019-2]\n\n501.0.5", "line 3: no symbol to become"),
            (
                "[ISSprOM 2019-2]\n501 \"x 501",
                "line 2: a name without its closing quote",
            ),
            (
                "[ISSprOM 2019-2]\n501 - 501.5",
                "line 2: `-` is a line's only target",
            ),
        ] {
            let err = SymbolMap::parse(text).unwrap_err();
            assert!(err.starts_with(says), "{text:?}: {err}");
        }
        let unknown = SymbolMap::parse("[ISSprOM 2019-2]\n# none\n501.0.5 999.9").unwrap();
        let err = SymbolTable::issprom()
            .with_symbol_map(&unknown)
            .err()
            .unwrap();
        assert_eq!(err, "line 3: 999.9 is not a symbol of ISSprOM 2019-2");
    }
}
