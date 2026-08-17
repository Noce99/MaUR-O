//! The naming rules a benchmark archive has to follow, and how to repair an
//! archive which does not.
//!
//! A benchmark archive holds a `maps/` and an `expected/` folder. Every map
//! is named
//!
//! ```text
//! NNN__rest_of_the_name.omap
//! ```
//!
//! with a zero padded ordinal, two underscores, and a name free of spaces.
//! The ordinals run from zero upwards without a hole, all padded to the same
//! width, and `expected/` holds exactly the same names with a `.png` suffix.
//!
//! The point of the rules is that the ordinal orders the suite the same way
//! everywhere: sorted as text, `01_a` comes before `100_a` comes before
//! `02_a`, which is neither the order the maps were collected in nor any
//! other useful one. A padded ordinal sorts as text exactly as it sorts as a
//! number.
//!
//! Where an archive breaks the rules, [`plan`] says what the names should
//! have been instead, and why. It never invents files: a map without a
//! reference image, or a reference image without a map, is reported and left
//! alone, since only the archive's author can say which of the two is
//! missing.

/// The smallest ordinal width, so that a small suite is still named `000__`
/// rather than `0__`. A larger suite gets as many digits as it needs.
const MINIMUM_DIGITS: usize = 3;

/// The separator between the ordinal and the rest of the name.
const SEPARATOR: &str = "__";

/// One file of an archive, with the name the rules ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renaming {
    /// The file name as it is in the archive, without any folder.
    pub original: String,
    /// The file name it should have. Equal to `original` when it is already right.
    pub corrected: String,
    /// Why it had to change, empty when it did not.
    pub reasons: Vec<Reason>,
}

impl Renaming {
    /// Whether the file has to be renamed at all.
    pub fn changed(&self) -> bool {
        self.original != self.corrected
    }
}

/// One thing wrong with one file name.
///
/// The `kind` is what makes two problems the same problem, and the `detail`
/// is what makes this one worth printing: a suite whose ordinals are all one
/// off has one kind and as many details as it has maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    /// What kind of problem this is. Two files with the same `kind` broke the
    /// same rule, which is what lets a summary count them together.
    pub kind: &'static str,
    /// This file's own version of it, ready to print.
    pub detail: String,
}

