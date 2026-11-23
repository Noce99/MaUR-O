//! What a network is asked to see in a map: the classes its answer has a
//! channel for, and how each object of a symbol set is labelled with them.
//!
//! The plain answer is one class per symbol — each opaque area of the set a
//! class of the ground, each line a channel of the lines, each point symbol a
//! heatmap — and [`LabelSet::of`] is that. It asks two things of a network a
//! picture cannot always give it, though:
//!
//! * to tell apart symbols which look the same. ISSprOM's bank line of a lake,
//!   border line of a marsh and tag lines of a cliff are all a black line
//!   0.18 mm wide, and a network shown one has no way to say which;
//! * to learn as one thing what is plainly two. A building is a grey fill and
//!   a black outline; a road 2 m wide is a strip of paved area with a black
//!   border either side, which is pixel for pixel what a paved area 2 m wide
//!   with its border line is too. A class per symbol makes the network learn
//!   each of those again for every symbol that draws one, and gives it a
//!   channel for each, which for a set like ISSprOM is a great many channels
//!   of a few pixels each.
//!
//! So a set can come with a **table** beside it, `<set>.labels.json`, which
//! names the classes and says, symbol by symbol, which of them an object of
//! that symbol is labelled with. The maps are still drawn with the set's own
//! symbols, so the pictures are the pictures a real map makes; only what the
//! network is told about them changes. `symbols_sets/almost_ISSprOM.labels.json`
//! is one, and `symbols_sets/make_almost_issprom.py` is where it comes from.
//!
//! # The table
//!
//! ```json
//! {
//!  "areas":  [{"name": "Paved area, light traffic", "symbol": "501.5"}, ...],
//!  "lines":  [{"name": "Black line 0.10 mm", "symbol": "501.1"}, ...],
//!  "points": [{"name": "Slope line", "symbol": "101.1"},
//!             {"name": "Pillar", "symbol": "522.1", "symmetry": 4}, ...],
//!  "symbols": {
//!   "501.7": [{"area": "Paved area, light traffic"}, {"borders": "Black line 0.10 mm"}],
//!   "521":   [{"area": "Building"}, {"outline": "Black line 0.14 mm"}],
//!   "521.3": [],
//!   ...
//!  }
//! }
//! ```
//!
//! A class is a name, and the symbol a network's answer for it is written
//! back with — the one `mauro net image-to-map` draws it in. A point class may
//! say how many turns of it look alike, which is what its angle is folded by
//! (see [`crate::point_truth`]); an area class's comes out of its symbol, as
//! [`crate::symbol_kinds::pattern_symmetry`] works it out.
//!
//! Every symbol of the set but its lettering is listed, by its code, with the
//! parts it is labelled as — each one of
//!
//! * `area`: for an area object, its shape; for a line object, the strip its
//!   main stroke covers, as wide as the symbol draws it;
//! * `outline`: an area object's boundary, as a line;
//! * `line`: a line object's path;
//! * `borders`: a line object's borders, each at the offset the symbol draws
//!   it at, as lines;
//! * `point`: a point object.
//!
//! A symbol with no parts is one which is only ever drawn as part of another
//! — a building's fill, which the standard never draws without its outline —
//! and a generator leaves it out. A see-through area can only be listed that
//! way: there is no class for what is under one yet.
//!
//! Everything is checked as the table is read, against the set it is for: a
//! symbol missing from the table, a class named nowhere, a part an object of
//! that kind cannot have. A table which has drifted from its set is refused
//! there, rather than found out a training run later.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::dataset::{Classes, CLASSES_FILE};
use crate::geometry::shift_coordinates;
use crate::ground_truth::Ground;
use crate::line_truth::LineStroke;
use crate::map::{coord_flag, is_color, Map, Object, ObjectKind, PartRef, Symbol};
use crate::symbol_kinds::{Catalogue, Entry, SymbolKind};
use crate::xml_reader::read_xml_map;

/// What the table of a symbol set is called, after the set's own stem:
/// `almost_ISSprOM.omap` has `almost_ISSprOM.labels.json`.
pub const TABLE_SUFFIX: &str = ".labels.json";

