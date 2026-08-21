//! That a map is checked against a standard the way a mapper would check it.
//!
//! The standard used here is mostly the fixture map itself: a map compared
//! against its own symbol set must have nothing wrong with it, which is the
//! strongest single thing to know about a checker. The rest are small maps
//! written out in full, where what should be found is plain to see.

use std::collections::{HashMap, HashSet};

use maur_o::map::{Map, Symbol};
use maur_o::validate::{validate, validate_stage, Category, Reference, Severity, Stage};
use maur_o::xml_reader::read_xml_map_str;

/// Never stop early: these maps are small enough to finish.
fn run_on() -> impl Fn() -> bool {
    || false
}

/// A map written out in full, so a test can say exactly what is on it.
fn map_of(colors: &str, symbols: &str, objects: &str) -> Map {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<map xmlns="http://openorienteering.org/apps/mapper/xml/v2" version="9">
<georeferencing scale="15000"/>
<colors count="1">{colors}</colors>
<barrier version="6" required="0.6.0">
<symbols count="1" id="test">{symbols}</symbols>
<parts count="1" current="0"><part name="default part"><objects count="1">{objects}</objects></part></parts>
</barrier></map>"#
    );
    read_xml_map_str(&xml)
        .unwrap_or_else(|e| panic!("the fixture does not parse: {e}"))
        .0
}

const BROWN: &str =
    r#"<color priority="0" name="Brown" c="0" m="0.56" y="1" k="0.18" opacity="1"/>"#;

struct Standard {
    golden: Map,
    /// The scale the standard's dimensions are defined at.
    base_scale: f64,
    descriptions: HashMap<String, String>,
    impassable: HashSet<String>,
    no_cross: HashSet<String>,
    forbidden: Vec<(String, String)>,
    exempt: Vec<(String, String)>,
}

impl Standard {
    fn of(golden: Map) -> Standard {
        Standard {
            golden,
            base_scale: 15000.0,
            descriptions: HashMap::new(),
            impassable: HashSet::new(),
            no_cross: HashSet::new(),
            forbidden: Vec::new(),
            exempt: Vec::new(),
        }
    }

    fn reference(&self) -> Reference<'_> {
        Reference {
            golden: &self.golden,
            base_scale: self.base_scale,
            code_descriptions: &self.descriptions,
            impassable_codes: &self.impassable,
            no_cross_codes: &self.no_cross,
            forbidden_area_overlaps: &self.forbidden,
            gap_exempt_pairs: &self.exempt,
            min_gap_mm: 0.15,
            min_gap_impassable_mm: 0.3,
        }
    }
}

