//! Checking a map against a mapping standard.
//!
//! An orienteering map is drawn to a specification: ISOM for forest maps,
//! ISSprOM for sprint, each of which fixes the colours, defines every symbol
//! down to its line widths, and lays down rules about what may touch what.
//! This module reads a map and reports where it departs from one.
//!
//! # The standard comes from the caller
//!
//! Nothing here knows what ISOM is. A [`Reference`] carries everything the
//! checks need — a pristine map of the standard's own symbols to compare
//! against, the codes it considers impassable, the combinations it forbids,
//! the smallest gap it allows — and a caller with a different standard passes
//! a different one. That is what makes adding ISSprOM a matter of new data
//! rather than new code.
//!
//! # What is checked
//!
//! * **Colours** — the ink of each colour against the standard's, and the
//!   order they are printed in, which is what decides what covers what.
//! * **Symbol definitions** — every symbol the map defines against the
//!   standard's own, field by field, with the differences named.
//! * **Point orientation** — symbols the standard says face north, but which
//!   the map has turned.
//! * **Contour crossings** — contours may never cross; where they do, the
//!   map is saying two heights at one place.
//! * **Forbidden overlaps** — screens the standard does not allow to be
//!   combined, such as one marsh over another.
//! * **Minimum gaps** — two symbols closer together than the standard's
//!   smallest gap, which at printing size merge into one mark.
//!
//! The last three are geometric and are the expensive half. Each takes a
//! `should_stop` callback, checked as it goes, so a caller can put a limit on
//! them; a stage that stops early says so.
//!
//! # Units
//!
//! Millimetres on the paper, like the rest of the crate. A standard's
//! graphical minimums are defined at one scale and scale with the map, so
//! that the ground they stand for stays the same size: everything the
//! reference gives in millimetres is multiplied by
//! [`Reference::base_scale`] over the map's own.

use std::collections::{HashMap, HashSet};

use crate::map::{
    coord_flag, Border, Color, CombinedSymbol, Coord, FillPattern, LineSymbol, Map, Object,
    ObjectKind, PartRef, PointSymbol, Symbol, TextSymbol,
};

/// The map scale assumed for a map that does not say.
const DEFAULT_MAP_SCALE: f64 = 15000.0;

/// The unit symbol dimensions are quantized to before comparison: a
/// thousandth of a millimetre, which is what the file format stores.
const QUANTUM_MM: f64 = 0.001;

/// How nearly two angles must agree, in radians.
const ANGLE_TOLERANCE: f64 = 1e-3;

/// How close two things must be to count as touching rather than as a gap.
///
/// Below this they merge into one mark in any case, and a file that has been
/// through a format conversion routinely has slivers this size where the
/// original had a join.
const TOUCH_EPSILON_MM: f64 = 0.05;

/// How bad a finding is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    /// The map is wrong: this cannot be printed as it stands.
    Error,
    /// The map departs from the standard, and the mapper should look.
    Warning,
    /// Worth knowing, but not a fault.
    Info,
}

impl Severity {
    /// What to call it.
    pub fn name(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

/// What kind of finding it is, for grouping and for filtering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    /// A symbol the standard does not define.
    UnknownSymbol,
    /// A symbol whose definition differs from the standard's.
    ModifiedSymbol,
    /// A numbered variant of a standard symbol.
    SymbolVariant,
    /// A non-standard symbol which is defined but never used.
    UnusedNonstandardSymbol,
    /// A colour whose ink differs from the standard's.
    ColorModified,
    /// Two colours printed in the wrong order.
    ColorOrder,
    /// A colour the standard does not have.
    ColorExtra,
    /// A colour the standard has and the map does not.
    ColorMissing,
    /// A symbol turned which the standard orients to north.
    PointRotation,
    /// Two contours crossing.
    ContourIntersection,
    /// Two areas overlapping which may not.
    AreaOverlap,
    /// Two symbols closer together than the standard allows.
    Gap,
    /// Something about the run itself, rather than about the map.
    Process,
}

impl Category {
    /// What to call it.
    pub fn name(self) -> &'static str {
        match self {
            Category::UnknownSymbol => "unknown-symbol",
            Category::ModifiedSymbol => "modified-symbol",
            Category::SymbolVariant => "symbol-variant",
            Category::UnusedNonstandardSymbol => "unused-nonstandard-symbol",
            Category::ColorModified => "color-modified",
            Category::ColorOrder => "color-order",
            Category::ColorExtra => "color-extra",
            Category::ColorMissing => "color-missing",
            Category::PointRotation => "point-rotation",
            Category::ContourIntersection => "contour-intersection",
            Category::AreaOverlap => "area-overlap",
            Category::Gap => "gap",
            Category::Process => "process",
        }
    }
}

/// One thing found wrong with a map.
#[derive(Clone, Debug)]
pub struct Issue {
    /// How bad it is.
    pub severity: Severity,
    /// What kind of finding it is.
    pub category: Category,
    /// The symbol code it is about, or the colour name for a colour finding.
    pub code: String,
    /// The second symbol, where the finding is about a pair.
    pub code2: Option<String>,
    /// One line saying what is wrong.
    pub message: String,
    /// The differences in detail, where there are several.
    pub details: Vec<String>,
    /// Which object it is about, as an index into the map's objects.
    pub object_index: Option<usize>,
    /// The second object, where the finding is about a pair.
    pub object_index2: Option<usize>,
    /// Where on the map to look, in mm.
    pub location: Option<(f64, f64)>,
    /// How much of the map around that point the finding covers, in mm.
    pub radius: Option<f64>,
}

impl Issue {
    fn new(
        severity: Severity,
        category: Category,
        code: impl Into<String>,
        message: String,
    ) -> Issue {
        Issue {
            severity,
            category,
            code: code.into(),
            code2: None,
            message,
            details: Vec::new(),
            object_index: None,
            object_index2: None,
            location: None,
            radius: None,
        }
    }
}

/// A mapping standard, as everything the checks need to know about one.
pub struct Reference<'a> {
    /// The standard's own symbol set, as a map: what the map under test is
    /// compared against, symbol for symbol and colour for colour.
    pub golden: &'a Map,
    /// The scale the standard's graphical minimums are defined at.
    pub base_scale: f64,
    /// Symbol code to what the standard calls it, for readable messages. A
    /// code is looked up whole and then by its base, so `101.1` finds `101`.
    pub code_descriptions: &'a HashMap<String, String>,
    /// Base codes for things a runner cannot cross, which the standard holds
    /// further apart than the rest.
    pub impassable_codes: &'a HashSet<String>,
    /// Base codes of lines which must never cross one another.
    pub no_cross_codes: &'a HashSet<String>,
    /// Pairs of base codes whose areas may not overlap, in either order.
    pub forbidden_area_overlaps: &'a [(String, String)],
    /// Pairs of base codes the gap rule does not apply between.
    pub gap_exempt_pairs: &'a [(String, String)],
    /// The smallest gap the standard allows, in mm at the base scale.
    pub min_gap_mm: f64,
    /// The same, between two things nothing can cross.
    pub min_gap_impassable_mm: f64,
}

/// One of the checks, so a caller can run them one at a time and say which is
/// running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    /// The colour table, and the order it is printed in.
    Colors,
    /// Every symbol definition against the standard's.
    Symbols,
    /// Symbols turned which the standard orients to north.
    PointRotations,
    /// Contours crossing one another.
    ContourCrossings,
    /// Areas overlapping which may not.
    AreaOverlaps,
    /// Symbols too close together to stay separate in print.
    Gaps,
}

impl Stage {
    /// Every check, in the order they are worth running.
    pub const ALL: [Stage; 6] = [
        Stage::Colors,
        Stage::Symbols,
        Stage::PointRotations,
        Stage::ContourCrossings,
        Stage::AreaOverlaps,
        Stage::Gaps,
    ];

    /// A machine-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Colors => "colors",
            Stage::Symbols => "symbols",
            Stage::PointRotations => "point-rotations",
            Stage::ContourCrossings => "contour-crossings",
            Stage::AreaOverlaps => "area-overlaps",
            Stage::Gaps => "gaps",
        }
    }

    /// What to say while it runs.
    pub fn message(self) -> &'static str {
        match self {
            Stage::Colors => "checking colors…",
            Stage::Symbols => "checking symbol definitions…",
            Stage::PointRotations => "checking point symbol orientation…",
            Stage::ContourCrossings => "checking contour crossings…",
            Stage::AreaOverlaps => "checking area overlaps…",
            Stage::Gaps => "checking minimum gaps…",
        }
    }

    /// Roughly what share of the work it is, for a progress bar.
    pub fn weight(self) -> u32 {
        match self {
            Stage::Colors => 1,
            Stage::Symbols => 2,
            Stage::PointRotations => 1,
            Stage::ContourCrossings => 3,
            Stage::AreaOverlaps => 3,
            Stage::Gaps => 8,
        }
    }
}