/// One thing an object is labelled with — see the module documentation for
/// what each is of which kind of object. The number is the class, a place in
/// [`LabelSet::areas`], [`LabelSet::lines`] or [`LabelSet::points`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Part {
    /// The area object's shape, or the line object's strip, in an area class.
    Area(usize),
    /// The area object's boundary, in a line class.
    Outline(usize),
    /// The line object's path, in a line class.
    Line(usize),
    /// The line object's borders, in a line class.
    Borders(usize),
    /// The point object, in a point class.
    Point(usize),
}

/// The classes of a network's answer, and how a symbol set's objects are
/// labelled with them.
#[derive(Clone, Debug)]
pub struct LabelSet {
    /// The classes of the ground, a softmax over them: one channel each.
    ///
    /// Each is the [`Entry`] of the symbol its answer is written back with,
    /// under the class's own name, so that a class can be used wherever an
    /// opaque area of a [`Catalogue`] was.
    pub areas: Vec<Entry>,
    /// The classes of the lines layer, a sigmoid each.
    pub lines: Vec<Entry>,
    /// The classes of the points layer, a heatmap each. An entry's
    /// [`Entry::symmetry`] is how many turns of it look alike.
    pub points: Vec<Entry>,
    /// What each symbol of the set is labelled as, by its place in
    /// [`Map::symbols`]; `None` for one which is neither labelled nor drawn.
    parts: Vec<Option<Vec<Part>>>,
    /// Whether an object of each symbol is given an angle of its own.
    turns: Vec<bool>,
    /// The table this came out of, where it came out of one.
    pub table: Option<PathBuf>,
}

impl LabelSet {
    /// One class per symbol: each opaque area of `map`'s set a class of the
    /// ground, each line a line class, each point symbol a point class, in
    /// the order of their [`Catalogue`]. The see-through areas are drawn and
    /// not labelled, as there is no class for them.
    pub fn of(map: &Map) -> LabelSet {
        let catalogue = Catalogue::of(map);
        let mut parts = vec![None; map.symbols.len()];
        for (class, entry) in catalogue.opaque_areas.iter().enumerate() {
            parts[entry.index] = Some(vec![Part::Area(class)]);
        }
        for (class, entry) in catalogue.lines.iter().enumerate() {
            parts[entry.index] = Some(vec![Part::Line(class)]);
        }
        for (class, entry) in catalogue.points.iter().enumerate() {
            parts[entry.index] = Some(vec![Part::Point(class)]);
        }
        // Drawn and not labelled: there is no class for what is under a
        // see-through area, and lettering nothing draws yet in any case.
        for entry in catalogue.transparent_areas.iter().chain(&catalogue.texts) {
            parts[entry.index] = Some(Vec::new());
        }
        LabelSet {
            turns: turns_of(map, &catalogue),
            areas: catalogue.opaque_areas,
            lines: catalogue.lines,
            points: catalogue.points,
            parts,
            table: None,
        }
    }

    /// The labels of the set at `set`, already read into `map`: the table
    /// beside it where there is one (see [`table_beside`]), and one class per
    /// symbol where there is not.
    pub fn beside(map: &Map, set: &Path) -> Result<LabelSet, String> {
        let table = table_beside(set);
        if table.is_file() {
            LabelSet::read(map, &table)
        } else {
            Ok(LabelSet::of(map))
        }
    }

    /// The labels the table at `table` gives `map`'s set.
    pub fn read(map: &Map, table: &Path) -> Result<LabelSet, String> {
        let text = std::fs::read_to_string(table)
            .map_err(|e| format!("cannot read {}: {e}", table.display()))?;
        let mut labels =
            LabelSet::from_table(map, &text).map_err(|e| format!("{}: {e}", table.display()))?;
        labels.table = Some(table.to_path_buf());
        Ok(labels)
    }