impl Reason {
    fn new(kind: &'static str, detail: impl Into<String>) -> Self {
        Reason {
            kind,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// What [`plan`] made of an archive's contents.
#[derive(Debug, Default)]
pub struct Plan {
    /// The maps, in the order their ordinals will run.
    pub maps: Vec<Renaming>,
    /// The reference images, in the same order as `maps`, plus any which
    /// belong to no map at the end.
    pub expected: Vec<Renaming>,
    /// Maps with no reference image, by their original name. Not fixable.
    pub missing_expected: Vec<String>,
    /// Reference images belonging to no map, by their original name. Not
    /// fixable, and left under their original name.
    pub orphan_expected: Vec<String>,
}

impl Plan {
    /// Whether the archive already follows the rules, i.e. whether there is
    /// nothing to correct and nothing to complain about.
    pub fn is_valid(&self) -> bool {
        self.problems().is_empty()
    }

    /// Every problem found, one line each, ready to print.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (folder, renamings) in [("maps", &self.maps), ("expected", &self.expected)] {
            for renaming in renamings.iter().filter(|r| r.changed()) {
                let reasons: Vec<&str> =
                    renaming.reasons.iter().map(|r| r.detail.as_str()).collect();
                problems.push(format!(
                    "{folder}/{} -> {folder}/{}  ({})",
                    renaming.original,
                    renaming.corrected,
                    reasons.join("; ")
                ));
            }
        }
        for name in &self.missing_expected {
            problems.push(format!(
                "maps/{name} has no reference image in expected/ (not fixable)"
            ));
        }
        for name in &self.orphan_expected {
            problems.push(format!(
                "expected/{name} belongs to no map in maps/ (not fixable)"
            ));
        }
        problems
    }

    /// How often each kind of problem came up, most common first.
    ///
    /// A suite of a couple of hundred maps whose ordinals are all one off
    /// yields a couple of hundred near-identical lines from [`problems`],
    /// which says what happened to each file but not what happened to the
    /// archive. This says the latter.
    ///
    /// [`problems`]: Plan::problems
    pub fn summary(&self) -> Vec<(&'static str, usize)> {
        let mut counts: std::collections::BTreeMap<&'static str, usize> =
            std::collections::BTreeMap::new();
        for renaming in self.maps.iter().chain(&self.expected) {
            for reason in &renaming.reasons {
                *counts.entry(reason.kind).or_default() += 1;
            }
        }
        if !self.missing_expected.is_empty() {
            counts.insert("a map with no reference image", self.missing_expected.len());
        }
        if !self.orphan_expected.is_empty() {
            counts.insert(
                "a reference image belonging to no map",
                self.orphan_expected.len(),
            );
        }
        let mut summary: Vec<(&'static str, usize)> = counts.into_iter().collect();
        summary.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        summary
    }

    /// The corrected name for a file of `maps/`, or `None` if it is not one.
    pub fn corrected_map(&self, original: &str) -> Option<&str> {
        self.maps
            .iter()
            .find(|r| r.original == original)
            .map(|r| r.corrected.as_str())
    }

    /// The corrected name for a file of `expected/`, or `None` if it is not one.
    pub fn corrected_expected(&self, original: &str) -> Option<&str> {
        self.expected
            .iter()
            .find(|r| r.original == original)
            .map(|r| r.corrected.as_str())
    }
}

/// Splits a file name into its stem and its suffix, suffix including the dot.
///
/// Not `Path::extension`, which would cut a name like
/// `042__area_413.1_Orchard.omap` at the `.1` when there is no `.omap` — here
/// only a known map or image suffix counts as one.
fn split_suffix(name: &str) -> (&str, &str) {
    for suffix in [".omap", ".xmap", ".png"] {
        if name.len() > suffix.len()
            && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        {
            return (
                &name[..name.len() - suffix.len()],
                &name[name.len() - suffix.len()..],
            );
        }
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

/// The leading run of digits of a stem, what separated it from the rest, and
/// the rest. `("01", "_", "001_line_101")` for `01_001_line_101`.
fn split_ordinal(stem: &str) -> (&str, &str, &str) {
    let digits = stem.len() - stem.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let after = &stem[digits..];
    let separator = after.len() - after.trim_start_matches(['_', '-', ' ']).len();
    (&stem[..digits], &after[..separator], &after[separator..])
}

/// The rest of a name, with everything the rules forbid taken out of it.
fn sanitize(rest: &str) -> String {
    let cleaned: String = rest
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    if cleaned.is_empty() {
        "map".to_string()
    } else {
        cleaned
    }
}

/// Works out what every file in an archive should be called.
///
/// `map_files` and `expected_files` are plain file names, without their
/// folder. The order the maps end up in is the order of their existing
/// ordinals read as numbers — so `01_a`, `02_a`, `100_a` keeps that order
/// rather than being resorted as text — with anything that has no ordinal
/// sorted by name after them.
pub fn plan(map_files: &[String], expected_files: &[String]) -> Plan {
    let mut ordered: Vec<(Option<u64>, &String)> = map_files
        .iter()
        .map(|name| {
            let (stem, _) = split_suffix(name);
            let (digits, _, _) = split_ordinal(stem);
            (digits.parse::<u64>().ok(), name)
        })
        .collect();
    // `None` last: a map with no ordinal has nothing to say about where it
    // belongs, so it goes after everything that does, by name.
    ordered.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.1.cmp(b.1)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(b.1),
    });

    let width = MINIMUM_DIGITS.max(ordered.len().saturating_sub(1).to_string().len());

    let mut plan = Plan::default();
    let mut taken_expected = std::collections::HashSet::new();

    for (index, (ordinal, name)) in ordered.iter().enumerate() {
        let (stem, suffix) = split_suffix(name);
        let (digits, separator, rest) = split_ordinal(stem);

        let mut reasons = Vec::new();
        match ordinal {
            None => reasons.push(Reason::new(
                "no leading number",
                "does not start with a number",
            )),
            Some(value) => {
                if digits.len() != width {
                    reasons.push(Reason::new(
                        "leading number of the wrong width",
                        format!(
                            "leading number has {} digit{}, the suite needs {width}",
                            digits.len(),
                            if digits.len() == 1 { "" } else { "s" }
                        ),
                    ));
                }
                if *value != index as u64 {
                    reasons.push(Reason::new(
                        "leading number out of sequence",
                        format!("leading number {value} is out of sequence, expected {index}"),
                    ));
                }
            }
        }
        if ordinal.is_some() && separator != SEPARATOR {
            reasons.push(Reason::new(
                "wrong separator after the leading number",
                format!("{separator:?} after the leading number, expected {SEPARATOR:?}"),
            ));
        }
        if rest.chars().any(char::is_whitespace) {
            reasons.push(Reason::new("spaces in the name", "contains spaces"));
        }

        let corrected_stem = format!(
            "{:0width$}{SEPARATOR}{}",
            index,
            sanitize(rest),
            width = width
        );
        let corrected = format!("{corrected_stem}{suffix}");
        // A reason list can be empty while the name still changes (or the
        // other way round) only if something above is wrong; keep the two
        // consistent so that nothing is ever renamed without saying why.
        if corrected != **name && reasons.is_empty() {
            reasons.push(Reason::new(
                "does not match the naming rules",
                "does not match the naming rules",
            ));
        }
        plan.maps.push(Renaming {
            original: (*name).clone(),
            corrected,
            reasons,
        });

        let wanted = format!("{stem}.png");
        match expected_files.iter().find(|e| **e == wanted) {
            Some(found) => {
                taken_expected.insert(found.clone());
                let corrected = format!("{corrected_stem}.png");
                let reasons = if corrected == *wanted {
                    Vec::new()
                } else {
                    vec![Reason::new(
                        "renamed with the map it belongs to",
                        format!("follows maps/{}", plan.maps[index].corrected),
                    )]
                };
                plan.expected.push(Renaming {
                    original: found.clone(),
                    corrected,
                    reasons,
                });
            }
            None => plan.missing_expected.push((*name).clone()),
        }
    }

    for name in expected_files {
        if !taken_expected.contains(name) {
            plan.orphan_expected.push(name.clone());
            plan.expected.push(Renaming {
                original: name.clone(),
                corrected: name.clone(),
                reasons: Vec::new(),
            });
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_archive_which_follows_the_rules_is_left_alone() {
        let maps = names(&["000__a_map.omap", "001__another_map.omap"]);
        let expected = names(&["000__a_map.png", "001__another_map.png"]);
        let plan = plan(&maps, &expected);
        assert!(plan.is_valid(), "{:?}", plan.problems());
        assert_eq!(
            plan.corrected_map("001__another_map.omap"),
            Some("001__another_map.omap")
        );
    }

    #[test]
    fn ordinals_are_read_as_numbers_not_as_text() {
        // Sorted as text this is 01, 100, 02; the order to keep is 01, 02, 100.
        let maps = names(&["100_c.omap", "01_a.omap", "02_b.omap"]);
        let expected = names(&["100_c.png", "01_a.png", "02_b.png"]);
        let plan = plan(&maps, &expected);
        let corrected: Vec<&str> = plan.maps.iter().map(|r| r.corrected.as_str()).collect();
        assert_eq!(corrected, ["000__a.omap", "001__b.omap", "002__c.omap"]);
    }

    #[test]
    fn the_old_ordinal_and_its_separator_are_replaced() {
        let maps = names(&["01_001_line_101_Contour.omap"]);
        let expected = names(&["01_001_line_101_Contour.png"]);
        let plan = plan(&maps, &expected);
        assert_eq!(plan.maps[0].corrected, "000__001_line_101_Contour.omap");
        assert_eq!(plan.expected[0].corrected, "000__001_line_101_Contour.png");
    }

    #[test]
    fn holes_in_the_sequence_are_closed_and_reported() {
        let maps = names(&["000__a.omap", "007__b.omap"]);
        let expected = names(&["000__a.png", "007__b.png"]);
        let plan = plan(&maps, &expected);
        assert_eq!(plan.maps[1].corrected, "001__b.omap");
        assert!(
            plan.maps[1]
                .reasons
                .iter()
                .any(|r| r.kind == "leading number out of sequence"),
            "{:?}",
            plan.maps[1]
        );
    }

    #[test]
    fn spaces_become_underscores() {
        let maps = names(&["000__a map.omap"]);
        let expected = names(&["000__a map.png"]);
        let plan = plan(&maps, &expected);
        assert_eq!(plan.maps[0].corrected, "000__a_map.omap");
        assert!(plan.maps[0]
            .reasons
            .iter()
            .any(|r| r.kind == "spaces in the name"));
    }

    #[test]
    fn a_dot_in_the_name_is_not_mistaken_for_a_suffix() {
        let maps = names(&["12_area_413.1_Orchard.omap"]);
        let expected = names(&["12_area_413.1_Orchard.png"]);
        let plan = plan(&maps, &expected);
        assert_eq!(plan.maps[0].corrected, "000__area_413.1_Orchard.omap");
    }

    #[test]
    fn the_ordinal_grows_with_the_suite() {
        let maps: Vec<String> = (0..1200).map(|i| format!("{i}_map.omap")).collect();
        let expected: Vec<String> = (0..1200).map(|i| format!("{i}_map.png")).collect();
        let plan = plan(&maps, &expected);
        assert_eq!(plan.maps[0].corrected, "0000__map.omap");
        assert_eq!(plan.maps[1199].corrected, "1199__map.omap");
    }

    #[test]
    fn missing_and_orphan_reference_images_are_reported_but_not_invented() {
        let maps = names(&["000__a.omap", "001__b.omap"]);
        let expected = names(&["000__a.png", "999__stray.png"]);
        let plan = plan(&maps, &expected);
        assert_eq!(plan.missing_expected, ["001__b.omap"]);
        assert_eq!(plan.orphan_expected, ["999__stray.png"]);
        assert_eq!(
            plan.corrected_expected("999__stray.png"),
            Some("999__stray.png")
        );
        assert!(!plan.is_valid());
    }

    #[test]
    fn the_summary_counts_kinds_not_lines() {
        // Every map is one off and separated by a single underscore, which is
        // two kinds of problem across 2 maps and their 2 reference images.
        let maps = names(&["1_a.omap", "2_b.omap"]);
        let expected = names(&["1_a.png", "2_b.png"]);
        let plan = plan(&maps, &expected);
        assert_eq!(plan.problems().len(), 4);
        assert_eq!(
            plan.summary(),
            [
                ("leading number of the wrong width", 2),
                ("leading number out of sequence", 2),
                ("renamed with the map it belongs to", 2),
                ("wrong separator after the leading number", 2),
            ]
        );
    }

    #[test]
    fn a_map_without_an_ordinal_sorts_last() {
        let maps = names(&["zebra.omap", "05_a.omap"]);
        let expected = names(&["zebra.png", "05_a.png"]);
        let plan = plan(&maps, &expected);
        assert_eq!(plan.maps[0].corrected, "000__a.omap");
        assert_eq!(plan.maps[1].corrected, "001__zebra.omap");
    }
}