/// What one check found.
#[derive(Debug, Default)]
pub struct StageResult {
    /// What is wrong with the map.
    pub issues: Vec<Issue>,
    /// Whether the check gave up before it had finished, so that its findings
    /// may be incomplete.
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Reading the map and the standard

/// The scale a map is drawn at.
fn map_scale(map: &Map) -> f64 {
    if map.scale_denominator > 0 {
        f64::from(map.scale_denominator)
    } else {
        DEFAULT_MAP_SCALE
    }
}

/// What the standard's dimensions are multiplied by to suit this map.
///
/// A standard's minimums are about the ground, not the paper: on a map drawn
/// at twice the scale everything is drawn twice the size, so the same patch of
/// forest is the same patch of forest.
fn dimension_factor(map: &Map, reference: &Reference) -> f64 {
    reference.base_scale / map_scale(map)
}

/// The part of a code before its dot: `101.1` belongs to `101`.
fn base_code(code: &str) -> &str {
    match code.split_once('.') {
        Some((base, _)) => base,
        None => code,
    }
}

/// "521 Building" — the code, and what the standard calls it.
fn describe_code(code: &str, reference: &Reference) -> String {
    let described = reference
        .code_descriptions
        .get(code)
        .or_else(|| reference.code_descriptions.get(base_code(code)));
    match described {
        Some(description) => format!("{code} {description}"),
        None => code.to_string(),
    }
}

/// A colour as the ink that makes it, to three decimals.
///
/// Colours are referred to by their place in a map's own colour table, which
/// says nothing across two maps, so two colours are compared by what they
/// actually put on paper.
fn cmyk_key(color: i32, map: &Map) -> String {
    match map.color(color) {
        Some(c) => format!(
            "{:.3}/{:.3}/{:.3}/{:.3}/{:.3}",
            c.cmyk.0, c.cmyk.1, c.cmyk.2, c.cmyk.3, c.opacity
        ),
        None => "none".to_string(),
    }
}

/// What a colour is called, for a message.
fn color_name(color: i32, map: &Map) -> String {
    match map.color(color) {
        Some(c) => c.name.clone(),
        None => "(no color)".to_string(),
    }
}

/// Whether two lengths agree, allowing for rounding and for a percent of
/// slack on the larger ones.
fn nearly_equal(actual: f64, expected: f64, min_abs: f64) -> bool {
    (actual - expected).abs() <= min_abs.max(0.01 * expected.abs())
}

/// The middle of an object, for pointing at it on the map.
fn object_center(object: &Object) -> Option<(f64, f64)> {
    let mut coords = object.coords.iter();
    let first = coords.next()?;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.x, first.y, first.x, first.y);
    for c in coords {
        min_x = min_x.min(c.x);
        min_y = min_y.min(c.y);
        max_x = max_x.max(c.x);
        max_y = max_y.max(c.y);
    }
    Some(((min_x + max_x) / 2.0, (min_y + max_y) / 2.0))
}

/// The symbol an object is drawn with.
fn symbol_of<'m>(map: &'m Map, object: &Object) -> Option<&'m Symbol> {
    object.symbol_index.and_then(|i| map.symbols.get(i))
}

/// The symbols a symbol actually draws with: itself, unless it is a
/// combination, in which case its parts, recursively.
fn leaves<'m>(symbol: &'m Symbol, map: &'m Map, out: &mut Vec<&'m Symbol>) {
    let Symbol::Combined(combined) = symbol else {
        out.push(symbol);
        return;
    };
    for part in &combined.parts {
        match *part {
            PartRef::Shared(i) => {
                if let Some(part) = map.symbols.get(i) {
                    leaves(part, map, out);
                }
            }
            PartRef::Private(i) => {
                if let Some(part) = combined.owned_parts.get(i) {
                    leaves(part, map, out);
                }
            }
            PartRef::None => {}
        }
    }
}