    /// The labels a table's text gives `map`'s set — see the module
    /// documentation for what it holds and what is checked.
    pub fn from_table(map: &Map, text: &str) -> Result<LabelSet, String> {
        let table: Table =
            serde_json::from_str(text).map_err(|e| format!("not a table of labels: {e}"))?;
        let catalogue = Catalogue::of(map);
        let kinds: Vec<(SymbolKind, &Entry)> = catalogue
            .by_kind()
            .into_iter()
            .flat_map(|(kind, list)| list.iter().map(move |entry| (kind, entry)))
            .collect();
        let mut by_code: HashMap<&str, (SymbolKind, &Entry)> = HashMap::new();
        for &(kind, entry) in &kinds {
            if by_code.insert(entry.code.trim(), (kind, entry)).is_some() {
                return Err(format!(
                    "the set has two symbols numbered {}, so a table cannot say which it means",
                    entry.code.trim()
                ));
            }
        }

        // The classes, each the entry of the symbol it is written back with.
        let classes = |list: &[Class], want: SymbolKind, layer: &str| {
            let mut entries = Vec::with_capacity(list.len());
            let mut names: HashMap<String, usize> = HashMap::new();
            for (at, class) in list.iter().enumerate() {
                if names.insert(class.name.clone(), at).is_some() {
                    return Err(format!("two {layer} classes are called {}", class.name));
                }
                let &(kind, entry) = by_code.get(class.symbol.trim()).ok_or_else(|| {
                    format!(
                        "the {layer} class {} is written back as symbol {}, which the set does \
                         not have",
                        class.name, class.symbol
                    )
                })?;
                if kind != want {
                    return Err(format!(
                        "the {layer} class {} is written back as symbol {}, which is a {} and \
                         not a {}",
                        class.name,
                        class.symbol,
                        kind.name(),
                        want.name()
                    ));
                }
                let mut entry = entry.clone();
                entry.name = class.name.clone();
                if let Some(symmetry) = class.symmetry {
                    if want != SymbolKind::Point || symmetry == 0 {
                        return Err(format!(
                            "the {layer} class {} says {symmetry} turns of it look alike, which \
                             only a point class can say, and only of one turn or more",
                            class.name
                        ));
                    }
                    entry.symmetry = symmetry;
                }
                entries.push(entry);
            }
            Ok((entries, names))
        };
        let (areas, area_names) = classes(&table.areas, SymbolKind::OpaqueArea, "area")?;
        let (lines, line_names) = classes(&table.lines, SymbolKind::Line, "line")?;
        let (points, point_names) = classes(&table.points, SymbolKind::Point, "point")?;

        let mut parts = vec![None; map.symbols.len()];
        for (code, listed) in &table.symbols {
            let &(kind, entry) = by_code.get(code.trim()).ok_or_else(|| {
                format!("the table labels symbol {code}, which the set does not have")
            })?;
            let mut out = Vec::with_capacity(listed.len());
            for part in listed {
                let [(what, name)] = part.iter().collect::<Vec<_>>()[..] else {
                    return Err(format!(
                        "a part of symbol {code} names {} things, and a part is one",
                        part.len()
                    ));
                };
                let class = |names: &HashMap<String, usize>, layer: &str| {
                    names.get(name.as_str()).copied().ok_or_else(|| {
                        format!("symbol {code} is labelled with {name}, which is no {layer} class")
                    })
                };
                out.push(match what.as_str() {
                    "area" => Part::Area(class(&area_names, "area")?),
                    "outline" => Part::Outline(class(&line_names, "line")?),
                    "line" => Part::Line(class(&line_names, "line")?),
                    "borders" => Part::Borders(class(&line_names, "line")?),
                    "point" => Part::Point(class(&point_names, "point")?),
                    other => {
                        return Err(format!(
                            "symbol {code} has a part of {other}, which is none of area, \
                             outline, line, borders and point"
                        ))
                    }
                });
            }
            check_parts(map, code, kind, entry, &out)?;
            parts[entry.index] = Some(out);
        }
        if let Some(&(_, entry)) = kinds
            .iter()
            .find(|(kind, entry)| *kind != SymbolKind::Text && parts[entry.index].is_none())
        {
            return Err(format!(
                "symbol {} ({}) is not in the table: every symbol of the set but its lettering \
                 is, with the parts it is labelled as, or none for one never drawn on its own",
                entry.code.trim(),
                entry.name
            ));
        }

        Ok(LabelSet {
            turns: turns_of(map, &catalogue),
            areas,
            lines,
            points,
            parts,
            table: None,
        })
    }

    /// What an object of the symbol at `index` of [`Map::symbols`] is
    /// labelled as: nothing for a symbol which is not labelled at all.
    pub fn parts(&self, index: usize) -> &[Part] {
        self.parts
            .get(index)
            .and_then(Option::as_deref)
            .unwrap_or(&[])
    }

