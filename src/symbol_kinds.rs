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
//!
//! # What shows up over what
//!
//! Sorting the set is half of what a generator needs. The other half is
//! which of those symbols can be drawn *over* which: a map's drawing order
//! is its colour table, from the last colour up to the first, so a symbol
//! whose every colour sits below the colour a piece of ground is filled with
//! is a symbol which is there in the file and nowhere in the picture. A
//! generated map full of those would be a map which claims to exercise a
//! symbol it never draws.
//!
//! [`Overlays`] is that question answered once, for the two kinds a
//! generator drops onto filled ground — the transparent areas and the point
//! symbols — against every opaque area the set holds.

use crate::map::{fill_pattern_type, is_color, FillPattern, Map, PointSymbol, Symbol};

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
    /// Whether an object drawn with this symbol can be given an angle of its
    /// own: an area which fills with a pattern that turns, or a point symbol
    /// which says it is rotatable. A line runs where its path runs, so it
    /// never turns, and a combined symbol is left alone — the file carries
    /// no `rotatable` of its own for one.
    pub turns: bool,
    /// How many turns of this symbol look exactly alike — see
    /// [`pattern_symmetry`], which is where it comes from and what it is for.
    ///
    /// One for a symbol whose every angle draws something different, and for
    /// every symbol which does not turn at all.
    pub symmetry: u32,
}