fn leaves_of<'m>(symbol: &'m Symbol, map: &'m Map) -> Vec<&'m Symbol> {
    let mut out = Vec::new();
    leaves(symbol, map, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Colours

/// Colour names compared as a reader would: case and spacing do not make two
/// colours different.
fn norm_name(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether two colours are the same ink to the precision the format is
/// written in.
fn same_cmyk(a: &Color, b: &Color) -> bool {
    // The format writes three decimals, so anything past rounding is an edit.
    const TOLERANCE: f64 = 0.005;
    (a.cmyk.0 - b.cmyk.0).abs() <= TOLERANCE
        && (a.cmyk.1 - b.cmyk.1).abs() <= TOLERANCE
        && (a.cmyk.2 - b.cmyk.2).abs() <= TOLERANCE
        && (a.cmyk.3 - b.cmyk.3).abs() <= TOLERANCE
        && (a.opacity - b.opacity).abs() <= TOLERANCE
}

fn channel_diffs(actual: &Color, expected: &Color) -> Vec<String> {
    const TOLERANCE: f64 = 0.005;
    let channels: [(&str, f64, f64); 5] = [
        ("cyan", actual.cmyk.0, expected.cmyk.0),
        ("magenta", actual.cmyk.1, expected.cmyk.1),
        ("yellow", actual.cmyk.2, expected.cmyk.2),
        ("black", actual.cmyk.3, expected.cmyk.3),
        ("opacity", actual.opacity, expected.opacity),
    ];
    channels
        .iter()
        .filter(|(_, a, e)| (a - e).abs() > TOLERANCE)
        .map(|(name, a, e)| format!("{name}: expected {e:.3}, found {a:.3}"))
        .collect()
}

/// Checks the map's colour table against the standard's: the ink of each
/// colour by name, colours on one side and not the other, and the order they
/// are printed in — which in an orienteering map is what decides which symbol
/// covers which.
pub fn validate_colors(map: &Map, reference: &Reference) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut golden_by_name: HashMap<String, &Color> = HashMap::new();
    for color in &reference.golden.colors {
        golden_by_name
            .entry(norm_name(&color.name))
            .or_insert(color);
    }

    let mut matched: HashSet<String> = HashSet::new();
    // A colour's place in the table is the order it is printed in.
    for color in &map.colors {
        let key = norm_name(&color.name);
        if let Some(golden) = golden_by_name.get(&key) {
            matched.insert(key);
            let diffs = channel_diffs(color, golden);
            if !diffs.is_empty() {
                let mut issue = Issue::new(
                    Severity::Warning,
                    Category::ColorModified,
                    color.name.clone(),
                    format!("Color \"{}\" differs from the ISOM definition", color.name),
                );
                issue.details = diffs;
                issues.push(issue);
            }
            continue;
        }
        // Not a name the standard knows: it may still be one of its colours
        // under another name, which is worth saying rather than calling it new.
        let twin = reference.golden.colors.iter().find(|g| same_cmyk(color, g));
        let message = match twin {
            Some(twin) => format!(
                "Color \"{}\" is not an ISOM color name but matches \"{}\" (renamed standard color)",
                color.name, twin.name
            ),
            None => format!("Color \"{}\" is not part of the ISOM color table", color.name),
        };
        issues.push(Issue::new(
            Severity::Info,
            Category::ColorExtra,
            color.name.clone(),
            message,
        ));
    }

    for golden in &reference.golden.colors {
        if !matched.contains(&norm_name(&golden.name)) {
            issues.push(Issue::new(
                Severity::Info,
                Category::ColorMissing,
                golden.name.clone(),
                format!(
                    "Standard color \"{}\" is not defined in this map",
                    golden.name
                ),
            ));
        }
    }

    // The order: the standard's colours, in the order this map prints them,
    // must be in the standard's order too. Every step backwards is a symbol
    // covering one it should be under.
    let mut golden_rank: HashMap<String, usize> = HashMap::new();
    for (rank, color) in reference.golden.colors.iter().enumerate() {
        golden_rank.entry(norm_name(&color.name)).or_insert(rank);
    }
    let ordered: Vec<&Color> = map
        .colors
        .iter()
        .filter(|c| golden_rank.contains_key(&norm_name(&c.name)))
        .collect();
    let mut order_issues = 0;
    for pair in ordered.windows(2) {
        if order_issues >= 10 {
            break;
        }
        let (prev, cur) = (pair[0], pair[1]);
        if golden_rank[&norm_name(&cur.name)] < golden_rank[&norm_name(&prev.name)] {
            order_issues += 1;
            let mut issue = Issue::new(
                Severity::Warning,
                Category::ColorOrder,
                cur.name.clone(),
                format!(
                    "Color \"{}\" is drawn below \"{}\" but ISOM puts it above",
                    cur.name, prev.name
                ),
            );
            issue.code2 = Some(prev.name.clone());
            issues.push(issue);
        }
    }

    issues
}

// ---------------------------------------------------------------------------
// Symbol definitions

/// One side of a symbol comparison: a map, and how its lengths relate to the
/// standard's.
struct Side<'a> {
    map: &'a Map,
    /// What a length on this side is divided by to reach the standard's scale.
    inv_f: f64,
}

/// A length as a whole number of the smallest unit the format stores, at the
/// standard's own scale — so a map drawn at twice the scale quantizes to the
/// same numbers the standard does.
fn norm_length(value: f64, inv_f: f64) -> i64 {
    (value / QUANTUM_MM * inv_f).round() as i64
}

/// Builds a canonical description of a symbol, for comparing one against
/// another.
///
/// Colours become the ink they resolve to and lengths become whole units at
/// the standard's scale, so that two symbols which draw the same thing
/// describe the same thing however their file happens to store them. Used for
/// the sub-symbols hanging off a symbol — the glyph along a line, the pattern
/// filling an area — where naming every field that differs would bury the one
/// that matters; the symbol itself gets a real comparison.
fn fingerprint_point_sub(point: Option<&PointSymbol>, side: &Side, out: &mut String) {
    match point {
        None => out.push_str("none;"),
        Some(point) => fingerprint_point(point, side, out),
    }
}

fn fingerprint(symbol: Option<&Symbol>, side: &Side, out: &mut String) {
    let Some(symbol) = symbol else {
        out.push_str("none;");
        return;
    };
    match symbol {
        Symbol::Line(line) => fingerprint_line(line, side, out),
        Symbol::Area(area) => {
            out.push_str("area{");
            out.push_str(&cmyk_key(area.color, side.map));
            for pattern in &area.patterns {
                fingerprint_pattern(pattern, side, out);
            }
            out.push_str("};");
        }
        Symbol::Point(point) => fingerprint_point(point, side, out),
        Symbol::Text(text) => fingerprint_text(text, side, out),
        Symbol::Combined(combined) => {
            out.push_str("combined{");
            for part in resolved_parts(combined, side.map) {
                fingerprint(part, side, out);
            }
            out.push_str("};");
        }
    }
}

fn fingerprint_line(line: &LineSymbol, side: &Side, out: &mut String) {
    let l = |v: f64| norm_length(v, side.inv_f);
    out.push_str("line{");
    out.push_str(&cmyk_key(line.color, side.map));
    out.push_str(&format!(",w{}", l(line.line_width)));
    out.push_str(&format!(",dashed{}", line.dashed));
    // A dash pattern only means anything on a dashed line, and the numbers
    // stored on a solid one are whatever its editor happened to leave there.
    if line.dashed {
        out.push_str(&format!(
            ",d{},b{},g{},ib{},h{}",
            l(line.dash_length),
            l(line.break_length),
            line.dashes_in_group,
            l(line.in_group_break_length),
            line.half_outer_dashes
        ));
    }
    out.push_str(&format!(
        ",so{},eo{},cap{},join{}",
        l(line.start_offset),
        l(line.end_offset),
        line.cap_style,
        line.join_style
    ));
    fingerprint_border(&line.border, side, out);
    fingerprint_border(&line.right_border, side, out);
    // Likewise the spacing of a glyph along the line, without a glyph.
    if line.mid_symbol.is_some() {
        out.push_str(&format!(
            ",sl{},el{},one{},per{},md{},mp{}",
            l(line.segment_length),
            l(line.end_length),
            line.show_at_least_one_symbol,
            line.mid_symbols_per_spot,
            l(line.mid_symbol_distance),
            line.mid_symbol_placement
        ));
    }
    fingerprint_point_sub(line.mid_symbol.as_deref(), side, out);
    fingerprint_point_sub(line.dash_symbol.as_deref(), side, out);
    fingerprint_point_sub(line.start_symbol.as_deref(), side, out);
    fingerprint_point_sub(line.end_symbol.as_deref(), side, out);
    out.push_str("};");
}

/// A border of zero width draws nothing, which is how the format says a line
/// has none.
fn has_border(border: &Border) -> bool {
    border.width > 0.0
}

fn fingerprint_border(border: &Border, side: &Side, out: &mut String) {
    if !has_border(border) {
        out.push_str("noborder;");
        return;
    }
    let l = |v: f64| norm_length(v, side.inv_f);
    out.push_str(&format!(
        "border{{{},w{},s{},dashed{},d{},b{}}};",
        cmyk_key(border.color, side.map),
        l(border.width),
        l(border.shift),
        border.dashed,
        l(border.dash_length),
        l(border.break_length)
    ));
}

fn fingerprint_pattern(pattern: &FillPattern, side: &Side, out: &mut String) {
    let l = |v: f64| norm_length(v, side.inv_f);
    out.push_str(&format!(
        "pattern{{t{},{},w{},sp{},a{},lo{},oa{},pd{},rot{}}}",
        pattern.pattern_type,
        cmyk_key(pattern.line_color, side.map),
        l(pattern.line_width),
        l(pattern.line_spacing),
        (pattern.angle / ANGLE_TOLERANCE).round(),
        l(pattern.line_offset),
        l(pattern.offset_along_line),
        l(pattern.point_distance),
        pattern.rotatable
    ));
    fingerprint_point_sub(pattern.point.as_deref(), side, out);
    out.push(';');
}

fn fingerprint_point(point: &PointSymbol, side: &Side, out: &mut String) {
    let l = |v: f64| norm_length(v, side.inv_f);
    out.push_str(&format!(
        "point{{rot{},ir{},{},ow{},{}",
        point.is_rotatable,
        l(point.inner_radius),
        cmyk_key(point.inner_color, side.map),
        l(point.outer_width),
        cmyk_key(point.outer_color, side.map)
    ));
    for element in &point.elements {
        fingerprint(Some(&element.symbol), side, out);
        fingerprint_coords(&element.object, side, out);
    }
    out.push_str("};");
}

/// An element's shape, quantized like every other length.
fn fingerprint_coords(object: &Object, side: &Side, out: &mut String) {
    out.push_str("coords[");
    for coord in &object.coords {
        out.push_str(&format!(
            "{},{},{};",
            norm_length(coord.x, side.inv_f),
            norm_length(coord.y, side.inv_f),
            coord.flags
        ));
    }
    out.push(']');
    if let ObjectKind::Path(path) = &object.kind {
        out.push_str(&format!(
            "pr{}",
            (path.pattern_rotation / ANGLE_TOLERANCE).round()
        ));
    }
    out.push(';');
}

fn fingerprint_text(text: &TextSymbol, side: &Side, out: &mut String) {
    out.push_str(&format!(
        "text{{fs{},{},ls{},b{},i{}}};",
        norm_length(text.font_size, side.inv_f),
        cmyk_key(text.color, side.map),
        (text.line_spacing * 1000.0).round(),
        text.bold,
        text.italic
    ));
}

/// The symbols a combined symbol is made of, resolved against the map it
/// belongs to.
fn resolved_parts<'m>(combined: &'m CombinedSymbol, map: &'m Map) -> Vec<Option<&'m Symbol>> {
    combined
        .parts
        .iter()
        .map(|part| match *part {
            PartRef::Shared(i) => map.symbols.get(i),
            PartRef::Private(i) => combined.owned_parts.get(i),
            PartRef::None => None,
        })
        .collect()
}

fn fingerprint_of(symbol: Option<&Symbol>, side: &Side) -> String {
    let mut out = String::new();
    fingerprint(symbol, side, &mut out);
    out
}

/// Compares one symbol against the standard's, and says what differs.
///
/// The symbol itself is compared field by field, so that a message can name
/// the line width or the fill colour; what hangs off it is compared whole,
/// since a glyph that differs differs, and listing every number in it would
/// not help.
fn diff_symbols(
    actual: &Symbol,
    expected: &Symbol,
    map_side: &Side,
    golden_side: &Side,
    f: f64,
) -> Vec<String> {
    let mut diffs = Vec::new();

    let kind_name = |s: &Symbol| match s {
        Symbol::Point(_) => "point",
        Symbol::Line(_) => "line",
        Symbol::Area(_) => "area",
        Symbol::Text(_) => "text",
        Symbol::Combined(_) => "combined",
    };
    if std::mem::discriminant(actual) != std::mem::discriminant(expected) {
        return vec![format!(
            "symbol type: expected {}, found {}",
            kind_name(expected),
            kind_name(actual)
        )];
    }

    let len = |label: &str, actual: f64, expected: f64, diffs: &mut Vec<String>| {
        if !nearly_equal(actual, expected * f, QUANTUM_MM) {
            diffs.push(format!(
                "{label}: expected {}, found {} (0.001 mm units)",
                (expected * f / QUANTUM_MM).round(),
                (actual / QUANTUM_MM).round()
            ));
        }
    };
    let color = |label: &str, actual: i32, expected: i32, diffs: &mut Vec<String>| {
        if cmyk_key(actual, map_side.map) != cmyk_key(expected, golden_side.map) {
            diffs.push(format!(
                "{label}: expected \"{}\", found \"{}\"",
                color_name(expected, golden_side.map),
                color_name(actual, map_side.map)
            ));
        }
    };
    let nested_point = |label: &str,
                        actual: Option<&PointSymbol>,
                        expected: Option<&PointSymbol>,
                        diffs: &mut Vec<String>| {
        let mut fa = String::new();
        fingerprint_point_sub(actual, map_side, &mut fa);
        let mut fe = String::new();
        fingerprint_point_sub(expected, golden_side, &mut fe);
        if fa != fe {
            diffs.push(format!("{label}: definition differs from the standard"));
        }
    };
    let nested = |label: &str,
                  actual: Option<&Symbol>,
                  expected: Option<&Symbol>,
                  diffs: &mut Vec<String>| {
        if fingerprint_of(actual, map_side) != fingerprint_of(expected, golden_side) {
            diffs.push(format!("{label}: definition differs from the standard"));
        }
    };
    fn exact<T: PartialEq + std::fmt::Display>(
        label: &str,
        actual: T,
        expected: T,
        diffs: &mut Vec<String>,
    ) {
        if actual != expected {
            diffs.push(format!("{label}: expected {expected}, found {actual}"));
        }
    }

    match (actual, expected) {
        (Symbol::Line(a), Symbol::Line(e)) => {
            color("color", a.color, e.color, &mut diffs);
            len("line width", a.line_width, e.line_width, &mut diffs);
            exact("dashed", a.dashed, e.dashed, &mut diffs);
            if a.dashed && e.dashed {
                len("dash length", a.dash_length, e.dash_length, &mut diffs);
                len("break length", a.break_length, e.break_length, &mut diffs);
                exact(
                    "dashes in group",
                    a.dashes_in_group,
                    e.dashes_in_group,
                    &mut diffs,
                );
                len(
                    "in-group break length",
                    a.in_group_break_length,
                    e.in_group_break_length,
                    &mut diffs,
                );
                exact(
                    "half outer dashes",
                    a.half_outer_dashes,
                    e.half_outer_dashes,
                    &mut diffs,
                );
            }
            len("start offset", a.start_offset, e.start_offset, &mut diffs);
            len("end offset", a.end_offset, e.end_offset, &mut diffs);
            exact("cap style", a.cap_style, e.cap_style, &mut diffs);
            exact("join style", a.join_style, e.join_style, &mut diffs);
            // A pointed cap's taper is kept as the line's own offsets, and
            // is compared along with them.
            // Without a glyph on the line, its spacing is inert, and editors
            // leave different numbers there.
            if a.mid_symbol.is_some() || e.mid_symbol.is_some() {
                len(
                    "mid-symbol spacing",
                    a.segment_length,
                    e.segment_length,
                    &mut diffs,
                );
                len("end length", a.end_length, e.end_length, &mut diffs);
                exact(
                    "mid-symbols per spot",
                    a.mid_symbols_per_spot,
                    e.mid_symbols_per_spot,
                    &mut diffs,
                );
                len(
                    "mid-symbol distance",
                    a.mid_symbol_distance,
                    e.mid_symbol_distance,
                    &mut diffs,
                );
                exact(
                    "mid-symbol placement",
                    a.mid_symbol_placement,
                    e.mid_symbol_placement,
                    &mut diffs,
                );
            }
            if has_border(&a.border) != has_border(&e.border) {
                diffs.push(format!(
                    "border: expected {}, found {}",
                    if has_border(&e.border) {
                        "present"
                    } else {
                        "absent"
                    },
                    if has_border(&a.border) {
                        "present"
                    } else {
                        "absent"
                    }
                ));
            } else if has_border(&a.border) {
                color("border color", a.border.color, e.border.color, &mut diffs);
                len("border width", a.border.width, e.border.width, &mut diffs);
                len("border shift", a.border.shift, e.border.shift, &mut diffs);
                exact(
                    "border dashed",
                    a.border.dashed,
                    e.border.dashed,
                    &mut diffs,
                );
            }
            nested_point(
                "mid-symbol",
                a.mid_symbol.as_deref(),
                e.mid_symbol.as_deref(),
                &mut diffs,
            );
            nested_point(
                "dash-point symbol",
                a.dash_symbol.as_deref(),
                e.dash_symbol.as_deref(),
                &mut diffs,
            );
            nested_point(
                "start symbol",
                a.start_symbol.as_deref(),
                e.start_symbol.as_deref(),
                &mut diffs,
            );
            nested_point(
                "end symbol",
                a.end_symbol.as_deref(),
                e.end_symbol.as_deref(),
                &mut diffs,
            );
        }
        (Symbol::Area(a), Symbol::Area(e)) => {
            color("fill color", a.color, e.color, &mut diffs);
            if a.patterns.len() != e.patterns.len() {
                diffs.push(format!(
                    "pattern count: expected {}, found {}",
                    e.patterns.len(),
                    a.patterns.len()
                ));
            } else {
                for (i, (ap, ep)) in a.patterns.iter().zip(&e.patterns).enumerate() {
                    let label = format!("pattern {}", i + 1);
                    exact(
                        &format!("{label} type"),
                        ap.pattern_type,
                        ep.pattern_type,
                        &mut diffs,
                    );
                    color(
                        &format!("{label} color"),
                        ap.line_color,
                        ep.line_color,
                        &mut diffs,
                    );
                    len(
                        &format!("{label} line width"),
                        ap.line_width,
                        ep.line_width,
                        &mut diffs,
                    );
                    len(
                        &format!("{label} spacing"),
                        ap.line_spacing,
                        ep.line_spacing,
                        &mut diffs,
                    );
                    if (ap.angle - ep.angle).abs() > ANGLE_TOLERANCE {
                        diffs.push(format!(
                            "{label} angle: expected {:.3} rad, found {:.3} rad",
                            ep.angle, ap.angle
                        ));
                    }
                    len(
                        &format!("{label} point distance"),
                        ap.point_distance,
                        ep.point_distance,
                        &mut diffs,
                    );
                    nested_point(
                        &format!("{label} glyph"),
                        ap.point.as_deref(),
                        ep.point.as_deref(),
                        &mut diffs,
                    );
                }
            }
        }
        (Symbol::Point(a), Symbol::Point(e)) => {
            exact("rotatable", a.is_rotatable, e.is_rotatable, &mut diffs);
            len("inner radius", a.inner_radius, e.inner_radius, &mut diffs);
            color("inner color", a.inner_color, e.inner_color, &mut diffs);
            len("outer width", a.outer_width, e.outer_width, &mut diffs);
            color("outer color", a.outer_color, e.outer_color, &mut diffs);
            if a.elements.len() != e.elements.len() {
                diffs.push(format!(
                    "element count: expected {}, found {}",
                    e.elements.len(),
                    a.elements.len()
                ));
            } else {
                for (i, (ae, ee)) in a.elements.iter().zip(&e.elements).enumerate() {
                    let mut fa = String::new();
                    fingerprint(Some(&ae.symbol), map_side, &mut fa);
                    fingerprint_coords(&ae.object, map_side, &mut fa);
                    let mut fe = String::new();
                    fingerprint(Some(&ee.symbol), golden_side, &mut fe);
                    fingerprint_coords(&ee.object, golden_side, &mut fe);
                    if fa != fe {
                        diffs.push(format!(
                            "element {}: definition differs from the standard",
                            i + 1
                        ));
                    }
                }
            }
        }
        (Symbol::Text(a), Symbol::Text(e)) => {
            len("font size", a.font_size, e.font_size, &mut diffs);
            color("color", a.color, e.color, &mut diffs);
            exact("bold", a.bold, e.bold, &mut diffs);
            exact("italic", a.italic, e.italic, &mut diffs);
        }
        (Symbol::Combined(a), Symbol::Combined(e)) => {
            if a.parts.len() != e.parts.len() {
                diffs.push(format!(
                    "part count: expected {}, found {}",
                    e.parts.len(),
                    a.parts.len()
                ));
            } else {
                let ap = resolved_parts(a, map_side.map);
                let ep = resolved_parts(e, golden_side.map);
                for (i, (ra, re)) in ap.iter().zip(&ep).enumerate() {
                    nested(&format!("part {}", i + 1), *ra, *re, &mut diffs);
                }
            }
        }
        _ => {}
    }
    diffs
}

/// Finds which of the standard's symbols a code belongs to, allowing for a
/// map's own variants: `101.23` is tried, then `101.2`, then `101`.
fn resolve_golden<'a>(
    code: &str,
    golden_by_code: &'a HashMap<String, Vec<usize>>,
) -> Option<(&'a Vec<usize>, String)> {
    let mut candidate = code.to_string();
    loop {
        if let Some(list) = golden_by_code.get(&candidate) {
            return Some((list, candidate));
        }
        candidate.truncate(candidate.rfind('.')?);
    }
}