    /// Whether a generator may draw the symbol at `index` on its own: every
    /// symbol of a set with no table, and every symbol a table gives parts
    /// to.
    pub fn drawn(&self, index: usize) -> bool {
        match self.parts.get(index) {
            Some(Some(parts)) => !parts.is_empty() || self.table.is_none(),
            _ => false,
        }
    }

    /// Half of the widest outline an object of the symbol at `index` is
    /// labelled with, in mm on the paper: how far its boundary reaches into
    /// the ground beside it. Nought for one labelled with no outline.
    pub fn outline_half_width(&self, map: &Map, index: usize) -> f64 {
        self.parts(index)
            .iter()
            .filter_map(|part| match part {
                Part::Outline(class) => Some(stroke_width(map, self.lines[*class].index) / 2.0),
                _ => None,
            })
            .fold(0.0, f64::max)
    }

    /// The pieces of ground `objects` are labelled with, in the order they
    /// are drawn, which is the order a later one wins in: an area object's
    /// shape, and a line object's strip.
    pub fn grounds(&self, map: &Map, objects: &[Object]) -> Vec<Ground> {
        let mut grounds = Vec::new();
        for object in objects {
            let (ObjectKind::Path(_), Some(index)) = (&object.kind, object.symbol_index) else {
                continue;
            };
            let line = matches!(map.symbols.get(index), Some(Symbol::Line(_)));
            for part in self.parts(index) {
                let &Part::Area(class) = part else {
                    continue;
                };
                grounds.push(if line {
                    Ground {
                        class,
                        // A strip is the ground of a line, and a line draws
                        // no pattern to turn.
                        turn: None,
                        outline: object.coords.clone(),
                        strip: Some(stroke_width(map, index)),
                    }
                } else {
                    Ground {
                        class,
                        // A symbol with no pattern to turn was given no
                        // angle, which is not the same as having been given
                        // nought.
                        turn: self.turns[index].then_some(object.rotation),
                        outline: object.coords.clone(),
                        strip: None,
                    }
                });
            }
        }
        grounds
    }

    /// The strokes `objects` are labelled with in the lines layer: a line
    /// object's path and its borders, an area object's boundary.
    pub fn strokes(&self, map: &Map, objects: &[Object]) -> Vec<LineStroke> {
        let mut strokes = Vec::new();
        for object in objects {
            let (ObjectKind::Path(_), Some(index)) = (&object.kind, object.symbol_index) else {
                continue;
            };
            for part in self.parts(index) {
                match *part {
                    Part::Line(class) => strokes.push(LineStroke {
                        class,
                        width: stroke_width(map, index),
                        path: object.coords.clone(),
                    }),
                    Part::Outline(class) => strokes.push(LineStroke {
                        class,
                        width: stroke_width(map, self.lines[class].index),
                        path: object.coords.clone(),
                    }),
                    Part::Borders(class) => {
                        let Some(Symbol::Line(line)) = map.symbols.get(index) else {
                            continue;
                        };
                        for (border, sign) in [(&line.border, -1.0), (&line.right_border, 1.0)] {
                            if !border.is_visible() || object.coords.len() < 2 {
                                continue;
                            }
                            let last = object.coords.len() - 1;
                            let closed = object.coords[last].flags & coord_flag::CLOSE_POINT != 0;
                            strokes.push(LineStroke {
                                class,
                                width: border.width,
                                path: shift_coordinates(
                                    &object.coords,
                                    0,
                                    last,
                                    closed,
                                    sign * line.line_width / 2.0,
                                    sign * border.shift,
                                    line.join_style,
                                ),
                            });
                        }
                    }
                    Part::Area(_) | Part::Point(_) => {}
                }
            }
        }
        strokes
    }

    /// The point class an object is labelled with, and the angle it was
    /// turned to where its symbol turns: `None` for an object which is not a
    /// point labelled with one.
    pub fn point_of(&self, object: &Object) -> Option<(usize, Option<f64>)> {
        if !matches!(object.kind, ObjectKind::Point) {
            return None;
        }
        let index = object.symbol_index?;
        self.parts(index).iter().find_map(|part| match part {
            &Part::Point(class) => Some((class, self.turns[index].then_some(object.rotation))),
            _ => None,
        })
    }
}