/// A symbol set sorted into the five kinds.
///
/// Hidden and helper symbols are left out of all five: they draw nothing, and
/// a generated map which used one would come out with an empty cell and no
/// way to tell that apart from a rendering bug. So is the OpenOrienteering
/// logo, see [`is_the_mapper_logo`].
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
            if !symbol.is_visible() || is_the_mapper_logo(symbol) {
                continue;
            }
            let kind = kind_of(map, symbol);
            let entry = Entry {
                index,
                id: map.symbol_ids.get(index).copied().unwrap_or(-1),
                code: code_of(symbol).to_string(),
                name: name_of(symbol).to_string(),
                turns: match kind {
                    SymbolKind::OpaqueArea | SymbolKind::TransparentArea => {
                        has_rotatable_pattern(map, symbol)
                    }
                    SymbolKind::Point => matches!(symbol, Symbol::Point(p) if p.is_rotatable),
                    SymbolKind::Line | SymbolKind::Text => false,
                },
                symmetry: match kind {
                    SymbolKind::OpaqueArea | SymbolKind::TransparentArea => {
                        pattern_symmetry(map, symbol)
                    }
                    // A point symbol draws whatever it draws and this says
                    // nothing about it; a line and a text do not turn at all.
                    _ => 1,
                },
            };
            match kind {
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

/// Which overlays show up over which ground, for a whole symbol set.
///
/// Both lists are indexed by an opaque area's place in
/// [`Catalogue::opaque_areas`] — the ground a cell is filled with — and hold
/// the places in [`Catalogue::transparent_areas`] and [`Catalogue::points`]
/// of the symbols which can be drawn over it and still be seen. A generator
/// which picks out of those lists cannot draw an invisible object.
#[derive(Debug, Default)]
pub struct Overlays {
    /// For each opaque area, the transparent areas which show up on it.
    pub transparent_areas: Vec<Vec<usize>>,
    /// For each opaque area, the point symbols which show up on it.
    pub points: Vec<Vec<usize>>,
}

impl Overlays {
    /// Every pair of an opaque area with something drawn over it, tried.
    pub fn of(map: &Map, catalogue: &Catalogue) -> Overlays {
        let over_each = |overlays: &[Entry]| -> Vec<Vec<usize>> {
            catalogue
                .opaque_areas
                .iter()
                .map(|ground| {
                    overlays
                        .iter()
                        .enumerate()
                        .filter(|(_, overlay)| {
                            shows_over(map, &map.symbols[overlay.index], &map.symbols[ground.index])
                        })
                        .map(|(at, _)| at)
                        .collect()
                })
                .collect()
        };
        Overlays {
            transparent_areas: over_each(&catalogue.transparent_areas),
            points: over_each(&catalogue.points),
        }
    }

    /// How many of the transparent-area-over-ground pairs show up, of how
    /// many were tried: every transparent area against every opaque one.
    pub fn transparent_pairs(&self, catalogue: &Catalogue) -> (usize, usize) {
        (
            self.transparent_areas.iter().map(Vec::len).sum(),
            self.transparent_areas.len() * catalogue.transparent_areas.len(),
        )
    }

    /// The same for the point symbols.
    pub fn point_pairs(&self, catalogue: &Catalogue) -> (usize, usize) {
        (
            self.points.iter().map(Vec::len).sum(),
            self.points.len() * catalogue.points.len(),
        )
    }
}

/// Whether `overlay` drawn over `ground` shows up at all.
///
/// A map is drawn from the last colour of its table to the first, so a
/// colour hides every colour after it in the table. What a piece of ground
/// hides is decided by the one colour which covers it completely — the
/// opaque fill which makes it an opaque area in the first place — and an
/// overlay is seen when it draws any colour above that one. An overlay whose
/// every colour is below it is drawn into the file and covered up by the
/// ground it was drawn on.
pub fn shows_over(map: &Map, overlay: &Symbol, ground: &Symbol) -> bool {
    let colors = colors_of(map, overlay);
    match covering_color(map, ground, 0) {
        // Nothing about the ground covers anything, so whatever the overlay
        // draws is there to be seen.
        None => !colors.is_empty(),
        Some(covering) => colors.iter().any(|&color| color < covering),
    }
}

/// Every colour the symbol draws with, itself and everything it is built out
/// of, as priorities into [`Map::colors`].
///
/// Only the colours which reach the paper: a stroke of no width, a dot of no
/// radius and a colour the table gives no opacity draw nothing, and a
/// generator which counted them would think an overlay visible which is not.
pub fn colors_of(map: &Map, symbol: &Symbol) -> Vec<i32> {
    let mut colors = Vec::new();
    collect_colors(map, symbol, 0, &mut colors);
    colors
}

/// The colour which hides what is under the symbol: the topmost fully opaque
/// area fill it draws, or `None` for a symbol which hides nothing.
fn covering_color(map: &Map, symbol: &Symbol, depth: usize) -> Option<i32> {
    match symbol {
        // An area with no fill colour is a pattern over bare ground, and one
        // whose colour is not fully opaque is blended with what it covers;
        // either way, something under it shows through.
        Symbol::Area(area) => is_color(area.color)
            .then_some(area.color)
            .filter(|&color| map.color(color).is_some_and(|c| c.opacity >= 1.0)),
        Symbol::Combined(combined) if depth < MAX_PART_DEPTH => map
            .parts(combined)
            .iter()
            // The topmost of them, which is the one that hides the most.
            .filter_map(|part| covering_color(map, part, depth + 1))
            .min(),
        _ => None,
    }
}

fn collect_colors(map: &Map, symbol: &Symbol, depth: usize, into: &mut Vec<i32>) {
    if depth > MAX_PART_DEPTH {
        return;
    }
    match symbol {
        Symbol::Point(point) => collect_point_colors(map, point, depth, into),
        Symbol::Line(line) => {
            if line.line_width > 0.0 {
                push_color(map, line.color, into);
            }
            for border in [&line.border, &line.right_border] {
                if border.width > 0.0 {
                    push_color(map, border.color, into);
                }
            }
            for point in [
                &line.start_symbol,
                &line.mid_symbol,
                &line.end_symbol,
                &line.dash_symbol,
            ]
            .into_iter()
            .flatten()
            {
                collect_point_colors(map, point, depth + 1, into);
            }
        }
        Symbol::Area(area) => {
            push_color(map, area.color, into);
            for pattern in &area.patterns {
                if pattern.pattern_type == fill_pattern_type::POINT {
                    if let Some(point) = &pattern.point {
                        collect_point_colors(map, point, depth + 1, into);
                    }
                } else if pattern.line_width > 0.0 {
                    push_color(map, pattern.line_color, into);
                }
            }
        }
        Symbol::Text(text) => {
            push_color(map, text.color, into);
            if text.framing {
                push_color(map, text.framing_color, into);
            }
        }
        Symbol::Combined(combined) => {
            for part in map.parts(combined) {
                collect_colors(map, part, depth + 1, into);
            }
        }
    }
}

fn collect_point_colors(map: &Map, point: &PointSymbol, depth: usize, into: &mut Vec<i32>) {
    if depth > MAX_PART_DEPTH {
        return;
    }
    if point.inner_radius > 0.0 {
        push_color(map, point.inner_color, into);
    }
    if point.outer_width > 0.0 {
        push_color(map, point.outer_color, into);
    }
    for element in &point.elements {
        collect_colors(map, &element.symbol, depth + 1, into);
    }
}

/// Keeps a colour which the table has something to draw with.
fn push_color(map: &Map, color: i32, into: &mut Vec<i32>) {
    if is_color(color) && map.color(color).is_some_and(|c| c.opacity > 0.0) {
        into.push(color);
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
/// it, which is exactly the colour [`covering_color`] looks for.
fn area_kind(map: &Map, symbol: &Symbol, depth: usize) -> SymbolKind {
    if covering_color(map, symbol, depth).is_some() {
        SymbolKind::OpaqueArea
    } else {
        SymbolKind::TransparentArea
    }
}

/// Whether the symbol fills an area with a pattern which turns with the
/// object it is drawn on.
///
/// A pattern says for itself whether it turns; one which does not keeps the
/// angle the symbol set gave it however the object is rotated, which is why
/// a generator has to ask before it bothers turning anything.
pub fn has_rotatable_pattern(map: &Map, symbol: &Symbol) -> bool {
    contains(map, symbol, 0, &|part| match part {
        Symbol::Area(area) => area.patterns.iter().any(|pattern| pattern.rotatable),
        _ => false,
    })
}

/// How many turns of a symbol's fill look exactly alike: the order `n` of its
/// rotational symmetry, so that turning it by `1/n` of a whole turn draws the
/// same picture.
///
/// This is a question nothing about drawing a map needs to ask, and one
/// anything *reading* a map back has to. A regular grid of round dots turned
/// by a quarter turn is the same grid: four different angles draw one
/// picture, so a picture cannot say which of the four it was. Asked to
/// predict the angle from the picture anyway, the best a network can do is
/// answer the average of the four — which for four angles a quarter turn
/// apart is nothing at all, and scores exactly as well as guessing. Folding
/// the angle by `n` before it is ever asked for is what turns four
/// unanswerable questions into one answerable one; see
/// [`crate::ground_truth::GroundTruth::sin_cos_folded`].
///
/// Only the patterns which **turn** count. One which does not is drawn at the
/// angle the symbol set gave it however the object is rotated, so it looks
/// the same at every angle and says nothing about any of them.
///
/// What each kind of pattern comes to:
///
/// * Parallel lines are the same lines upside down, and nothing better:
///   **two**.
/// * A grid of points is **four** where the grid is square — the spacing
///   between the lines equal to the spacing along them, and the lines not
///   staggered — and **two** otherwise. A staggered grid is left at two
///   rather than worked out: a hexagonal one is really six, and answering
///   two where the truth is six costs a little of what could have been
///   learned, where answering six where the truth is two would fold together
///   angles which really do look different.
/// * That grid is only as symmetric as the thing standing on it, so a point
///   symbol which draws more than a dot and a ring — both of which look the
///   same at every angle — takes the whole thing down to **one**. What it
///   draws is anybody's guess, and guessing high is the one mistake here
///   which cannot be recovered from.
///
/// Several turning patterns over one another are all turned by the same
/// rotation about the same point, so what survives is what survives in all of
/// them: the greatest common divisor.
///
/// One, therefore, for a symbol this cannot make a case about — which is the
/// answer that folds nothing and leaves the angle exactly as it was.
pub fn pattern_symmetry(map: &Map, symbol: &Symbol) -> u32 {
    let mut order: Option<u32> = None;
    turning_patterns(map, symbol, 0, &mut |pattern| {
        let one = symmetry_of_pattern(pattern);
        order = Some(match order {
            Some(so_far) => greatest_common_divisor(so_far, one),
            None => one,
        });
    });
    order.unwrap_or(1).max(1)
}

/// Calls `found` for every pattern of the symbol, or of any symbol it is
/// built out of, which turns with the object it is drawn on.
fn turning_patterns(map: &Map, symbol: &Symbol, depth: usize, found: &mut dyn FnMut(&FillPattern)) {
    match symbol {
        Symbol::Area(area) => {
            for pattern in area.patterns.iter().filter(|pattern| pattern.rotatable) {
                found(pattern);
            }
        }
        Symbol::Combined(combined) if depth < MAX_PART_DEPTH => {
            for part in map.parts(combined) {
                turning_patterns(map, part, depth + 1, found);
            }
        }
        _ => {}
    }
}

/// The order of one pattern's rotational symmetry — see [`pattern_symmetry`],
/// which is where the reasoning is.
fn symmetry_of_pattern(pattern: &FillPattern) -> u32 {
    match pattern.pattern_type {
        fill_pattern_type::LINE => 2,
        fill_pattern_type::POINT => {
            let lattice = point_lattice_symmetry(pattern);
            match pattern.point.as_deref().map(point_symmetry) {
                // A dot, a ring, or both: round, and so no constraint of its
                // own on what the grid under it allows.
                Some(None) => lattice,
                Some(Some(glyph)) => greatest_common_divisor(lattice, glyph),
                // A point pattern with no point symbol draws nothing.
                None => 1,
            }
        }
        _ => 1,
    }
}

/// How symmetric the grid a point pattern stands its symbols on is.
fn point_lattice_symmetry(pattern: &FillPattern) -> u32 {
    // The spacings are lengths a file wrote down, so they are compared with a
    // tolerance rather than for equality: a square grid written as two
    // roundings of one number is still a square grid.
    const CLOSE: f64 = 1e-6;

    let (across, along) = (pattern.line_spacing, pattern.point_distance);
    if across <= 0.0 || along <= 0.0 {
        return 1;
    }
    // Successive lines shifted along themselves: the grid is no longer
    // rectangular, and a half turn is all this claims for it.
    if pattern.offset_along_line.abs() > CLOSE * along {
        return 2;
    }
    if (across - along).abs() <= CLOSE * across.max(along) {
        4
    } else {
        2
    }
}

/// How symmetric a point symbol is about its own centre: `None` for one which
/// looks the same at every angle, and `Some(n)` for one which does not.
fn point_symmetry(point: &PointSymbol) -> Option<u32> {
    // A filled dot and a ring around it are circles, and a circle is the same
    // at every angle. Anything past that is drawn out of elements this does
    // not read, so it is taken to look different at every angle -- see
    // [`pattern_symmetry`] on why that is the safe way to be wrong.
    if point.elements.is_empty() {
        None
    } else {
        Some(1)
    }
}

/// The largest number which divides both, and the order of the rotations two
/// symmetric things have in common.
fn greatest_common_divisor(a: u32, b: u32) -> u32 {
    let (mut a, mut b) = (a.max(1), b.max(1));
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
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

/// Whether the symbol is the OpenOrienteering logo.
///
/// Every symbol set Mapper ships carries it, as an ordinary point symbol
/// which says nothing about itself: not hidden, no helper flag, code 999. It
/// is there to be stamped in a corner of a printed map, and it is not a
/// thing which stands anywhere on the ground — so a generator which dropped
/// it into a cell would be testing the renderer against a map no surveyor
/// could draw.
///
/// Matched by name, with the case and the spaces taken out, so that the
/// "OpenOrienteering Logo" of one set and the "OpenOrienteering Mapper logo"
/// of the next are both caught.
fn is_the_mapper_logo(symbol: &Symbol) -> bool {
    let name: String = name_of(symbol)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    name.contains("openorienteering") && name.contains("logo")
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
    use crate::map::{
        AreaSymbol, Color, CombinedSymbol, FillPattern, LineSymbol, PointSymbol, TextSymbol,
    };

    /// A map with three colours, in drawing order from the top: an opaque
    /// one, a half transparent one, and the opaque one a piece of ground is
    /// filled with, which everything else is above.
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
            Color {
                name: "ground".to_string(),
                rgb: (1.0, 1.0, 0.0),
                opacity: 1.0,
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

    /// An area filled with one turning grid of round dots, the lines `across`
    /// apart and the dots `along` apart down each of them.
    fn dotted(across: f64, along: f64, stagger: f64) -> Symbol {
        Symbol::Area(AreaSymbol {
            color: 2,
            patterns: vec![FillPattern {
                pattern_type: fill_pattern_type::POINT,
                rotatable: true,
                line_spacing: across,
                point_distance: along,
                offset_along_line: stagger,
                point: Some(Box::new(PointSymbol {
                    inner_radius: 0.2,
                    inner_color: 0,
                    ..PointSymbol::new()
                })),
                ..FillPattern::default()
            }],
            ..AreaSymbol::new()
        })
    }

    /// A square grid of round dots is the same grid every quarter turn, which
    /// is the whole reason any of this is counted.
    #[test]
    fn a_square_grid_of_round_dots_is_four_fold() {
        let map = map_with(vec![dotted(1.2, 1.2, 0.0)]);
        assert_eq!(pattern_symmetry(&map, &map.symbols[0]), 4);
        // And the catalogue carries it, which is where everything else reads
        // it from.
        assert_eq!(Catalogue::of(&map).opaque_areas[0].symmetry, 4);
    }

    /// Lines further apart than the dots along them is a rectangular grid,
    /// and a rectangle only comes back to itself at a half turn.
    #[test]
    fn an_oblong_grid_of_dots_is_only_two_fold() {
        let map = map_with(vec![dotted(2.4, 1.2, 0.0)]);
        assert_eq!(pattern_symmetry(&map, &map.symbols[0]), 2);
    }

    /// A grid whose lines are shifted along themselves is not rectangular at
    /// all, and this claims no more than a half turn for it — see
    /// [`pattern_symmetry`] on why it errs downwards.
    #[test]
    fn a_staggered_grid_is_left_at_two() {
        let map = map_with(vec![dotted(1.2, 1.2, 0.6)]);
        assert_eq!(pattern_symmetry(&map, &map.symbols[0]), 2);
    }

    /// The grid is only as symmetric as what stands on it: a point symbol
    /// which draws more than a dot and a ring could be anything, and takes
    /// the whole pattern down to a symmetry of one.
    #[test]
    fn a_grid_of_shapes_which_are_not_round_claims_nothing() {
        let Symbol::Area(mut area) = dotted(1.2, 1.2, 0.0) else {
            unreachable!("dotted builds an area")
        };
        let point = area.patterns[0].point.as_mut().expect("a point symbol");
        point.elements = vec![crate::map::Element {
            symbol: Symbol::Line(LineSymbol::default()),
            object: crate::map::Object::new(crate::map::ObjectKind::Point),
        }];
        let map = map_with(vec![Symbol::Area(area)]);
        assert_eq!(pattern_symmetry(&map, &map.symbols[0]), 1);
    }

    /// Parallel lines are the same lines upside down and nothing better.
    #[test]
    fn a_hatching_is_two_fold() {
        let map = map_with(vec![Symbol::Area(AreaSymbol {
            color: 2,
            patterns: vec![FillPattern {
                pattern_type: fill_pattern_type::LINE,
                rotatable: true,
                line_spacing: 1.0,
                line_color: 0,
                line_width: 0.1,
                ..FillPattern::default()
            }],
            ..AreaSymbol::new()
        })]);
        assert_eq!(pattern_symmetry(&map, &map.symbols[0]), 2);
    }

    /// A pattern which does not turn with its object looks the same whatever
    /// the object's angle, so it says nothing about the angle and is not
    /// counted. A symbol with only such patterns folds nothing.
    #[test]
    fn a_pattern_which_does_not_turn_is_not_counted() {
        let Symbol::Area(mut area) = dotted(1.2, 1.2, 0.0) else {
            unreachable!("dotted builds an area")
        };
        area.patterns[0].rotatable = false;
        let map = map_with(vec![Symbol::Area(area)]);
        assert_eq!(pattern_symmetry(&map, &map.symbols[0]), 1);
        assert!(!Catalogue::of(&map).opaque_areas[0].turns);
    }

    /// Two turning patterns over one another are turned together, so what
    /// survives is what survives in both: a quarter-symmetric grid under a
    /// hatching comes to a half turn.
    #[test]
    fn two_patterns_keep_only_what_they_share() {
        let Symbol::Area(mut area) = dotted(1.2, 1.2, 0.0) else {
            unreachable!("dotted builds an area")
        };
        area.patterns.push(FillPattern {
            pattern_type: fill_pattern_type::LINE,
            rotatable: true,
            line_spacing: 1.0,
            line_color: 0,
            line_width: 0.1,
            ..FillPattern::default()
        });
        let map = map_with(vec![Symbol::Area(area)]);
        assert_eq!(pattern_symmetry(&map, &map.symbols[0]), 2);
    }

    fn area_with_pattern(rotatable: bool) -> Symbol {
        Symbol::Area(AreaSymbol {
            color: 0,
            patterns: vec![FillPattern {
                rotatable,
                ..FillPattern::default()
            }],
            ..AreaSymbol::new()
        })
    }

    fn combined(part_ids: Vec<i32>) -> Symbol {
        Symbol::Combined(CombinedSymbol {
            part_ids,
            ..Default::default()
        })
    }

    /// A dot of the given colour, which is a point symbol drawing one thing.
    fn dot(color: i32) -> Symbol {
        Symbol::Point(PointSymbol {
            inner_radius: 0.5,
            inner_color: color,
            ..PointSymbol::new()
        })
    }

    /// An area which is only a pattern of lines of the given colour: it
    /// covers nothing, and what it draws is that one colour.
    fn hatching(color: i32) -> Symbol {
        Symbol::Area(AreaSymbol {
            color: -1,
            patterns: vec![FillPattern {
                line_color: color,
                line_width: 0.1,
                ..FillPattern::default()
            }],
            ..AreaSymbol::new()
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
    fn a_pattern_turns_only_where_it_says_it_does() {
        let map = map_with(vec![
            area(0),
            area_with_pattern(false),
            area_with_pattern(true),
            // A combined symbol turns where any part of it does.
            combined(vec![0, 1]),
            combined(vec![0, 2]),
        ]);
        let turns = |index: usize| has_rotatable_pattern(&map, &map.symbols[index]);
        assert!(!turns(0));
        assert!(!turns(1));
        assert!(turns(2));
        assert!(!turns(3));
        assert!(turns(4));
        // A line has no fill to turn.
        let lines = map_with(vec![Symbol::Line(LineSymbol::default())]);
        assert!(!has_rotatable_pattern(&lines, &lines.symbols[0]));
    }

    #[test]
    fn a_symbol_draws_with_every_colour_it_is_built_out_of() {
        // A dot inside a pattern inside an area, and the area itself.
        let point_pattern = Symbol::Area(AreaSymbol {
            color: 2,
            patterns: vec![FillPattern {
                pattern_type: fill_pattern_type::POINT,
                point: Some(Box::new(PointSymbol {
                    inner_radius: 0.5,
                    inner_color: 0,
                    ..PointSymbol::new()
                })),
                ..FillPattern::default()
            }],
            ..AreaSymbol::new()
        });
        let map = map_with(vec![point_pattern, dot(1), combined(vec![0, 1])]);
        assert_eq!(colors_of(&map, &map.symbols[0]), vec![2, 0]);
        assert_eq!(colors_of(&map, &map.symbols[1]), vec![1]);
        // A combined symbol draws with everything its parts draw with.
        assert_eq!(colors_of(&map, &map.symbols[2]), vec![2, 0, 1]);

        // What draws nothing has no colour: a stroke of no width and a dot
        // of no radius are set down in the file and never reach the paper.
        let nothing = map_with(vec![
            Symbol::Line(LineSymbol {
                color: 0,
                line_width: 0.0,
                ..Default::default()
            }),
            Symbol::Point(PointSymbol {
                inner_color: 0,
                inner_radius: 0.0,
                ..PointSymbol::new()
            }),
        ]);
        assert!(colors_of(&nothing, &nothing.symbols[0]).is_empty());
        assert!(colors_of(&nothing, &nothing.symbols[1]).is_empty());
    }

    #[test]
    fn an_overlay_shows_up_only_above_the_colour_which_covers_the_ground() {
        // Two grounds: one filled with the bottom colour, one with the top.
        let map = map_with(vec![area(2), area(0), dot(1), hatching(1)]);
        let (low_ground, high_ground) = (&map.symbols[0], &map.symbols[1]);
        for overlay in [&map.symbols[2], &map.symbols[3]] {
            // Colour 1 is above colour 2, so it is seen on that ground.
            assert!(shows_over(&map, overlay, low_ground));
            // It is below colour 0, so that ground covers it up.
            assert!(!shows_over(&map, overlay, high_ground));
        }
        // The same colour over itself is not something you can see either.
        assert!(!shows_over(&map, &dot(2), low_ground));
    }

    #[test]
    fn the_overlays_of_a_set_are_worked_out_per_ground() {
        let map = map_with(vec![area(2), area(0), dot(1), hatching(1)]);
        let catalogue = Catalogue::of(&map);
        assert_eq!(catalogue.opaque_areas.len(), 2);
        assert_eq!(catalogue.transparent_areas.len(), 1);
        assert_eq!(catalogue.points.len(), 1);

        let overlays = Overlays::of(&map, &catalogue);
        // The first ground takes both overlays, the second takes neither.
        assert_eq!(overlays.transparent_areas, vec![vec![0], vec![]]);
        assert_eq!(overlays.points, vec![vec![0], vec![]]);
        assert_eq!(overlays.transparent_pairs(&catalogue), (1, 2));
        assert_eq!(overlays.point_pairs(&catalogue), (1, 2));
    }

    /// The logo is a point symbol like any other as far as the file goes, so
    /// nothing but its name keeps it off a generated map.
    #[test]
    fn the_mapper_logo_is_in_no_kind_at_all() {
        let named = |name: &str| {
            Symbol::Point(PointSymbol {
                name: name.to_string(),
                code: "999".to_string(),
                inner_radius: 0.5,
                inner_color: 0,
                ..PointSymbol::new()
            })
        };
        let map = map_with(vec![
            named("OpenOrienteering Logo"),
            named("OpenOrienteering Mapper logo"),
            named("openorienteeringmapperlogo"),
            // A symbol which only sounds like it: what draws the logo is a
            // symbol of the set, and it is one a map may be drawn with.
            named("Red for logos"),
            named("Boulder"),
        ]);
        let catalogue = Catalogue::of(&map);
        let kept: Vec<&str> = catalogue
            .points
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(kept, vec!["Red for logos", "Boulder"]);
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