/// The same walk over the standard's list of codes, for a code the golden map
/// has no definition of but the standard still knows.
fn resolve_official_code(code: &str, descriptions: &HashMap<String, String>) -> Option<String> {
    let mut candidate = code.to_string();
    loop {
        if descriptions.contains_key(&candidate) {
            return Some(candidate);
        }
        candidate.truncate(candidate.rfind('.')?);
    }
}

/// Groups the standard's symbols by the code they are numbered with.
fn golden_by_code(reference: &Reference) -> HashMap<String, Vec<usize>> {
    let mut by_code: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, symbol) in reference.golden.symbols.iter().enumerate() {
        by_code
            .entry(symbol.code().to_string())
            .or_default()
            .push(i);
    }
    by_code
}

/// Checks every symbol the map defines against the standard's own.
///
/// A symbol may be the standard's, a numbered variant of one -- which is
/// allowed, and worth saying -- or something the standard does not have at
/// all. Where it is the standard's but drawn differently, the differences are
/// named field by field.
pub fn validate_symbols(map: &Map, reference: &Reference) -> Vec<Issue> {
    let mut issues = Vec::new();
    let f = dimension_factor(map, reference);
    let map_side = Side {
        map,
        inv_f: 1.0 / f,
    };
    let golden_side = Side {
        map: reference.golden,
        inv_f: 1.0,
    };
    let by_code = golden_by_code(reference);

    // How often each symbol is used, and where to find the first one.
    let mut usage: HashMap<usize, (usize, usize)> = HashMap::new();
    for (index, object) in map.objects.iter().enumerate() {
        if let Some(symbol_index) = object.symbol_index {
            let entry = usage.entry(symbol_index).or_insert((0, index));
            entry.0 += 1;
        }
    }

    for (symbol_index, symbol) in map.symbols.iter().enumerate() {
        let code = symbol.code();
        let used = usage.get(&symbol_index).copied();
        let Some((golden, via_code)) = resolve_golden(code, &by_code) else {
            // The standard's list of codes is part of the allowance too: some
            // official codes have no drawn definition to compare against, but
            // a symbol carrying one is not a new symbol.
            if let Some(official) = resolve_official_code(code, reference.code_descriptions) {
                if official != code {
                    issues.push(Issue::new(
                        Severity::Info,
                        Category::SymbolVariant,
                        code,
                        format!(
                            "Symbol {code} \"{}\" maps to {} (no reference definition to compare against)",
                            symbol.name(),
                            describe_code(&official, reference)
                        ),
                    ));
                }
                continue;
            }
            match used {
                Some((count, first_index)) => {
                    let mut issue = Issue::new(
                        Severity::Error,
                        Category::UnknownSymbol,
                        code,
                        format!(
                            "Symbol {code} \"{}\" is not an ISOM 2017-2 symbol and cannot be mapped to one ({count} object{})",
                            symbol.name(),
                            if count == 1 { "" } else { "s" }
                        ),
                    );
                    issue.object_index = Some(first_index);
                    issue.location = object_center(&map.objects[first_index]);
                    issues.push(issue);
                }
                None => issues.push(Issue::new(
                    Severity::Info,
                    Category::UnusedNonstandardSymbol,
                    code,
                    format!(
                        "Symbol {code} \"{}\" is not an ISOM 2017-2 symbol (defined but unused)",
                        symbol.name()
                    ),
                )),
            }
            continue;
        };

        let is_variant = via_code != code;
        // Matching any of the standard's definitions under that code is
        // enough; the closest one is what gets reported if none match.
        let mut best: Option<Vec<String>> = None;
        for &g in golden {
            let diffs = diff_symbols(
                symbol,
                &reference.golden.symbols[g],
                &map_side,
                &golden_side,
                f,
            );
            if diffs.is_empty() {
                best = Some(Vec::new());
                break;
            }
            if best.as_ref().is_none_or(|b| diffs.len() < b.len()) {
                best = Some(diffs);
            }
        }

        let official = describe_code(&via_code, reference);
        match best {
            Some(diffs) if !diffs.is_empty() => {
                let count = diffs.len();
                let mut issue = Issue::new(
                    Severity::Warning,
                    Category::ModifiedSymbol,
                    code,
                    if is_variant {
                        format!(
                            "Symbol {code} \"{}\" maps to {official} but its definition differs ({count} difference{})",
                            symbol.name(),
                            if count == 1 { "" } else { "s" }
                        )
                    } else {
                        format!(
                            "Symbol {}: definition differs from ISOM ({count} difference{})",
                            describe_code(code, reference),
                            if count == 1 { "" } else { "s" }
                        )
                    },
                );
                issue.details = diffs;
                issues.push(issue);
            }
            _ if is_variant => issues.push(Issue::new(
                Severity::Info,
                Category::SymbolVariant,
                code,
                format!(
                    "Symbol {code} \"{}\" is a compliant variant of {official}",
                    symbol.name()
                ),
            )),
            _ => {}
        }
    }

    issues
}