/// The labels of a dataset, or of a run folder trained on one, and the
/// symbol set they are for: out of the `classes.json` there, which names the
/// set and — where its classes are a table's — the table, both beside it.
pub fn of_folder(folder: &Path) -> Result<(LabelSet, Map), String> {
    let notes_file = folder.join(CLASSES_FILE);
    let notes = Classes::read(&notes_file)?;
    let set = folder.join(notes.symbol_set.ok_or_else(|| {
        format!(
            "{} names no symbol set, so there is no telling which symbol a class is",
            notes_file.display()
        )
    })?);
    let (mut map, _) = read_xml_map(&set)
        .map_err(|e| format!("cannot read the symbol set {}: {e}", set.display()))?;
    map.resolve_references();
    let labels = match notes.labels {
        Some(table) => LabelSet::read(&map, &folder.join(table))?,
        None => LabelSet::of(&map),
    };
    Ok((labels, map))
}

/// Where the table of the symbol set at `set` would be: beside it, under its
/// stem and [`TABLE_SUFFIX`].
pub fn table_beside(set: &Path) -> PathBuf {
    let stem = set
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    set.with_file_name(format!("{stem}{TABLE_SUFFIX}"))
}

/// A table as the file holds it.
#[derive(Deserialize)]
struct Table {
    areas: Vec<Class>,
    lines: Vec<Class>,
    points: Vec<Class>,
    symbols: HashMap<String, Vec<HashMap<String, String>>>,
}

/// One class as the table holds it.
#[derive(Deserialize)]
struct Class {
    name: String,
    symbol: String,
    #[serde(default)]
    symmetry: Option<u32>,
}

/// Whether an object of the symbol `entry`, of `kind`, can be labelled as
/// `parts`.
fn check_parts(
    map: &Map,
    code: &str,
    kind: SymbolKind,
    entry: &Entry,
    parts: &[Part],
) -> Result<(), String> {
    if parts.is_empty() {
        return Ok(());
    }
    let count = |f: fn(&Part) -> bool| parts.iter().filter(|p| f(p)).count();
    let areas = count(|p| matches!(p, Part::Area(_)));
    let refuse = |why: &str| Err(format!("symbol {code} ({}) {why}", entry.name));
    match kind {
        SymbolKind::OpaqueArea => {
            if areas != 1 {
                return refuse("is an area, and is labelled as exactly one area class");
            }
            if parts
                .iter()
                .any(|p| !matches!(p, Part::Area(_) | Part::Outline(_)))
            {
                return refuse("is an area, which has a shape and an outline and nothing else");
            }
        }
        SymbolKind::Line => {
            if parts
                .iter()
                .any(|p| matches!(p, Part::Outline(_) | Part::Point(_)))
            {
                return refuse("is a line, which has a strip, a path and borders and nothing else");
            }
            let plain = matches!(map.symbols.get(entry.index), Some(Symbol::Line(_)));
            if !plain
                && parts
                    .iter()
                    .any(|p| matches!(p, Part::Area(_) | Part::Borders(_)))
            {
                return refuse("is made of several lines, whose strip and borders are not one");
            }
            if areas > 1 {
                return refuse("is a line, which covers one strip of ground");
            }
            if let Some(Symbol::Line(line)) = map.symbols.get(entry.index) {
                if parts.iter().any(|p| matches!(p, Part::Borders(_)))
                    && !line.border.is_visible()
                    && !line.right_border.is_visible()
                {
                    return refuse("is labelled with its borders, and draws none");
                }
                if areas == 1 && !(is_color(line.color) && line.line_width > 0.0) {
                    return refuse("is labelled with its strip, and draws no stroke to have one");
                }
            }
        }
        SymbolKind::Point => {
            if parts.len() != 1 || !matches!(parts[0], Part::Point(_)) {
                return refuse("is a point, and is labelled as exactly one point class");
            }
        }
        SymbolKind::TransparentArea => {
            return refuse("is a see-through area, which no class can label yet");
        }
        SymbolKind::Text => return refuse("is lettering, which no class labels"),
    }
    Ok(())
}