/// A line symbol, and an object drawn with it along the given points.
fn line_map(code: &str, width: i64, coords: &str) -> Map {
    map_of(
        BROWN,
        &format!(
            r#"<symbol type="2" id="0" code="{code}" name="Line"><line_symbol color="0" line_width="{width}" cap_style="0" join_style="0"/></symbol>"#
        ),
        &format!(r#"<object type="1" symbol="0"><coords count="2">{coords}</coords></object>"#),
    )
}

#[test]
fn a_map_measured_against_its_own_symbols_is_faultless() {
    let map = read_xml_map_str(&std::fs::read_to_string("tests/data/shapes.xmap").unwrap())
        .unwrap()
        .0;
    let golden = read_xml_map_str(&std::fs::read_to_string("tests/data/shapes.xmap").unwrap())
        .unwrap()
        .0;
    let mut standard = Standard::of(golden);
    // The fixture is drawn at 1:10000; a standard defined at another scale
    // would have every dimension legitimately scaled against it.
    standard.base_scale = f64::from(map.scale_denominator);

    for stage in [Stage::Colors, Stage::Symbols, Stage::PointRotations] {
        let result = validate_stage(&map, &standard.reference(), stage, &run_on());
        let faults: Vec<&str> = result
            .issues
            .iter()
            .filter(|i| i.severity != Severity::Info)
            .map(|i| i.message.as_str())
            .collect();
        assert!(faults.is_empty(), "{}: {faults:?}", stage.name());
    }
}

#[test]
fn a_symbol_drawn_differently_is_reported_field_by_field() {
    let map = line_map("101", 200, "0 0;1000 0;");
    let standard = Standard::of(line_map("101", 140, "0 0;1000 0;"));

    let issues = validate_stage(&map, &standard.reference(), Stage::Symbols, &run_on()).issues;
    let issue = issues
        .iter()
        .find(|i| i.category == Category::ModifiedSymbol)
        .expect("a wider line should be reported");
    assert_eq!(issue.severity, Severity::Warning);
    assert_eq!(issue.code, "101");
    assert!(
        issue.details.iter().any(|d| d.contains("line width")),
        "the field should be named: {:?}",
        issue.details
    );
}

#[test]
fn a_symbol_the_standard_does_not_have_is_an_error_when_it_is_used() {
    let map = line_map("999", 140, "0 0;1000 0;");
    let standard = Standard::of(line_map("101", 140, "0 0;1000 0;"));

    let issues = validate_stage(&map, &standard.reference(), Stage::Symbols, &run_on()).issues;
    let issue = issues
        .iter()
        .find(|i| i.category == Category::UnknownSymbol)
        .expect("an unknown symbol should be reported");
    assert_eq!(issue.severity, Severity::Error);
    assert!(issue.location.is_some(), "and it should say where to look");
}

#[test]
fn a_numbered_variant_of_a_standard_symbol_is_allowed() {
    let map = line_map("101.1", 140, "0 0;1000 0;");
    let standard = Standard::of(line_map("101", 140, "0 0;1000 0;"));

    let issues = validate_stage(&map, &standard.reference(), Stage::Symbols, &run_on()).issues;
    let issue = issues
        .iter()
        .find(|i| i.category == Category::SymbolVariant)
        .expect("a variant should be recognized as one");
    assert_eq!(issue.severity, Severity::Info);
    assert!(!issues.iter().any(|i| i.category == Category::UnknownSymbol));
}

#[test]
fn a_colour_mixed_differently_is_reported_channel_by_channel() {
    let map = line_map("101", 140, "0 0;1000 0;");
    let mut golden = line_map("101", 140, "0 0;1000 0;");
    golden.colors[0].cmyk.1 = 0.30;
    let standard = Standard::of(golden);

    let issues = validate_stage(&map, &standard.reference(), Stage::Colors, &run_on()).issues;
    let issue = issues
        .iter()
        .find(|i| i.category == Category::ColorModified)
        .expect("a changed ink should be reported");
    assert!(
        issue.details.iter().any(|d| d.starts_with("magenta")),
        "the channel should be named: {:?}",
        issue.details
    );
}

#[test]
fn ink_is_compared_to_the_decimal_the_file_is_written_to() {
    // Two colours a twentieth of a percent apart are not the same colour,
    // and saying so must not depend on how the numbers are stored.
    let map = line_map("101", 140, "0 0;1000 0;");
    let mut golden = line_map("101", 140, "0 0;1000 0;");
    golden.colors[0].name = "Different name".to_string();
    golden.colors[0].cmyk.1 = 0.56 - 0.005;
    let standard = Standard::of(golden);

    let issues = validate_stage(&map, &standard.reference(), Stage::Colors, &run_on()).issues;
    let extra = issues
        .iter()
        .find(|i| i.category == Category::ColorExtra)
        .expect("a colour the standard does not name should be reported");
    assert!(
        !extra.message.contains("renamed"),
        "0.005 apart is not the same colour: {}",
        extra.message
    );
}

#[test]
fn contours_crossing_is_an_error() {
    let map = map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="101" name="Contour"><line_symbol color="0" line_width="140" cap_style="0" join_style="0"/></symbol>"#,
        r#"<object type="1" symbol="0"><coords count="2">0 0;10000 10000;</coords></object>
           <object type="1" symbol="0"><coords count="2">0 10000;10000 0;</coords></object>"#,
    );
    let mut standard = Standard::of(map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="101" name="Contour"><line_symbol color="0" line_width="140" cap_style="0" join_style="0"/></symbol>"#,
        "",
    ));
    standard.no_cross.insert("101".to_string());

    let result = validate_stage(
        &map,
        &standard.reference(),
        Stage::ContourCrossings,
        &run_on(),
    );
    let issue = result
        .issues
        .iter()
        .find(|i| i.category == Category::ContourIntersection)
        .expect("two contours crossing should be found");
    assert_eq!(issue.severity, Severity::Error);
    // They cross in the middle.
    let (x, y) = issue.location.expect("it should say where");
    assert!((x - 5.0).abs() < 0.01 && (y - 5.0).abs() < 0.01, "{x},{y}");
    assert!(!result.truncated);
}

#[test]
fn contours_meeting_end_to_end_are_not_crossing() {
    // A contour drawn in two pieces is one contour.
    let map = map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="101" name="Contour"><line_symbol color="0" line_width="140" cap_style="0" join_style="0"/></symbol>"#,
        r#"<object type="1" symbol="0"><coords count="2">0 0;5000 0;</coords></object>
           <object type="1" symbol="0"><coords count="2">5000 0;10000 0;</coords></object>"#,
    );
    let mut standard = Standard::of(map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="101" name="Contour"><line_symbol color="0" line_width="140"/></symbol>"#,
        "",
    ));
    standard.no_cross.insert("101".to_string());

    let result = validate_stage(
        &map,
        &standard.reference(),
        Stage::ContourCrossings,
        &run_on(),
    );
    assert!(result.issues.is_empty(), "{:?}", result.issues);
}