/// Finds objects the map has turned whose symbol the standard orients to
/// north.
///
/// Most point symbols on an orienteering map are read against north — a
/// boulder, a pit, a knoll — and turning one says something the mapper did
/// not mean. The standard says which by refusing them a rotation of their own.
pub fn validate_point_rotations(map: &Map, reference: &Reference) -> Vec<Issue> {
    let by_code = golden_by_code(reference);
    // In the order the codes are first met, so the report reads the same way
    // twice running.
    let mut order: Vec<String> = Vec::new();
    let mut by_symbol: HashMap<String, (usize, usize)> = HashMap::new();

    for (index, object) in map.objects.iter().enumerate() {
        if object.rotation == 0.0 {
            continue;
        }
        let Some(symbol) = symbol_of(map, object) else {
            continue;
        };
        if !matches!(symbol, Symbol::Point(_)) {
            continue;
        }
        let Some((golden, _)) = resolve_golden(symbol.code(), &by_code) else {
            continue;
        };
        let fixed_north = golden
            .iter()
            .any(|&g| matches!(&reference.golden.symbols[g], Symbol::Point(p) if !p.is_rotatable));
        if !fixed_north {
            continue;
        }
        let entry = by_symbol
            .entry(symbol.code().to_string())
            .or_insert_with(|| {
                order.push(symbol.code().to_string());
                (0, index)
            });
        entry.0 += 1;
    }

    order
        .into_iter()
        .map(|code| {
            let (count, first_index) = by_symbol[&code];
            let mut issue = Issue::new(
                Severity::Warning,
                Category::PointRotation,
                code.clone(),
                format!(
                    "{count} object{} of {} {} rotated, but this symbol shall be oriented to north",
                    if count == 1 { "" } else { "s" },
                    describe_code(&code, reference),
                    if count == 1 { "is" } else { "are" }
                ),
            );
            issue.object_index = Some(first_index);
            issue.location = object_center(&map.objects[first_index]);
            issue
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Geometry

/// A box around something, in mm.
#[derive(Clone, Copy, Debug)]
struct Bounds {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

impl Bounds {
    fn of(polylines: &[Vec<(f64, f64)>]) -> Bounds {
        let mut b = Bounds {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        };
        for line in polylines {
            for &(x, y) in line {
                b.min_x = b.min_x.min(x);
                b.min_y = b.min_y.min(y);
                b.max_x = b.max_x.max(x);
                b.max_y = b.max_y.max(y);
            }
        }
        b
    }

    fn overlaps(&self, other: &Bounds) -> bool {
        self.min_x <= other.max_x
            && other.min_x <= self.max_x
            && self.min_y <= other.max_y
            && other.min_y <= self.max_y
    }
}

/// How far a curve's control net may stand out from its chord before it is
/// split again, in mm. A hundredth of a millimetre is finer than anything
/// these checks measure.
const FLATTEN_TOLERANCE_MM: f64 = 0.01;
const MAX_SUBDIVISION_DEPTH: u32 = 20;

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

/// Splits a curve until it is straight enough to measure as a line.
fn flatten_cubic(
    p0: (f64, f64),
    p1: (f64, f64),
    p2: (f64, f64),
    p3: (f64, f64),
    depth: u32,
    out: &mut Vec<(f64, f64)>,
) {
    let chord = dist(p0, p3);
    let net = dist(p0, p1) + dist(p1, p2) + dist(p2, p3);
    if depth >= MAX_SUBDIVISION_DEPTH || net - chord <= FLATTEN_TOLERANCE_MM {
        out.push(p3);
        return;
    }
    let mid = |a: (f64, f64), b: (f64, f64)| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let p01 = mid(p0, p1);
    let p12 = mid(p1, p2);
    let p23 = mid(p2, p3);
    let p012 = mid(p01, p12);
    let p123 = mid(p12, p23);
    let m = mid(p012, p123);
    flatten_cubic(p0, p01, p012, m, depth + 1, out);
    flatten_cubic(m, p123, p23, p3, depth + 1, out);
}

/// An object's outline as plain polylines, one per subpath, with its curves
/// straightened.
///
/// The geometric checks measure distances and crossings, and a curve has to
/// become segments before either can be measured.
fn flatten_object(object: &Object) -> Vec<Vec<(f64, f64)>> {
    let coords = &object.coords;
    let mut parts: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut current: Vec<Coord> = Vec::new();
    let mut closed_flags: Vec<bool> = Vec::new();
    let mut raw_parts: Vec<Vec<Coord>> = Vec::new();

    // Subpaths end where the file says they do.
    let mut i = 0;
    while i < coords.len() {
        let c = coords[i];
        current.push(c);
        if c.flags & (coord_flag::CLOSE_POINT | coord_flag::HOLE_POINT) != 0 {
            closed_flags.push(c.flags & coord_flag::CLOSE_POINT != 0);
            raw_parts.push(std::mem::take(&mut current));
            i += 1;
            continue;
        }
        if c.flags & coord_flag::CURVE_START != 0 && i + 3 < coords.len() {
            // The two control points belong to this subpath; the curve's end
            // is read next time round, so its own flags are honoured.
            current.push(coords[i + 1]);
            current.push(coords[i + 2]);
            i += 3;
        } else {
            i += 1;
        }
    }
    if !current.is_empty() {
        closed_flags.push(false);
        raw_parts.push(current);
    }

    for (part, closed) in raw_parts.into_iter().zip(closed_flags) {
        if part.is_empty() {
            continue;
        }
        let mut pts: Vec<(f64, f64)> = vec![(part[0].x, part[0].y)];
        let mut k = 0;
        while k + 1 < part.len() {
            if part[k].flags & coord_flag::CURVE_START != 0 && k + 3 < part.len() {
                flatten_cubic(
                    (part[k].x, part[k].y),
                    (part[k + 1].x, part[k + 1].y),
                    (part[k + 2].x, part[k + 2].y),
                    (part[k + 3].x, part[k + 3].y),
                    0,
                    &mut pts,
                );
                k += 3;
            } else {
                pts.push((part[k + 1].x, part[k + 1].y));
                k += 1;
            }
        }
        if closed && pts.len() > 1 && dist(pts[0], pts[pts.len() - 1]) > 1e-9 {
            pts.push(pts[0]);
        }
        if pts.len() > 1 {
            parts.push(pts);
        }
    }
    parts
}

fn orient(a: (f64, f64), b: (f64, f64), p: (f64, f64)) -> f64 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

/// Where two segments cross, if they properly do -- an endpoint touching
/// another segment is not a crossing, which is what lets a line be drawn in
/// several pieces.
fn segments_cross(
    a: (f64, f64),
    b: (f64, f64),
    c: (f64, f64),
    d: (f64, f64),
) -> Option<(f64, f64)> {
    let d1 = orient(c, d, a);
    let d2 = orient(c, d, b);
    let d3 = orient(a, b, c);
    let d4 = orient(a, b, d);
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        let t = d1 / (d1 - d2);
        return Some((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
    }
    None
}

/// The nearest point of a segment to a point, and how far away it is.
fn point_seg_closest(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> (f64, (f64, f64)) {
    let vx = b.0 - a.0;
    let vy = b.1 - a.1;
    let len_sq = vx * vx + vy * vy;
    let t = if len_sq > 0.0 {
        (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / len_sq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let q = (a.0 + vx * t, a.1 + vy * t);
    let dx = p.0 - q.0;
    let dy = p.1 - q.1;
    (dx * dx + dy * dy, q)
}

/// How near two segments come to each other, and where. Zero where they
/// cross.
fn seg_seg_closest(
    a: (f64, f64),
    b: (f64, f64),
    c: (f64, f64),
    d: (f64, f64),
) -> (f64, (f64, f64)) {
    if let Some(hit) = segments_cross(a, b, c, d) {
        return (0.0, hit);
    }
    let (mut best_d2, mut best_q) = point_seg_closest(a, c, d);
    let mut best_p = a;
    for (p, s0, s1) in [(b, c, d), (c, a, b), (d, a, b)] {
        let (d2, q) = point_seg_closest(p, s0, s1);
        if d2 < best_d2 {
            best_d2 = d2;
            best_q = q;
            best_p = p;
        }
    }
    (
        best_d2.sqrt(),
        ((best_p.0 + best_q.0) / 2.0, (best_p.1 + best_q.1) / 2.0),
    )
}

/// Whether a point is inside a set of rings, counting crossings -- so a ring
/// inside another is a hole.
fn point_in_rings(x: f64, y: f64, rings: &[Vec<(f64, f64)>]) -> bool {
    let mut inside = false;
    for ring in rings {
        let n = ring.len();
        if n < 2 {
            continue;
        }
        let mut j = n - 1;
        for i in 0..n {
            let (xi, yi) = ring[i];
            let (xj, yj) = ring[j];
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
    }
    inside
}

/// A uniform grid of line segments, so that a segment is only measured
/// against the ones near it rather than against all of them.
struct SegmentGrid {
    ax: Vec<f64>,
    ay: Vec<f64>,
    bx: Vec<f64>,
    by: Vec<f64>,
    /// Whose segment each one is: an index the caller gives meaning to.
    owner: Vec<usize>,
    cells: HashMap<(i64, i64), Vec<usize>>,
    cell_size: f64,
}

impl SegmentGrid {
    fn new(cell_size: f64) -> SegmentGrid {
        SegmentGrid {
            ax: Vec::new(),
            ay: Vec::new(),
            bx: Vec::new(),
            by: Vec::new(),
            owner: Vec::new(),
            cells: HashMap::new(),
            cell_size: cell_size.max(f64::MIN_POSITIVE),
        }
    }

    /// Adds a segment, in every cell its box reaches once grown by `inflate`
    /// -- which is how far away another segment could still be too close.
    fn insert(&mut self, a: (f64, f64), b: (f64, f64), owner: usize, inflate: f64) {
        let index = self.ax.len();
        self.ax.push(a.0);
        self.ay.push(a.1);
        self.bx.push(b.0);
        self.by.push(b.1);
        self.owner.push(owner);
        let cell = |v: f64| (v / self.cell_size).floor() as i64;
        let (min_cx, max_cx) = (cell(a.0.min(b.0) - inflate), cell(a.0.max(b.0) + inflate));
        let (min_cy, max_cy) = (cell(a.1.min(b.1) - inflate), cell(a.1.max(b.1) + inflate));
        for cx in min_cx..=max_cx {
            for cy in min_cy..=max_cy {
                self.cells.entry((cx, cy)).or_default().push(index);
            }
        }
    }

    fn seg(&self, i: usize) -> ((f64, f64), (f64, f64)) {
        ((self.ax[i], self.ay[i]), (self.bx[i], self.by[i]))
    }
}

// ---------------------------------------------------------------------------
// Contours

/// Most crossings reported before the rest are counted rather than listed.
const MAX_CROSSING_ISSUES: usize = 100;
/// Most forbidden overlaps reported.
const MAX_OVERLAP_ISSUES: usize = 100;
/// Most gaps reported, narrowest first.
const MAX_GAP_ISSUES: usize = 200;

/// Finds contours crossing one another.
///
/// A contour is a line of constant height, so two of them crossing says the
/// ground is at two heights in one place. Contours drawn in several pieces
/// meeting end to end are fine, which is why only proper crossings count.
pub fn validate_contour_crossings(
    map: &Map,
    reference: &Reference,
    should_stop: &dyn Fn() -> bool,
) -> StageResult {
    struct Entry {
        object_index: usize,
        code: String,
    }
    let mut entries: Vec<Entry> = Vec::new();
    // Two millimetres: wide enough that few segments share a cell, small
    // enough that few cells hold many.
    let mut grid = SegmentGrid::new(2.0);

    for (object_index, object) in map.objects.iter().enumerate() {
        let Some(symbol) = symbol_of(map, object) else {
            continue;
        };
        if !reference.no_cross_codes.contains(base_code(symbol.code())) {
            continue;
        }
        if !leaves_of(symbol, map)
            .iter()
            .any(|l| matches!(l, Symbol::Line(_)))
        {
            continue;
        }
        let entry_index = entries.len();
        entries.push(Entry {
            object_index,
            code: symbol.code().to_string(),
        });
        for line in flatten_object(object) {
            for pair in line.windows(2) {
                grid.insert(pair[0], pair[1], entry_index, 0.0);
            }
        }
    }

    let mut reported: Vec<(usize, usize, (f64, f64), usize)> = Vec::new();
    let mut reported_index: HashMap<(usize, usize), usize> = HashMap::new();
    // One segment pair may share several cells; each crossing counts once.
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut truncated = false;
    let mut checked = 0usize;

    for bucket in grid.cells.values() {
        checked += 1;
        if checked.is_multiple_of(256) && should_stop() {
            truncated = true;
            break;
        }
        for i in 0..bucket.len() {
            for j in i + 1..bucket.len() {
                let (si, sj) = (bucket[i], bucket[j]);
                if grid.owner[si] == grid.owner[sj] {
                    continue;
                }
                let seg_key = (si.min(sj), si.max(sj));
                if !seen.insert(seg_key) {
                    continue;
                }
                let (a, b) = grid.seg(si);
                let (c, d) = grid.seg(sj);
                let Some(hit) = segments_cross(a, b, c, d) else {
                    continue;
                };
                let (oa, ob) = (grid.owner[si], grid.owner[sj]);
                let key = (oa.min(ob), oa.max(ob));
                match reported_index.get(&key) {
                    Some(&at) => reported[at].3 += 1,
                    None => {
                        reported_index.insert(key, reported.len());
                        reported.push((key.0, key.1, hit, 1));
                    }
                }
            }
        }
    }

    let mut issues = Vec::new();
    for &(a, b, hit, count) in reported.iter().take(MAX_CROSSING_ISSUES) {
        let (ea, eb) = (&entries[a], &entries[b]);
        let mut issue = Issue::new(
            Severity::Error,
            Category::ContourIntersection,
            ea.code.clone(),
            format!(
                "{} intersects {}{} — contours must never cross",
                describe_code(&ea.code, reference),
                describe_code(&eb.code, reference),
                if count > 1 {
                    format!(" ({count} crossings)")
                } else {
                    String::new()
                }
            ),
        );
        issue.code2 = Some(eb.code.clone());
        issue.object_index = Some(ea.object_index);
        issue.object_index2 = Some(eb.object_index);
        issue.location = Some(hit);
        issue.radius = Some(1.5);
        issues.push(issue);
    }
    if reported.len() > MAX_CROSSING_ISSUES {
        issues.push(Issue::new(
            Severity::Info,
            Category::Process,
            "contours",
            format!(
                "{} further contour crossings omitted",
                reported.len() - MAX_CROSSING_ISSUES
            ),
        ));
    }
    StageResult { issues, truncated }
}

// ---------------------------------------------------------------------------
// Forbidden overlaps

struct AreaEntry {
    object_index: usize,
    code: String,
    base: String,
    rings: Vec<Vec<(f64, f64)>>,
    bounds: Bounds,
}

fn pair_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// Whether two sets of rings overlap: an edge crossing, or one inside the
/// other with no edges meeting at all.
fn rings_overlap(a: &AreaEntry, b: &AreaEntry) -> Option<(f64, f64)> {
    for ring_a in &a.rings {
        for pair in ring_a.windows(2) {
            let seg_bounds = Bounds {
                min_x: pair[0].0.min(pair[1].0),
                min_y: pair[0].1.min(pair[1].1),
                max_x: pair[0].0.max(pair[1].0),
                max_y: pair[0].1.max(pair[1].1),
            };
            if !seg_bounds.overlaps(&b.bounds) {
                continue;
            }
            for ring_b in &b.rings {
                for other in ring_b.windows(2) {
                    if let Some(hit) = segments_cross(pair[0], pair[1], other[0], other[1]) {
                        return Some(hit);
                    }
                }
            }
        }
    }
    // No edges met: one may still lie wholly within the other.
    if let Some(&p) = a.rings.first().and_then(|r| r.first()) {
        if point_in_rings(p.0, p.1, &b.rings) {
            return Some(p);
        }
    }
    if let Some(&p) = b.rings.first().and_then(|r| r.first()) {
        if point_in_rings(p.0, p.1, &a.rings) {
            return Some(p);
        }
    }
    None
}

/// Finds areas overlapping which the standard says may not.
///
/// Two screens printed over each other make a third colour, and a standard
/// lays down which combinations are allowed to happen: a marsh over another
/// marsh reads as neither.
pub fn validate_area_overlaps(
    map: &Map,
    reference: &Reference,
    should_stop: &dyn Fn() -> bool,
) -> StageResult {
    let forbidden: HashSet<(String, String)> = reference
        .forbidden_area_overlaps
        .iter()
        .map(|(a, b)| pair_key(a, b))
        .collect();
    let relevant: HashSet<&str> = reference
        .forbidden_area_overlaps
        .iter()
        .flat_map(|(a, b)| [a.as_str(), b.as_str()])
        .collect();

    let mut entries: Vec<AreaEntry> = Vec::new();
    for (object_index, object) in map.objects.iter().enumerate() {
        let Some(symbol) = symbol_of(map, object) else {
            continue;
        };
        let base = base_code(symbol.code());
        if !relevant.contains(base) {
            continue;
        }
        if !leaves_of(symbol, map)
            .iter()
            .any(|l| matches!(l, Symbol::Area(_)))
        {
            continue;
        }
        let rings: Vec<Vec<(f64, f64)>> = flatten_object(object)
            .into_iter()
            .filter(|r| r.len() > 2)
            .collect();
        if rings.is_empty() {
            continue;
        }
        entries.push(AreaEntry {
            object_index,
            code: symbol.code().to_string(),
            base: base.to_string(),
            bounds: Bounds::of(&rings),
            rings,
        });
    }

    let mut issues = Vec::new();
    let mut truncated = false;
    'outer: for i in 0..entries.len() {
        if i.is_multiple_of(16) && should_stop() {
            truncated = true;
            break;
        }
        for j in i + 1..entries.len() {
            let (a, b) = (&entries[i], &entries[j]);
            if !a.bounds.overlaps(&b.bounds) {
                continue;
            }
            if !forbidden.contains(&pair_key(&a.base, &b.base)) {
                continue;
            }
            let Some(hit) = rings_overlap(a, b) else {
                continue;
            };
            let mut issue = Issue::new(
                Severity::Warning,
                Category::AreaOverlap,
                a.code.clone(),
                if a.base == b.base {
                    format!(
                        "Two {} areas overlap — this screen must not be combined with itself",
                        describe_code(&a.code, reference)
                    )
                } else {
                    format!(
                        "{} overlaps {} — combination not permitted (ISOM §2.11.4)",
                        describe_code(&a.code, reference),
                        describe_code(&b.code, reference)
                    )
                },
            );
            issue.code2 = Some(b.code.clone());
            issue.object_index = Some(a.object_index);
            issue.object_index2 = Some(b.object_index);
            issue.location = Some(hit);
            issue.radius = Some(2.0);
            issues.push(issue);
            if issues.len() >= MAX_OVERLAP_ISSUES {
                truncated = true;
                break 'outer;
            }
        }
    }
    StageResult { issues, truncated }
}

// ---------------------------------------------------------------------------
// Minimum gaps

/// One thing the gap rule applies to, and how wide it is drawn.
struct GapParticipant {
    object_index: usize,
    code: String,
    base: String,
    impassable: bool,
    /// Half the width the symbol is drawn at, so that the gap between two
    /// symbols is the distance between their centrelines less both halves.
    half_width: f64,
    polylines: Vec<Vec<(f64, f64)>>,
}

/// Everything the minimum-gap rule applies to.
///
/// Every line, at the width it is actually drawn -- borders included -- and
/// those area symbols printed in full colour, whose edge is a real edge on the
/// ground. A screen fill has no edge to speak of and is exempt, as are the
/// overprint colours a course is drawn in, which are not part of the map.
fn gap_participants(map: &Map, reference: &Reference) -> Vec<GapParticipant> {
    let mut participants = Vec::new();
    for (object_index, object) in map.objects.iter().enumerate() {
        let Some(symbol) = symbol_of(map, object) else {
            continue;
        };
        let code = symbol.code();
        let base = base_code(code);
        // 600 and up is the course overprint and the technical symbols.
        let Ok(base_number) = base.parse::<f64>() else {
            continue;
        };
        if base_number >= 600.0 {
            continue;
        }

        let mut half_width = 0f64;
        let mut has_line = false;
        let mut has_area = false;
        for leaf in leaves_of(symbol, map) {
            match leaf {
                Symbol::Line(line) if line.line_width > 0.0 => {
                    has_line = true;
                    let mut half = line.line_width / 2.0;
                    for border in [&line.border, &line.right_border] {
                        if has_border(border) {
                            half = half.max(border.shift.abs() + border.width / 2.0);
                        }
                    }
                    half_width = half_width.max(half);
                }
                Symbol::Area(_) => has_area = true,
                _ => {}
            }
        }
        // A boulder field's dots are drawn as an area but read as solid rock.
        let solid_area = has_area && (reference.impassable_codes.contains(base) || base == "206");
        if !has_line && !solid_area {
            continue;
        }

        let polylines: Vec<Vec<(f64, f64)>> = flatten_object(object)
            .into_iter()
            .filter(|p| p.len() > 1)
            .collect();
        if polylines.is_empty() {
            continue;
        }
        participants.push(GapParticipant {
            object_index,
            code: code.to_string(),
            base: base.to_string(),
            impassable: reference.impassable_codes.contains(base),
            half_width: if has_line { half_width } else { 0.0 },
            polylines,
        });
    }
    participants
}

/// Finds symbols drawn closer together than the standard allows.
///
/// Below a certain gap two marks merge into one at printing size, and the map
/// says something it did not mean. What is measured is the gap between what is
/// actually printed: the distance between two centrelines, less half of each
/// line's width.
///
/// Symbols which touch or cross are exempt as a whole. That is not a
/// concession but the rule: a standard's gap requirement is about two separate
/// things being too close, and a junction, a crossing or a shared border is
/// one thing.
pub fn validate_gaps(
    map: &Map,
    reference: &Reference,
    should_stop: &dyn Fn() -> bool,
) -> StageResult {
    let f = dimension_factor(map, reference);
    let general_gap = reference.min_gap_mm * f;
    let impassable_gap = reference.min_gap_impassable_mm * f;

    let exempt: HashSet<(String, String)> = reference
        .gap_exempt_pairs
        .iter()
        .map(|(a, b)| pair_key(a, b))
        .collect();
    let participants = gap_participants(map, reference);
    let max_half = participants.iter().fold(0f64, |m, p| m.max(p.half_width));

    // A cell has to be wide enough that two segments too close to each other
    // always share one.
    let reach = impassable_gap + 2.0 * max_half;
    let mut grid = SegmentGrid::new(reach.max(0.5));
    for (i, participant) in participants.iter().enumerate() {
        let inflate = participant.half_width + impassable_gap / 2.0;
        for line in &participant.polylines {
            for pair in line.windows(2) {
                grid.insert(pair[0], pair[1], i, inflate);
            }
        }
    }

    /// The closest two objects come, and where.
    struct Hit {
        gap: f64,
        at: (f64, f64),
        a: usize,
        b: usize,
        threshold: f64,
    }
    let mut pair_best: HashMap<(usize, usize), Hit> = HashMap::new();
    let mut truncated = false;
    let mut checked = 0usize;

    for bucket in grid.cells.values() {
        checked += 1;
        if checked.is_multiple_of(64) && should_stop() {
            truncated = true;
            break;
        }
        for i in 0..bucket.len() {
            let si = bucket[i];
            let pa = &participants[grid.owner[si]];
            for &sj in &bucket[i + 1..] {
                if grid.owner[si] == grid.owner[sj] {
                    continue;
                }
                let pb = &participants[grid.owner[sj]];
                if !exempt.is_empty() && exempt.contains(&pair_key(&pa.base, &pb.base)) {
                    continue;
                }
                let threshold = if pa.impassable && pb.impassable {
                    impassable_gap
                } else {
                    general_gap
                };
                let (a, b) = grid.seg(si);
                let (c, d) = grid.seg(sj);
                let (distance, at) = seg_seg_closest(a, b, c, d);
                let gap = distance - pa.half_width - pb.half_width;
                if gap >= threshold {
                    continue;
                }
                let key = (
                    grid.owner[si].min(grid.owner[sj]),
                    grid.owner[si].max(grid.owner[sj]),
                );
                let entry = pair_best.entry(key).or_insert(Hit {
                    gap: f64::INFINITY,
                    at,
                    a: key.0,
                    b: key.1,
                    threshold,
                });
                if gap < entry.gap {
                    entry.gap = gap;
                    entry.at = at;
                    entry.threshold = threshold;
                }
            }
        }
    }

    // A pair which touches anywhere is joined, and its other near approaches
    // are the two lines diverging from that join rather than a fault.
    let mut hits: Vec<&Hit> = pair_best
        .values()
        .filter(|h| h.gap > TOUCH_EPSILON_MM)
        .collect();
    hits.sort_by(|a, b| {
        a.gap
            .partial_cmp(&b.gap)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| (a.a, a.b).cmp(&(b.a, b.b)))
    });

    let mut issues = Vec::new();
    for hit in hits.iter().take(MAX_GAP_ISSUES) {
        let pa = &participants[hit.a];
        let pb = &participants[hit.b];
        let mut issue = Issue::new(
            Severity::Warning,
            Category::Gap,
            pa.code.clone(),
            format!(
                "Gap of {:.2} mm between {} and {} (minimum {:.2} mm)",
                hit.gap,
                describe_code(&pa.code, reference),
                describe_code(&pb.code, reference),
                hit.threshold
            ),
        );
        issue.code2 = Some(pb.code.clone());
        issue.object_index = Some(pa.object_index);
        issue.object_index2 = Some(pb.object_index);
        issue.location = Some(hit.at);
        issue.radius = Some(hit.threshold);
        issues.push(issue);
    }
    if hits.len() > MAX_GAP_ISSUES {
        issues.push(Issue::new(
            Severity::Info,
            Category::Process,
            "gaps",
            format!(
                "{} further gap issues omitted (showing the {MAX_GAP_ISSUES} narrowest)",
                hits.len() - MAX_GAP_ISSUES
            ),
        ));
    }
    StageResult { issues, truncated }
}

// ---------------------------------------------------------------------------
// Running the checks

/// Runs one check.
///
/// `should_stop` is asked as the geometric checks go, so a caller can put a
/// limit on how long it will wait; a check that stops early says so in
/// [`StageResult::truncated`]. The other checks never ask.
pub fn validate_stage(
    map: &Map,
    reference: &Reference,
    stage: Stage,
    should_stop: &dyn Fn() -> bool,
) -> StageResult {
    match stage {
        Stage::Colors => StageResult {
            issues: validate_colors(map, reference),
            truncated: false,
        },
        Stage::Symbols => StageResult {
            issues: validate_symbols(map, reference),
            truncated: false,
        },
        Stage::PointRotations => StageResult {
            issues: validate_point_rotations(map, reference),
            truncated: false,
        },
        Stage::ContourCrossings => validate_contour_crossings(map, reference, should_stop),
        Stage::AreaOverlaps => validate_area_overlaps(map, reference, should_stop),
        Stage::Gaps => validate_gaps(map, reference, should_stop),
    }
}

/// What a whole run of the checks found.
#[derive(Debug, Default)]
pub struct Report {
    /// Everything found, most serious first.
    pub issues: Vec<Issue>,
    /// How many say the map is wrong.
    pub errors: usize,
    /// How many say the map departs from the standard.
    pub warnings: usize,
    /// How many are only worth knowing.
    pub infos: usize,
    /// Whether any check stopped early.
    pub truncated: bool,
}

/// Runs every check, in order.
pub fn validate(map: &Map, reference: &Reference, should_stop: &dyn Fn() -> bool) -> Report {
    let mut issues = Vec::new();
    let mut truncated = false;
    for stage in Stage::ALL {
        let result = validate_stage(map, reference, stage, should_stop);
        issues.extend(result.issues);
        truncated |= result.truncated;
    }
    if truncated {
        issues.push(Issue::new(
            Severity::Info,
            Category::Process,
            "budget",
            "Geometric checks stopped early on their time budget — results may be incomplete"
                .to_string(),
        ));
    }
    sort_issues(&mut issues);
    let mut report = Report {
        errors: issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .count(),
        warnings: issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
            .count(),
        infos: issues
            .iter()
            .filter(|i| i.severity == Severity::Info)
            .count(),
        issues,
        truncated,
    };
    report.issues.shrink_to_fit();
    report
}

/// Orders findings the way a reader wants them: the serious ones first, then
/// by what they are about.
pub fn sort_issues(issues: &mut [Issue]) {
    fn rank(severity: Severity) -> u8 {
        match severity {
            Severity::Error => 0,
            Severity::Warning => 1,
            Severity::Info => 2,
        }
    }
    issues.sort_by(|a, b| {
        rank(a.severity)
            .cmp(&rank(b.severity))
            .then_with(|| a.category.name().cmp(b.category.name()))
            .then_with(|| natural_cmp(&a.code, &b.code))
    });
}

/// Compares two codes the way a reader expects: `40` before `403` before
/// `403.1`, with runs of digits compared as numbers.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.as_bytes(), b.as_bytes());
    loop {
        match (ai.first(), bi.first()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let a_end = ai
                        .iter()
                        .position(|c| !c.is_ascii_digit())
                        .unwrap_or(ai.len());
                    let b_end = bi
                        .iter()
                        .position(|c| !c.is_ascii_digit())
                        .unwrap_or(bi.len());
                    let a_digits = ai[..a_end].iter().skip_while(|&&c| c == b'0').count();
                    let b_digits = bi[..b_end].iter().skip_while(|&&c| c == b'0').count();
                    let order = a_digits
                        .cmp(&b_digits)
                        .then_with(|| ai[..a_end].cmp(&bi[..b_end]));
                    if order != std::cmp::Ordering::Equal {
                        return order;
                    }
                    ai = &ai[a_end..];
                    bi = &bi[b_end..];
                } else {
                    let order = x.cmp(y);
                    if order != std::cmp::Ordering::Equal {
                        return order;
                    }
                    ai = &ai[1..];
                    bi = &bi[1..];
                }
            }
        }
    }
}