/// Whether an object of each symbol of `map` is given an angle of its own,
/// by its place in [`Map::symbols`].
fn turns_of(map: &Map, catalogue: &Catalogue) -> Vec<bool> {
    let mut turns = vec![false; map.symbols.len()];
    for (_, list) in catalogue.by_kind() {
        for entry in list {
            turns[entry.index] = entry.turns;
        }
    }
    turns
}

/// How wide the stroke of the line symbol at `index` is, in mm: its main
/// line's, or for a combined symbol the widest of its parts'. Nought for a
/// symbol which draws no stroke, only point symbols along it — its width is
/// the room those take up, which is not where the line is.
pub(crate) fn stroke_width(map: &Map, index: usize) -> f64 {
    fn of(map: &Map, symbol: &Symbol, depth: usize) -> f64 {
        match symbol {
            Symbol::Line(line) if is_color(line.color) => line.line_width,
            Symbol::Combined(combined) if depth < 16 => combined
                .parts
                .iter()
                .filter_map(|part| match part {
                    PartRef::Shared(at) => map.symbols.get(*at),
                    PartRef::Private(at) => combined.owned_parts.get(*at),
                    PartRef::None => None,
                })
                .map(|part| of(map, part, depth + 1))
                .fold(0.0, f64::max),
            _ => 0.0,
        }
    }
    map.symbols
        .get(index)
        .map(|symbol| of(map, symbol, 0))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Coord;

    const ALMOST: &str = "symbols_sets/almost_ISSprOM.omap";

    fn almost() -> (Map, LabelSet) {
        let (mut map, _) = read_xml_map(Path::new(ALMOST)).expect("the set");
        map.resolve_references();
        let labels = LabelSet::beside(&map, Path::new(ALMOST)).expect("its table");
        (map, labels)
    }

    fn index_of(map: &Map, code: &str) -> usize {
        map.symbols
            .iter()
            .position(|symbol| symbol.code().trim() == code)
            .unwrap_or_else(|| panic!("no symbol {code}"))
    }

    fn object(map: &Map, code: &str, kind: ObjectKind, coords: Vec<Coord>) -> Object {
        let mut object = Object::new(kind);
        object.symbol_index = Some(index_of(map, code));
        object.coords = coords;
        object
    }

    fn line(map: &Map, code: &str) -> Object {
        object(
            map,
            code,
            ObjectKind::Path(Default::default()),
            vec![Coord::new(0.0, 0.0, 0), Coord::new(10.0, 0.0, 0)],
        )
    }

    #[test]
    fn the_almost_issprom_table_fits_its_set() {
        let (map, labels) = almost();
        assert_eq!(
            (labels.areas.len(), labels.lines.len(), labels.points.len()),
            (33, 41, 29)
        );
        assert!(labels.table.is_some());
        // A building's fill is only ever part of a building.
        assert!(!labels.drawn(index_of(&map, "521.3")));
        assert!(labels.drawn(index_of(&map, "521")));
        // A class is written back with its symbol, under its own name.
        let border = labels
            .lines
            .iter()
            .find(|entry| entry.name == "Black line 0.10 mm")
            .expect("the class");
        assert_eq!(border.code, "501.1");
    }

    /// A road of a fixed footprint is a strip of paved area as wide as the
    /// road, and its two borders either side, just outside it.
    #[test]
    fn a_road_is_a_strip_of_paved_area_and_two_borders() {
        let (map, labels) = almost();
        let road = line(&map, "501.7");
        let grounds = labels.grounds(&map, std::slice::from_ref(&road));
        assert_eq!(grounds.len(), 1);
        assert_eq!(
            labels.areas[grounds[0].class].name,
            "Paved area, light traffic"
        );
        assert!((grounds[0].strip.expect("a strip") - 0.5).abs() < 1e-9);

        let strokes = labels.strokes(&map, std::slice::from_ref(&road));
        assert_eq!(strokes.len(), 2);
        let mut sides: Vec<f64> = strokes
            .iter()
            .map(|stroke| {
                assert_eq!(labels.lines[stroke.class].name, "Black line 0.10 mm");
                stroke.path[0].y
            })
            .collect();
        sides.sort_by(f64::total_cmp);
        // Half the road and half a border out, either way.
        assert!(
            (sides[0] + 0.3).abs() < 1e-6 && (sides[1] - 0.3).abs() < 1e-6,
            "{sides:?}"
        );
    }

    /// A building is its grey fill and a black outline round it.
    #[test]
    fn a_building_is_its_fill_and_its_outline() {
        let (map, labels) = almost();
        let square = vec![
            Coord::new(0.0, 0.0, 0),
            Coord::new(5.0, 0.0, 0),
            Coord::new(5.0, 5.0, 0),
            Coord::new(0.0, 5.0, 0),
            Coord::new(0.0, 0.0, coord_flag::CLOSE_POINT),
        ];
        let building = object(&map, "521", ObjectKind::Path(Default::default()), square);
        let grounds = labels.grounds(&map, std::slice::from_ref(&building));
        assert_eq!(labels.areas[grounds[0].class].name, "Building");
        assert!(grounds[0].strip.is_none());
        let strokes = labels.strokes(&map, std::slice::from_ref(&building));
        assert_eq!(strokes.len(), 1);
        assert_eq!(labels.lines[strokes[0].class].name, "Black line 0.14 mm");
        assert!(labels.outline_half_width(&map, index_of(&map, "521")) > 0.0);
    }

    /// Two symbols which look the same are the same class.
    #[test]
    fn the_bank_of_a_lake_and_the_border_of_a_marsh_are_one_line() {
        let (map, labels) = almost();
        let class = |code| labels.strokes(&map, &[line(&map, code)])[0].class;
        assert_eq!(class("301.6"), class("307.2"));
        assert_ne!(class("301.6"), class("501.1"));
    }

    #[test]
    fn a_turned_point_keeps_its_angle_and_a_still_one_has_none() {
        let (map, labels) = almost();
        let point = |code: &str| {
            let mut point = object(&map, code, ObjectKind::Point, vec![Coord::new(1.0, 1.0, 0)]);
            point.rotation = 0.5;
            labels.point_of(&point).expect("a point class")
        };
        assert_eq!(point("312").1, Some(0.5));
        assert_eq!(point("204").1, None);
        // The three slope lines are one class.
        assert_eq!(point("101.1").0, point("103.1").0);
    }

    /// A set with no table is a class per symbol, in the catalogue's order.
    #[test]
    fn a_set_without_a_table_is_a_class_per_symbol() {
        let set = Path::new("symbols_sets/simple_ISSprOM_points.omap");
        let (mut map, _) = read_xml_map(set).expect("the set");
        map.resolve_references();
        let labels = LabelSet::beside(&map, set).expect("no table to read");
        let catalogue = Catalogue::of(&map);
        assert!(labels.table.is_none());
        let codes = |list: &[Entry]| list.iter().map(|e| e.code.clone()).collect::<Vec<_>>();
        assert_eq!(codes(&labels.areas), codes(&catalogue.opaque_areas));
        assert_eq!(codes(&labels.lines), codes(&catalogue.lines));
        assert_eq!(codes(&labels.points), codes(&catalogue.points));
    }

    /// A table which has drifted from its set is refused, with what drifted.
    #[test]
    fn a_table_which_does_not_fit_its_set_is_refused() {
        let (map, _) = almost();
        let text = std::fs::read_to_string(table_beside(Path::new(ALMOST))).expect("the table");
        let mut table: serde_json::Value = serde_json::from_str(&text).expect("JSON");

        let mut missing = table.clone();
        missing["symbols"].as_object_mut().unwrap().remove("515");
        let error = LabelSet::from_table(&map, &missing.to_string()).unwrap_err();
        assert!(error.contains("515"), "{error}");

        let mut unknown = table.clone();
        unknown["symbols"]["515"] = serde_json::json!([{"line": "Hedge"}]);
        let error = LabelSet::from_table(&map, &unknown.to_string()).unwrap_err();
        assert!(error.contains("Hedge"), "{error}");

        // A point is not labelled as a line.
        table["symbols"]["204"] = serde_json::json!([{"line": "Uncrossable wall"}]);
        let error = LabelSet::from_table(&map, &table.to_string()).unwrap_err();
        assert!(error.contains("204"), "{error}");
    }
}