#[test]
fn two_lines_too_close_together_are_reported() {
    // Two parallel lines 0.1 mm apart, which is under the 0.15 mm minimum.
    let map = map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="501" name="Road"><line_symbol color="0" line_width="20" cap_style="0" join_style="0"/></symbol>"#,
        r#"<object type="1" symbol="0"><coords count="2">0 0;10000 0;</coords></object>
           <object type="1" symbol="0"><coords count="2">0 100;10000 100;</coords></object>"#,
    );
    let standard = Standard::of(map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="501" name="Road"><line_symbol color="0" line_width="20"/></symbol>"#,
        "",
    ));

    let result = validate_stage(&map, &standard.reference(), Stage::Gaps, &run_on());
    let issue = result
        .issues
        .iter()
        .find(|i| i.category == Category::Gap)
        .expect("a gap under the minimum should be found");
    assert!(issue.message.contains("0.08 mm"), "{}", issue.message);
    assert_eq!(issue.code, "501");
    assert_eq!(issue.code2.as_deref(), Some("501"));
}

#[test]
fn lines_that_meet_are_joined_rather_than_too_close() {
    // Two lines sharing an end are a junction, which every standard allows.
    let map = map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="501" name="Road"><line_symbol color="0" line_width="20" cap_style="0" join_style="0"/></symbol>"#,
        r#"<object type="1" symbol="0"><coords count="2">0 0;10000 0;</coords></object>
           <object type="1" symbol="0"><coords count="2">10000 0;10000 10000;</coords></object>"#,
    );
    let standard = Standard::of(map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="501" name="Road"><line_symbol color="0" line_width="20"/></symbol>"#,
        "",
    ));

    let result = validate_stage(&map, &standard.reference(), Stage::Gaps, &run_on());
    assert!(
        !result.issues.iter().any(|i| i.category == Category::Gap),
        "a junction is not a gap: {:?}",
        result.issues
    );
}

#[test]
fn the_worst_is_reported_first() {
    let map = line_map("999", 200, "0 0;1000 0;");
    let mut golden = line_map("101", 140, "0 0;1000 0;");
    golden.colors[0].cmyk.1 = 0.30;
    let standard = Standard::of(golden);

    let report = validate(&map, &standard.reference(), &run_on());
    assert!(report.errors > 0 && report.warnings > 0);
    let severities: Vec<Severity> = report.issues.iter().map(|i| i.severity).collect();
    let rank = |s: Severity| match s {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    };
    for pair in severities.windows(2) {
        assert!(rank(pair[0]) <= rank(pair[1]), "{severities:?}");
    }
    assert_eq!(
        report.issues.len(),
        report.errors + report.warnings + report.infos
    );
}

#[test]
fn a_check_can_be_told_to_give_up() {
    let map = map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="101" name="Contour"><line_symbol color="0" line_width="140" cap_style="0" join_style="0"/></symbol>"#,
        r#"<object type="1" symbol="0"><coords count="2">0 0;10000 10000;</coords></object>
           <object type="1" symbol="0"><coords count="2">0 10000;10000 0;</coords></object>"#,
    );
    let mut standard = Standard::of(map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="101" name="Contour"><line_symbol color="0" line_width="140"/></symbol>"#,
        "",
    ));
    standard.no_cross.insert("101".to_string());

    // Stopping at once leaves the geometric checks with nothing to say, and
    // they must say that rather than pretend the map is clean.
    let report = validate(&map, &standard.reference(), &|| true);
    assert!(report.truncated);
    assert!(report.issues.iter().any(|i| i.code == "budget"));
}

#[test]
fn every_stage_is_named_and_weighted() {
    assert_eq!(Stage::ALL.len(), 6);
    for stage in Stage::ALL {
        assert!(!stage.name().is_empty());
        assert!(!stage.message().is_empty());
        assert!(stage.weight() > 0);
    }
}

/// A symbol which is defined and never drawn is worth mentioning, not
/// worrying about.
#[test]
fn an_unused_nonstandard_symbol_is_only_worth_mentioning() {
    let map = map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="998" name="Something else"><line_symbol color="0" line_width="140"/></symbol>"#,
        "",
    );
    let standard = Standard::of(line_map("101", 140, "0 0;1000 0;"));
    let issues = validate_stage(&map, &standard.reference(), Stage::Symbols, &run_on()).issues;
    let issue = issues
        .iter()
        .find(|i| i.category == Category::UnusedNonstandardSymbol)
        .expect("an unused symbol should be reported as such");
    assert_eq!(issue.severity, Severity::Info);
}

/// The golden map's own symbols are what a code is resolved against, so a
/// symbol set with two definitions under one code matches either.
#[test]
fn matching_any_definition_under_a_code_is_enough() {
    let map = line_map("101", 140, "0 0;1000 0;");
    let golden = map_of(
        BROWN,
        r#"<symbol type="2" id="0" code="101" name="Contour A"><line_symbol color="0" line_width="200"/></symbol>
           <symbol type="2" id="1" code="101" name="Contour B"><line_symbol color="0" line_width="140" cap_style="0" join_style="0"/></symbol>"#,
        "",
    );
    assert!(golden.symbols.iter().filter(|s| s.code() == "101").count() == 2);
    assert!(matches!(golden.symbols[1], Symbol::Line(_)));

    let standard = Standard::of(golden);
    let issues = validate_stage(&map, &standard.reference(), Stage::Symbols, &run_on()).issues;
    assert!(
        !issues
            .iter()
            .any(|i| i.category == Category::ModifiedSymbol),
        "{issues:?}"
    );
}
